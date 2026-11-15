//! Composition-ready adapters for Managed installation acquisition, managed Wine
//! execution, and verified PD2 client synchronization.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    os::fd::AsRawFd,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

#[cfg(test)]
use std::fs::OpenOptions;

use rustix::fs::{
    FileType, MemfdFlags, Mode, OFlags, SealFlags, fcntl_add_seals, memfd_create, open,
};
use serde::{Deserialize, Serialize};
use tar::{Archive, EntryType};
use xz2::read::XzDecoder;
use zed_reqwest::{
    blocking::Client,
    header::{ACCEPT_ENCODING, CONTENT_DISPOSITION, CONTENT_RANGE, RANGE, RETRY_AFTER},
    redirect,
};

use super::*;
use crate::runtime::{
    CUSTOM_RUNTIME_DIRECTORY, CUSTOM_RUNTIME_RECEIPT, InstalledRuntime, ManagedRuntime,
};
use crate::sources::{CustomSourceReceipt, DownloadArtifact};
use crate::{
    host_execution::{HostExecution, ProcessRequest, SupervisedProcess},
    network::{
        ContentRange, Pd2Source, RetryPolicy, SystemPd2Source, parse_content_range,
        parse_retry_after,
    },
    operation::{EnvironmentPart, EnvironmentProgress, ProgressUnit},
    safe_fs::Directory,
    transaction::UpdateLayout,
    update::{GameActivity, ProtectedInventoryEvidence, VerifiedUpdater},
};

mod direct;

const RUNTIME_RECEIPT: &str = crate::runtime::BUILTIN_RUNTIME_RECEIPT;
const RUNTIME_MATERIALIZATION_STATE: &str = ".pd2launcher-runtime-materialization.sha512";
const GECKO_RECEIPT: &str = ".pd2launcher-gecko.sha256";
/// Prefix-scoped receipt for a Gecko MSI installed from a custom source.
pub(crate) const CUSTOM_GECKO_RECEIPT: &str = ".pd2launcher-gecko-custom.json";
/// Prefix-scoped receipts for Wine Mono and the .NET Desktop Runtime.
const MONO_RECEIPT: &str = ".pd2launcher-mono.sha256";
pub(crate) const CUSTOM_MONO_RECEIPT: &str = ".pd2launcher-mono-custom.json";
const DOTNET_DESKTOP_RECEIPT: &str = ".pd2launcher-dotnet-desktop.sha256";
pub(crate) const CUSTOM_DOTNET_DESKTOP_RECEIPT: &str = ".pd2launcher-dotnet-desktop-custom.json";
/// Runner-scoped state naming the custom archive whose materialization began.
const CUSTOM_RUNTIME_MATERIALIZATION_STATE: &str = ".pd2launcher-custom-materialization.sha256";
/// Launcher-owned extraction directory for a custom Wine archive.
const CUSTOM_RUNTIME_STAGING: &str = "custom-wine.staging";
const GECKO_SHA256: &str = "26cecc47706b091908f7f814bddb074c61beb8063318e9efc5a7f789857793d6";
const GECKO_XUL_SHA256: &str = "db2542df83e44540f44ef219e45fe463cd2621911cff5e1bd6ac453cabd600b0";
const MAX_ARCHIVE_ENTRIES: usize = 131_072;
const MAX_EXTRACTED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_INVENTORY_ENTRIES: usize = 16_384;
const PROCESS_OUTPUT_LIMIT: usize = 16 * 1024;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);
const RUNTIME_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
/// The .NET installer's Windows exit code 3010 ("restart required") as the
/// Unix exit status Wine reports for it (the low eight bits).
const DOTNET_RESTART_REQUIRED_STATUS: i32 = 3010 & 0xff;
const CHILD_PRELOADER_FD: i32 = 230;
const CHILD_WINE_PROOF_FD: i32 = 231;
const CHILD_WINESERVER_FD: i32 = 232;
const CHILD_PROGRAM_FD: i32 = 233;
const CHILD_MUTATION_LEASE_FD: i32 = 234;
const CHILD_MANAGED_ROOT_FD: i32 = 235;
const CHILD_RUNTIME_BIN_FD: i32 = 236;
const CHILD_VENDOR_UI_HELPER_FD: i32 = 237;
// `winedbg.exe=d`: a program that crashes ends with its exit code instead
// of waiting forever on Wine's crash debugger.
const WINE_DLL_OVERRIDES: &str = "winemenubuilder.exe=;dsound=;winedbg.exe=d";
const PREFIX_INITIALIZATION_DLL_OVERRIDES: &str =
    "winemenubuilder.exe=;dsound=;mscoree=;mshtml=;winedbg.exe=d";
/// FEX settings for every x86 program on ARM64 (installers, the official
/// launcher and the game): strict x86 memory ordering, full x87 precision,
/// one block at a time.
/// Slower, but old 32-bit programs (PD2's winmpq.exe, Diablo II's own x87
/// code) crash without them, and the game is light enough not to need the
/// speed. FEX reads them from the environment, both run as a Linux translator
/// and loaded into an ARM64 Wine as a Windows DLL.
const FEX_STRICT_ENVIRONMENT: [(&str, &str); 6] = [
    ("FEX_TSOENABLED", "1"),
    ("FEX_VECTORTSOENABLED", "1"),
    ("FEX_MEMCPYSETTSOENABLED", "1"),
    ("FEX_HALFBARRIERTSOENABLED", "1"),
    ("FEX_X87REDUCEDPRECISION", "0"),
    ("FEX_MULTIBLOCK", "0"),
];

/// The strict FEX settings on ARM64, where x86 programs run through FEX
/// (never on the Android host, whose x86 programs run in Box64).
pub(crate) fn fex_strict_settings() -> Vec<crate::wine_environment::Setting> {
    fex_strict_settings_on(crate::android_host())
}

/// [`fex_strict_settings`] on the Android host (`android`) or the desktop.
fn fex_strict_settings_on(android: bool) -> Vec<crate::wine_environment::Setting> {
    use crate::wine_environment::{Kind, Setting};
    if cfg!(target_arch = "aarch64") && !android {
        FEX_STRICT_ENVIRONMENT
            .iter()
            .map(|(name, value)| Setting::new(*name, *value, Kind::Preference))
            .collect()
    } else {
        Vec::new()
    }
}

/// What every Wine process of this installation gets from the Launcher:
/// its prefix, wineserver, debug channels, cache, home, path, and locale
/// (see `wine_environment`).
pub(crate) fn wine_settings(home: impl Into<OsString>) -> Vec<crate::wine_environment::Setting> {
    use crate::wine_environment::{Kind, Setting};
    vec![
        Setting::new(
            "WINEPREFIX",
            format!("/proc/self/fd/{CHILD_MANAGED_ROOT_FD}/prefix"),
            Kind::Required,
        ),
        // Wine runs the wineserver beside the program it was started as;
        // started through /proc/self/fd/N it would look there.
        Setting::new(
            "WINESERVER",
            format!("/proc/self/fd/{CHILD_RUNTIME_BIN_FD}/wineserver"),
            Kind::Required,
        ),
        Setting::new(
            "WINEDEBUG",
            crate::dev_build::SETUP_WINEDEBUG,
            Kind::Preference,
        ),
        Setting::new("HOME", home, Kind::Default),
        Setting::new("LANG", "C.UTF-8", Kind::Default),
        Setting::new("PATH", "/usr/bin:/usr/sbin", Kind::Default),
        // The installation's own cache: Wine never uses ~/.cache/wine.
        Setting::new("XDG_CACHE_HOME", wine_cache_home(), Kind::Required),
    ]
}
/// Upper bound on one official PD2 launcher session, including a game the
/// user starts from it.
const OFFICIAL_LAUNCHER_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
/// The official PD2 launcher, beside the client it manages.
#[cfg(test)]
const OFFICIAL_LAUNCHER: &str = "PD2Launcher.exe";
const CACHE_RECEIPT_SCHEMA: u32 = 2;
/// Idle bound of one artifact request: the blocking client applies it to
/// connecting plus response headers and, separately, to every body read. It
/// never limits a transfer that keeps delivering bytes.
const NETWORK_IDLE_TIMEOUT: Duration = Duration::from_secs(15);
/// Upper bound on requests for one artifact. Attempts that delivered new
/// bytes renew the retry budget, so a large download over a flaky connection
/// resumes with `Range` instead of failing after a fixed total time; this
/// still stops a server that trickles a few bytes per connection forever.
const MAX_PROGRESSING_ATTEMPTS: usize = 64;
/// How long Blizzard's downloader may go without new data in its download
/// folder before it is stopped and the stage fails. A lost connection leaves
/// it at "Lost Internet Connection!" with no dialog its helper could answer,
/// so it would otherwise wait for the whole [`INSTALL_TIMEOUT`]. Short on
/// purpose: Blizzard's downloader is expected to work, so a running download
/// that gets nothing for a minute is taken as a dead connection. A working
/// download that pauses longer (about three minutes was seen once) fails the
/// same way, and Try again resumes it from the parts it kept.
const DOWNLOAD_STALL_LIMIT: Duration = Duration::from_secs(60);
/// The limit before the first change once the downloader runs, when the
/// folder already holds parts from an earlier run ([`GrowthClock`]): the
/// downloader starts under Wine, fetches its description and may check the
/// parts it kept before it writes, which can take longer than a minute.
const DOWNLOAD_RESUME_LIMIT: Duration = Duration::from_secs(5 * 60);
/// The limit while the download folder holds no file and has not changed:
/// the downloader has not started its download, and its helper reports one
/// that showed no window after ten minutes, so this waits past that. A
/// downloader that runs creates its (empty) parts within seconds, also with
/// no connection, and the one-minute limit counts from there.
const DOWNLOAD_EMPTY_LIMIT: Duration = Duration::from_secs(11 * 60);
/// Data received that counts as the downloader working although its folder
/// has not changed (a slow connection filling a large piece, say), from the
/// count the host gives in `PD2_RECEIVED_BYTES_FILE` (Android: the app's
/// received bytes, which Blizzard's downloader under Wine shares): about
/// 1 KB/s over the minute. A lost connection's retries (lookups, a reset, a
/// failed TLS exchange of a few KB) stay below it; any download that is
/// worth waiting for passes it within seconds. The engine log says how much
/// came in when the downloader is stopped, to check it in the field.
const DOWNLOAD_ACTIVITY_BYTES: u64 = 64 * 1024;
/// Where the host writes its received-byte count ([`DOWNLOAD_ACTIVITY_BYTES`]),
/// one decimal number; without it only the folder counts (the desktop).
const RECEIVED_BYTES_FILE: &str = "PD2_RECEIVED_BYTES_FILE";
/// Redirect hops a custom (user-supplied) source may take.
const CUSTOM_REDIRECT_LIMIT: usize = 10;
const WINDOWS_GAME_DIRECTORY: &str = r"C:\Program Files (x86)\Diablo II";
const PAYLOAD_INSTALLER: &str = "Installer.exe";
const PAYLOAD_TOME: &str = "Installer Tome.mpq";
const BASE_PAYLOAD_SOURCE: &str = "D2-1.14b-Installer-enUS";
const EXPANSION_PAYLOAD_SOURCE: &str = "D2LOD-1.14b-Installer-enUS";
const BASE_PAYLOAD_CACHE: &str = "d2-payload";
const EXPANSION_PAYLOAD_CACHE: &str = "lod-payload";
const BASE_INSTALLER_SIZE: u64 = 2_664_936;
const BASE_INSTALLER_SHA256: &str =
    "f845e8bc0ab622989fd25a81b24161ec7abbd1a6cc07e6d771df3962859f5f68";
const BASE_TOME_SIZE: u64 = 1_551_865_181;
const BASE_TOME_SHA256: &str = "1a20e8e53a0c6b458c6ad0be37e873f127ed819c006505c75d181e9f0a9a1378";
const EXPANSION_INSTALLER_SIZE: u64 = 2_671_080;
const EXPANSION_INSTALLER_SHA256: &str =
    "a825d97018f2638d1ef067f3fc199cd3d613fe31748ab5494c61fe3250e7bbe5";
const EXPANSION_TOME_SIZE: u64 = 490_399_607;
const EXPANSION_TOME_SHA256: &str =
    "c286c74eba99c7905d61178d62527977df710a64b49bf557ea02c2fef90b3071";
/// Sealed vendor dialog helper; source in `tools/vendor-ui-helper.c`,
/// rebuilt by `tools/build-sealed-helper.sh`.
const VENDOR_UI_HELPER_SIZE: usize = 4_096;
const VENDOR_UI_HELPER_SHA256: &str =
    "ea405393bfe53f0c90a6ac264c02b53991515fb4ac2158d85886189e550998af";
const VENDOR_UI_HELPER_XZ_HEX: &str = include_str!("vendor_ui_helper.hex");
/// Sealed Blizzard Downloader helper; source in
/// `tools/downloader-ui-helper.c`, rebuilt by `tools/build-sealed-helper.sh`.
const DOWNLOADER_UI_HELPER_SIZE: usize = 4_096;
const DOWNLOADER_UI_HELPER_SHA256: &str =
    "85416beb66fe49804766e5a2d007f2e822cb160ee8122e2de0a7d4fa524eb46e";
const DOWNLOADER_UI_HELPER_XZ_HEX: &str = include_str!("downloader_ui_helper.hex");
/// Sealed OpenGL renderer probe the game supervisor runs in the prefix;
/// source in `tools/gl-probe-helper.c`, rebuilt by
/// `tools/build-sealed-helper.sh`.
const GL_PROBE_HELPER_SIZE: usize = 3_072;
const GL_PROBE_HELPER_SHA256: &str =
    "1b9c37bcf850b171f356c9e87995415c9e256a606627bc49fce2c58c38733bf6";
const GL_PROBE_HELPER_XZ_HEX: &str = include_str!("gl_probe_helper.hex");
/// Sealed XInput bridge the Android game loads as its `xinput1_4.dll`;
/// source in `tools/xinput-bridge.c`, rebuilt by `tools/build-sealed-helper.sh`.
pub(crate) const XINPUT_BRIDGE_SIZE: usize = 4_096;
pub(crate) const XINPUT_BRIDGE_SHA256: &str =
    "b78963150d55de280266418095e79ebeb4d754571f49076c77f8924b34ee70c7";
const XINPUT_BRIDGE_XZ_HEX: &str = include_str!("xinput_bridge.hex");
/// Where the 32-bit game loads `xinput1_4.dll` from, in the managed root.
pub(crate) const XINPUT_BRIDGE_PATH: &str = "prefix/drive_c/windows/syswow64/xinput1_4.dll";
/// Where a downloader wrapper saves the complete payload: a Launcher-owned
/// folder in the prefix's `C:` drive (`--saveas`), removed once the payload
/// it holds has installed.
const WRAPPER_DOWNLOADS: &str = "prefix/drive_c/pd2launcher-downloads";
const WINDOWS_WRAPPER_DOWNLOADS: &str = r"C:\pd2launcher-downloads";

/// The managed Wine runtime of this compile target's row.
fn managed_runtime() -> &'static ManagedRuntime {
    crate::runtime::current()
}

/// Arguments that precede the Windows program. Wine's preloader resolves its
/// loader argument by basename next to the preloader, so a path ending in a
/// bare descriptor number would be rewritten to a nonexistent `bin/<fd>`.
/// Keep the verified Wine file open as proof while naming `wine` beneath the
/// inherited, retained runtime-bin directory. A runtime executed through
/// `wine` itself takes no loader argument.
fn loader_arguments(runtime: InstalledRuntime) -> Vec<OsString> {
    if runtime.loader_takes_wine_path() {
        vec![OsString::from(format!(
            "/proc/self/fd/{CHILD_RUNTIME_BIN_FD}/wine"
        ))]
    } else {
        Vec::new()
    }
}
#[cfg(test)]
fn runtime_loader_arguments() -> Vec<OsString> {
    loader_arguments(InstalledRuntime::BuiltIn(managed_runtime()))
}

const DIRECTX_DIRECTORY: &str = "DirectX";
const DIRECTX_FILES: &[PayloadFileIdentity] = &[
    PayloadFileIdentity::new(
        "BDA.cab",
        703_080,
        "0f3e9859711233a80f4065ee14402d09b893e730c1a4834460af975e7b97e77b",
    ),
    PayloadFileIdentity::new(
        "BDANT.cab",
        1_156_363,
        "7262a0b2963f1240b17d8c7644c09e473f9706022ff3cd2cdab156f29f15b5a4",
    ),
    PayloadFileIdentity::new(
        "BDAXP.cab",
        976_020,
        "fd472360f4d706a838dd934337473ef6aa2f3dd6aaa669456b3aef6d91756a74",
    ),
    PayloadFileIdentity::new(
        "DSETUP.dll",
        62_976,
        "ad672597c675a160dbc9b5321c4b1720f05e11af22fb8723ab93d0510bab4e01",
    ),
    PayloadFileIdentity::new(
        "DirectX.cab",
        15_493_481,
        "0e8c0ea0a9bc50012fd9286d8941736f8a99d43af10413f9062db7773199efc2",
    ),
    PayloadFileIdentity::new(
        "ManagedDX.CAB",
        3_432_656,
        "41174c8582039a6c9484634292df28c55ce48c784bb3d9cef16d65ec81fc8a1e",
    ),
    PayloadFileIdentity::new(
        "dsetup32.dll",
        2_242_560,
        "79ba6d7ef7f8ffeb7cfec21b14003a121314b831e3522c24bb0a581406cc6c9a",
    ),
    PayloadFileIdentity::new(
        "dxnt.cab",
        13_265_040,
        "1cd52d218757a6108108ed7daa1f96aff4ebcc738888979f74126b983834d579",
    ),
    PayloadFileIdentity::new(
        "dxsetup.exe",
        472_576,
        "37a000d745c65286f029acdc126d355fe8368dee22a38d05be9ece0ffe02f286",
    ),
];

struct ArtifactHttpResponse {
    final_url: String,
    status: u16,
    declared_length: Option<u64>,
    content_range: Option<ContentRange>,
    suggested_filename: Option<String>,
    retry_after: Option<Duration>,
    body: Box<dyn Read + Send>,
}

trait ArtifactHttp: Send + Sync {
    fn get(
        &self,
        spec: &ArtifactSpec,
        range_start: Option<u64>,
        control: &OperationControl,
    ) -> Result<ArtifactHttpResponse, AdapterFailure>;
}

#[derive(Clone, Copy, Debug, Default)]
struct SystemArtifactHttp;

impl ArtifactHttp for SystemArtifactHttp {
    fn get(
        &self,
        spec: &ArtifactSpec,
        range_start: Option<u64>,
        control: &OperationControl,
    ) -> Result<ArtifactHttpResponse, AdapterFailure> {
        if control.is_cancelled() || !approved_request_transport(spec) {
            return Err(AdapterFailure::here());
        }
        let policy_spec = spec.clone();
        let policy = redirect::Policy::custom(move |attempt| {
            if approved_redirect(
                &policy_spec,
                attempt.url().as_str(),
                attempt.previous().len(),
            ) {
                attempt.follow()
            } else {
                attempt.error("artifact redirect left approved boundary")
            }
        });
        let client = Client::builder()
            .user_agent(concat!(
                "pd2-launcher/",
                env!("CARGO_PKG_VERSION"),
                " linux"
            ))
            .connect_timeout(Duration::from_secs(10))
            // An idle bound, not a total one: a stalled connect, header, or
            // body read fails the attempt, and acquisition keeps its partial
            // and resumes with Range.
            .timeout(NETWORK_IDLE_TIMEOUT)
            .redirect(policy)
            .build()
            .map_err(|_| AdapterFailure::here())?;
        let mut request = client
            .get(spec.request_url())
            .header(ACCEPT_ENCODING, "identity");
        if let Some(start) = range_start {
            request = request.header(RANGE, format!("bytes={start}-"));
        }
        let target = format!("{:?}", spec.kind());
        let response = request.send().map_err(|error| {
            if !control.is_cancelled() {
                crate::activity::record_download(
                    &target,
                    spec.request_url(),
                    None,
                    crate::activity::transport_error_kind(&error),
                );
            }
            AdapterFailure
        })?;
        if control.is_cancelled() {
            return Err(AdapterFailure::here());
        }
        let status = response.status().as_u16();
        if status != 200 && status != 206 {
            crate::activity::record_download(
                &target,
                spec.request_url(),
                Some(status),
                if retryable_status(status) {
                    "retryable-status"
                } else {
                    "rejected-status"
                },
            );
        }
        if status != 200 && status != 206 && !retryable_status(status) {
            return Err(AdapterFailure::here());
        }
        let final_url = response.url().to_string();
        if !spec.approves_final_url(&final_url) {
            crate::activity::record_download(
                &target,
                &final_url,
                Some(status),
                "final-url-not-approved",
            );
            return Err(AdapterFailure::here());
        }
        let declared_length = response.content_length();
        let content_range = if status == 200 || status == 206 {
            response
                .headers()
                .get(CONTENT_RANGE)
                .map(|value| {
                    value
                        .to_str()
                        .map_err(|_| AdapterFailure::here())
                        .and_then(|value| {
                            parse_content_range(value).map_err(|_| AdapterFailure::here())
                        })
                })
                .transpose()?
        } else {
            None
        };
        if (status == 200 || status == 206)
            && declared_length.is_some_and(|length| length > spec.maximum_bytes())
        {
            return Err(AdapterFailure::here());
        }
        let retry_after = parse_retry_after(response.headers().get(RETRY_AFTER));
        let suggested_filename = response
            .headers()
            .get(CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_attachment_filename);
        Ok(ArtifactHttpResponse {
            final_url,
            status,
            declared_length,
            content_range,
            suggested_filename,
            retry_after,
            body: Box::new(response),
        })
    }
}

/// Built-in sources use HTTPS only. A user-supplied custom source may use
/// plain HTTP as well; its content is unverified by the user's explicit
/// choice. The one built-in exception does not come here: the direct
/// download's pieces ([`crate::blizzard`]) come over plain HTTP from the one
/// pinned piece host, which presents no certificate for its name; each piece
/// must match its SHA-1 in a torrent whose infohash is pinned, and the
/// finished files their SHA-256 pins.
fn approved_request_transport(spec: &ArtifactSpec) -> bool {
    spec.request_url().starts_with("https://")
        || (spec.is_custom()
            && zed_reqwest::Url::parse(spec.request_url())
                .is_ok_and(|url| url.scheme() == "http" && url.host_str().is_some()))
}

/// `previous_hops` counts the URLs requested so far, the first request
/// included: a limit of 2 allows one redirect.
fn approved_redirect(spec: &ArtifactSpec, destination: &str, previous_hops: usize) -> bool {
    let hop_limit = match spec.redirects {
        RedirectPolicy::None => return false,
        RedirectPolicy::GithubReleaseAsset | RedirectPolicy::LaunchpadLibrarian => 2,
        RedirectPolicy::AnyHttp => CUSTOM_REDIRECT_LIMIT,
    };
    previous_hops < hop_limit && spec.approves_final_url(destination)
}

fn retryable_status(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

fn parse_attachment_filename(value: &str) -> Option<String> {
    value.split(';').skip(1).find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        if !name.eq_ignore_ascii_case("filename") {
            return None;
        }
        let value = value.trim().trim_matches('"');
        (!value.is_empty() && !value.contains(['/', '\\']) && !value.chars().any(char::is_control))
            .then(|| value.to_owned())
    })
}

/// Exact-origin, bounded, verified-cache artifact acquisition.
pub(crate) struct ProductionArtifactSource {
    http: Box<dyn ArtifactHttp>,
    retry_policy: RetryPolicy,
    installer_media: Option<InstallerMedia>,
    /// Only verified downloads already in the cache are returned; nothing
    /// is downloaded or imported (the install steps after Download).
    cache_only: bool,
    /// In a cache-only job, Blizzard's wrappers are still served from the
    /// verified cache and downloaded again when expired or missing: the
    /// Android host, whose downloaders run in Install.
    wrapper_refetch: bool,
}

/// User-selected folder containing the two official legacy Blizzard
/// downloader executables. The selection is session-only; each file is
/// reopened, validated, and imported into Launcher-owned storage before use.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstallerMedia {
    directory: PathBuf,
    layout: InstallerMediaLayout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstallerMediaLayout {
    LegacyWrappers,
    CompletePayloads,
}

#[derive(Clone, Copy)]
struct CompletePayloadIdentity {
    source_directory: &'static str,
    cache_directory: &'static str,
    installer_size: u64,
    installer_sha256: &'static str,
    tome_size: u64,
    tome_sha256: &'static str,
    directx_files: &'static [PayloadFileIdentity],
}

#[derive(Clone, Copy)]
struct PayloadFileIdentity {
    name: &'static str,
    size: u64,
    sha256: &'static str,
}

impl PayloadFileIdentity {
    const fn new(name: &'static str, size: u64, sha256: &'static str) -> Self {
        Self { name, size, sha256 }
    }
}

impl InstallerMedia {
    pub(crate) fn validate(directory: PathBuf) -> Result<Self, AdapterFailure> {
        if !directory.is_absolute() {
            return Err(AdapterFailure::here());
        }
        let opened = Directory::open(&directory).map_err(|_| AdapterFailure::here())?;
        let catalog = ArtifactCatalog::pinned();
        let wrappers_valid = [
            ArtifactKind::BaseGameWrapper,
            ArtifactKind::ExpansionWrapper,
        ]
        .into_iter()
        .all(|kind| {
            let spec = catalog.spec(kind);
            spec.expected_filename.is_some_and(|filename| {
                opened
                    .open_file(Path::new(filename))
                    .ok()
                    .is_some_and(|mut file| validate_selected_installer(spec, &mut file).is_ok())
            })
        });
        if wrappers_valid {
            return Ok(Self {
                directory,
                layout: InstallerMediaLayout::LegacyWrappers,
            });
        }
        for kind in [
            ArtifactKind::BaseGameWrapper,
            ArtifactKind::ExpansionWrapper,
        ] {
            let identity = complete_payload_identity(kind).ok_or_else(|| AdapterFailure::here())?;
            let payload = opened
                .open_directory(Path::new(identity.source_directory))
                .map_err(|_| AdapterFailure::here())?;
            validate_complete_payload_directory(&payload, identity, true)?;
        }
        Ok(Self {
            directory,
            layout: InstallerMediaLayout::CompletePayloads,
        })
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    fn layout(&self) -> InstallerMediaLayout {
        self.layout
    }
}

fn complete_payload_identity(kind: ArtifactKind) -> Option<CompletePayloadIdentity> {
    match kind {
        ArtifactKind::BaseGameWrapper => Some(CompletePayloadIdentity {
            source_directory: BASE_PAYLOAD_SOURCE,
            cache_directory: BASE_PAYLOAD_CACHE,
            installer_size: BASE_INSTALLER_SIZE,
            installer_sha256: BASE_INSTALLER_SHA256,
            tome_size: BASE_TOME_SIZE,
            tome_sha256: BASE_TOME_SHA256,
            directx_files: DIRECTX_FILES,
        }),
        ArtifactKind::ExpansionWrapper => Some(CompletePayloadIdentity {
            source_directory: EXPANSION_PAYLOAD_SOURCE,
            cache_directory: EXPANSION_PAYLOAD_CACHE,
            installer_size: EXPANSION_INSTALLER_SIZE,
            installer_sha256: EXPANSION_INSTALLER_SHA256,
            tome_size: EXPANSION_TOME_SIZE,
            tome_sha256: EXPANSION_TOME_SHA256,
            directx_files: DIRECTX_FILES,
        }),
        _ => None,
    }
}

impl ProductionArtifactSource {
    pub(crate) fn new() -> Self {
        Self {
            http: Box::new(SystemArtifactHttp),
            retry_policy: RetryPolicy::standard(),
            installer_media: None,
            cache_only: false,
            wrapper_refetch: false,
        }
    }

    pub(crate) fn with_installer_media(installer_media: InstallerMedia) -> Self {
        Self {
            http: Box::new(SystemArtifactHttp),
            retry_policy: RetryPolicy::standard(),
            installer_media: Some(installer_media),
            cache_only: false,
            wrapper_refetch: false,
        }
    }

    /// Serve only verified downloads already in the cache.
    pub(crate) fn cache_only(mut self) -> Self {
        self.cache_only = true;
        self
    }

    /// Blizzard's wrappers stay fetchable in a cache-only job: verified
    /// ones are reused, expired or missing ones downloaded again.
    pub(crate) fn with_wrapper_refetch(mut self) -> Self {
        self.wrapper_refetch = true;
        self
    }

    #[cfg(test)]
    fn with_http(http: impl ArtifactHttp + 'static) -> Self {
        Self {
            http: Box::new(http),
            retry_policy: RetryPolicy::immediate_for_tests(),
            installer_media: None,
            cache_only: false,
            wrapper_refetch: false,
        }
    }
}

impl Default for ProductionArtifactSource {
    fn default() -> Self {
        Self::new()
    }
}

impl ArtifactSource for ProductionArtifactSource {
    fn acquire_at(
        &self,
        spec: &ArtifactSpec,
        cache: &Path,
        cache_directory: &Directory,
        control: &OperationControl,
    ) -> Result<AcquiredArtifact, AdapterFailure> {
        reject_artifact_cancelled(control)?;
        validate_artifact_inventory(cache_directory).map_err(|_| AdapterFailure::here())?;
        remove_empty_partials(cache_directory)?;
        // A product's complete installer already in the cache (from a
        // folder, or fetched by Blizzard's downloader in the Download step)
        // is pinned by its identity, so it serves every installer choice.
        if volatile_wrapper(spec.kind)
            && let Some(cached) = cached_complete_payload(spec, cache, cache_directory)?
        {
            return Ok(cached);
        }
        let wrapper_fetch =
            self.wrapper_refetch && volatile_wrapper(spec.kind) && self.installer_media.is_none();
        if self.cache_only && volatile_wrapper(spec.kind) && !wrapper_fetch {
            return Err(AdapterFailure::here());
        }
        if volatile_wrapper(spec.kind)
            && let Some(installer_media) = &self.installer_media
            && installer_media.layout() == InstallerMediaLayout::CompletePayloads
        {
            return import_selected_complete_payload(
                installer_media,
                spec,
                cache,
                cache_directory,
                control,
            );
        }
        let final_name = OsStr::new(cache_name(spec));
        let final_path = cache.join(final_name);
        // A cached wrapper is reused only for the installer choice that
        // produced it: the same selected file, or a Blizzard download.
        let wrapper_expectation = if volatile_wrapper(spec.kind) && !spec.is_custom() {
            Some(match &self.installer_media {
                Some(installer_media) => {
                    WrapperExpectation::Selected(selected_installer_sha256(installer_media, spec)?)
                }
                None => WrapperExpectation::ApprovedNetwork,
            })
        } else {
            None
        };
        let cached_file = match cache_directory.open_file(Path::new(final_name)) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(_) => return Err(AdapterFailure::here()),
        };
        if let Some(cached_file) = cached_file {
            let cached = AcquiredArtifact::new(
                final_path.clone(),
                spec.request_url(),
                spec.request_url(),
                spec.expected_filename.map(str::to_owned),
            );
            let receipt_name = cache_receipt_name(spec);
            let receipt_file = if receipt_bound(spec) {
                match cache_directory.open_file(Path::new(&receipt_name)) {
                    Ok(file) => Some(file),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(_) => return Err(AdapterFailure::here()),
                }
            } else {
                None
            };
            let reusable =
                verify_cached_artifact(spec, cached, &cached_file, receipt_file.as_ref()).map(
                    |verified_cached| {
                        wrapper_expectation
                            .as_ref()
                            .is_none_or(|expected| {
                                expected.accepts(&verified_cached)
                                    && !(matches!(expected, WrapperExpectation::ApprovedNetwork)
                                        && expired_wrapper(&cached_file))
                            })
                            .then_some(verified_cached)
                    },
                );
            match reusable {
                Ok(Some(verified_cached)) => return Ok(verified_cached),
                Ok(None) | Err(()) if receipt_bound(spec) => {
                    remove_regular_cache_entry(cache_directory, final_name, &cached_file)?;
                    if let Some(receipt_file) = receipt_file.as_ref() {
                        remove_regular_cache_entry(
                            cache_directory,
                            OsStr::new(&receipt_name),
                            receipt_file,
                        )?;
                    }
                }
                Ok(None) | Err(()) => return Err(AdapterFailure::here()),
            }
        }

        if self.cache_only && !wrapper_fetch {
            return Err(AdapterFailure::here());
        }

        if volatile_wrapper(spec.kind)
            && let Some(installer_media) = &self.installer_media
        {
            return import_selected_installer(
                installer_media,
                spec,
                cache,
                cache_directory,
                control,
            );
        }

        self.download_from_sources(spec, cache, cache_directory, control, &final_path)
    }
}

impl ProductionArtifactSource {
    /// Download from each of the spec's sources in turn until one gives the
    /// pinned bytes. Standard error names the host each came from.
    fn download_from_sources(
        &self,
        spec: &ArtifactSpec,
        cache: &Path,
        cache_directory: &Directory,
        control: &OperationControl,
        final_path: &Path,
    ) -> Result<AcquiredArtifact, AdapterFailure> {
        let mut sources = spec.sources().peekable();
        while let Some(source) = sources.next() {
            let host = crate::sources::redacted_host(source.request_url());
            match self.download(
                &source,
                cache,
                cache_directory,
                control,
                final_path.to_path_buf(),
                true,
            ) {
                Ok(acquired) => {
                    eprintln!("pd2-launcher: downloaded {:?} from {host}", spec.kind());
                    return Ok(acquired);
                }
                Err(failure) if control.is_cancelled() => return Err(failure),
                Err(failure) => match sources.peek() {
                    Some(next) => eprintln!(
                        "pd2-launcher: {:?} could not be downloaded from {host}; trying {}",
                        spec.kind(),
                        crate::sources::redacted_host(next.request_url())
                    ),
                    None => return Err(failure),
                },
            }
        }
        Err(AdapterFailure::here())
    }
}

impl ProductionArtifactSource {
    /// Download into the kind's partial slot, verify, and publish. The
    /// partial of a resumable download (a pinned artifact with an exact
    /// size, or a custom source) is kept when the user stops or the network
    /// fails, bound to its source, and resumed with `Range` next time; the
    /// completed bytes are verified exactly as a fresh download, and a
    /// resumed partial that does not verify is discarded and downloaded
    /// once more from the start.
    fn download(
        &self,
        spec: &ArtifactSpec,
        cache: &Path,
        cache_directory: &Directory,
        control: &OperationControl,
        final_path: PathBuf,
        allow_restart: bool,
    ) -> Result<AcquiredArtifact, AdapterFailure> {
        let partial_name = artifact_partial_name(spec.kind).to_owned();
        let partial = cache.join(&partial_name);
        let mut output = cache_directory
            .open_or_create_file(OsStr::new(&partial_name))
            .map_err(|_| AdapterFailure::here())?;
        let resumable = resumable_download(spec);
        let fingerprint = partial_fingerprint(spec);
        let recorded = read_partial_binding(cache_directory, spec.kind);
        let existing = output.metadata().map_err(|_| AdapterFailure::here())?.len();
        let bound = resumable
            && recorded
                .as_ref()
                .is_some_and(|binding| binding.fingerprint == fingerprint);
        if existing > 0 && (!bound || existing > spec.maximum_bytes()) {
            // Bytes from another source, or nothing a download of this spec
            // could have produced: start over.
            output.set_len(0).map_err(|_| AdapterFailure::here())?;
        }
        let resumed_from = output.metadata().map_err(|_| AdapterFailure::here())?.len();
        let expected_total = known_size(spec).or_else(|| {
            (resumed_from > 0)
                .then(|| recorded.as_ref().and_then(|binding| binding.total))
                .flatten()
        });
        if resumed_from > 0 && expected_total.is_none() {
            output.set_len(0).map_err(|_| AdapterFailure::here())?;
        }
        let resumed_from = output.metadata().map_err(|_| AdapterFailure::here())?.len();
        let mut binding_total = expected_total;
        // The binding is written with the first accepted response, before
        // any byte of it is stored.
        let mut binding_written = false;
        let mut guard = PartialFileGuard::new(cache_directory, OsStr::new(&partial_name), &output)?
            .keeping_on_stop(resumable);
        let mut observed_final_url = None;
        let mut observed_attachment = None;
        let mut completed = false;

        // The retry budget (attempt count and elapsed time) counts only
        // consecutive attempts without progress; an attempt that stored new
        // bytes renews it.
        let mut started = Instant::now();
        let mut failed = 0_usize;
        for _ in 0..MAX_PROGRESSING_ATTEMPTS {
            reject_artifact_cancelled_with_guard(control, &mut guard)?;
            let range_start = output.metadata().map_err(|_| AdapterFailure::here())?.len();
            let response =
                match self
                    .http
                    .get(spec, (range_start > 0).then_some(range_start), control)
                {
                    Ok(response) => response,
                    Err(_) if control.is_cancelled() => {
                        reject_artifact_cancelled_with_guard(control, &mut guard)?;
                        return Err(AdapterFailure::here());
                    }
                    Err(_)
                        if self
                            .retry_policy
                            .pause(failed, None, started, Some(control)) =>
                    {
                        failed += 1;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
            reject_artifact_cancelled_with_guard(control, &mut guard)?;
            if !spec.approves_final_url(&response.final_url) {
                return Err(AdapterFailure::here());
            }
            if retryable_status(response.status) {
                if self
                    .retry_policy
                    .pause(failed, response.retry_after, started, Some(control))
                {
                    failed += 1;
                    continue;
                }
                reject_artifact_cancelled_with_guard(control, &mut guard)?;
                return Err(AdapterFailure::here());
            }
            let restart = match (
                (range_start > 0).then_some(range_start),
                response.status,
                response.content_range,
            ) {
                (None, 200, None) => false,
                (Some(_), 200, None) => true,
                (Some(start), 206, Some(range)) => {
                    if range.start != start
                        || range.end_inclusive < range.start
                        || range.end_inclusive >= range.complete_length
                    {
                        return Err(AdapterFailure::here());
                    }
                    if binding_total.is_some_and(|total| total != range.complete_length) {
                        // The source changed length since the kept bytes
                        // were written: they cannot be continued.
                        guard.discard()?;
                        remove_partial_binding(cache_directory, spec.kind)?;
                        return Err(AdapterFailure::here());
                    }
                    false
                }
                _ => return Err(AdapterFailure::here()),
            };
            let existing = if restart { 0 } else { range_start };
            let known_total = response
                .content_range
                .map(|range| range.complete_length)
                .or_else(|| {
                    response
                        .declared_length
                        .and_then(|length| existing.checked_add(length))
                });
            if existing > spec.maximum_bytes()
                || known_total.is_some_and(|total| total > spec.maximum_bytes())
            {
                guard.discard()?;
                remove_partial_binding(cache_directory, spec.kind)?;
                return Err(AdapterFailure::here());
            }
            if resumable && (!binding_written || (binding_total.is_none() && known_total.is_some()))
            {
                binding_total = binding_total.or(known_total);
                write_partial_binding(cache_directory, spec.kind, &fingerprint, binding_total)?;
                binding_written = true;
            }

            if restart || range_start == 0 {
                output.set_len(0).map_err(|_| AdapterFailure::here())?;
                output
                    .seek(SeekFrom::Start(0))
                    .map_err(|_| AdapterFailure::here())?;
            } else {
                output
                    .seek(SeekFrom::End(0))
                    .map_err(|_| AdapterFailure::here())?;
            }
            let remaining = spec.maximum_bytes().saturating_sub(existing);
            let declared_length = response.declared_length;
            let content_range = response.content_range;
            let final_url = response.final_url;
            let suggested_filename = response.suggested_filename;
            let mut bounded = response.body.take(remaining + 1);
            let mut copied = 0_u64;
            let mut interrupted = false;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                reject_artifact_cancelled_with_guard(control, &mut guard)?;
                let read = match bounded.read(&mut buffer) {
                    Ok(read) => read,
                    Err(_) => {
                        interrupted = true;
                        break;
                    }
                };
                if read == 0 {
                    break;
                }
                output
                    .write_all(&buffer[..read])
                    .map_err(|_| AdapterFailure::here())?;
                copied = copied
                    .checked_add(read as u64)
                    .ok_or_else(|| AdapterFailure::here())?;
                let completed_bytes = existing
                    .checked_add(copied)
                    .ok_or_else(|| AdapterFailure::here())?;
                let (phase, stage, stages) = artifact_progress(spec.kind);
                control.report(
                    WorkProgress::for_phase(phase)
                        .with_file_bytes(completed_bytes, known_total)
                        .with_overall(ProgressUnit::Stages, stage, Some(stages))
                        .with_verification(0, Some(1)),
                );
            }
            output.flush().map_err(|_| AdapterFailure::here())?;
            if copied > remaining {
                // Over the size bound: never kept for a resume.
                guard.discard()?;
                remove_partial_binding(cache_directory, spec.kind)?;
                return Err(AdapterFailure::here());
            }
            if declared_length.is_some_and(|length| copied != length)
                || content_range.is_some_and(|range| {
                    copied != range.end_inclusive.saturating_sub(range.start) + 1
                })
            {
                interrupted = true;
            }
            observed_final_url = Some(final_url);
            if observed_attachment.is_none() {
                observed_attachment = suggested_filename;
            }
            if interrupted {
                if copied > 0 {
                    failed = 0;
                    started = Instant::now();
                }
                if self
                    .retry_policy
                    .pause(failed, None, started, Some(control))
                {
                    failed += 1;
                    continue;
                }
                reject_artifact_cancelled_with_guard(control, &mut guard)?;
                return Err(AdapterFailure::here());
            }
            completed = match content_range {
                Some(range) => range.end_inclusive + 1 == range.complete_length,
                None => match (declared_length, spec.size) {
                    (None, SizePolicy::Exact(expected)) => {
                        existing.saturating_add(copied) == expected
                    }
                    _ => true,
                },
            };
            // getLegacy rate-limits with an empty (or next to empty) 200
            // answer: asked again after a pause, like a 429.
            let smallest = match spec.size {
                SizePolicy::Exact(size) | SizePolicy::Bounded { min: size, .. } => size,
            };
            if completed
                && volatile_wrapper(spec.kind)
                && !spec.is_custom()
                && existing.saturating_add(copied) < smallest.min(RATE_LIMITED_WRAPPER_BYTES)
            {
                if self
                    .retry_policy
                    .pause(failed, None, started, Some(control))
                {
                    failed += 1;
                    completed = false;
                    output.set_len(0).map_err(|_| AdapterFailure::here())?;
                    continue;
                }
                reject_artifact_cancelled_with_guard(control, &mut guard)?;
                return Err(AdapterFailure::here());
            }
            if completed {
                break;
            }
        }
        if !completed {
            return Err(AdapterFailure::here());
        }
        let final_url = observed_final_url.ok_or_else(|| AdapterFailure::here())?;
        output.flush().map_err(|_| AdapterFailure::here())?;
        output.sync_all().map_err(|_| AdapterFailure::here())?;
        let mut downloaded = AcquiredArtifact::new(
            partial.clone(),
            spec.request_url(),
            final_url.clone(),
            observed_attachment.clone(),
        );
        if receipt_bound(spec) {
            let (observed_sha256, _) =
                hash_open_artifact(&mut output).map_err(|_| AdapterFailure::here())?;
            downloaded = downloaded.with_retained_receipt_sha256(observed_sha256);
        }
        if volatile_wrapper(spec.kind) && !spec.is_custom() {
            // A wrapper downloaded from the built-in Blizzard source passes the
            // same regular-file, size-bound, and PE validation as a wrapper the
            // user selected from a local folder.
            if let Err(error) = validate_selected_installer(spec, &mut output) {
                guard.discard()?;
                remove_partial_binding(cache_directory, spec.kind)?;
                return Err(error);
            }
        }
        let verified = match verify_open_artifact(spec, downloaded, partial, &mut output) {
            Ok(verified) => verified,
            Err(_) => {
                // Unverified bytes are never kept for another resume.
                guard.discard()?;
                remove_partial_binding(cache_directory, spec.kind)?;
                drop(guard);
                drop(output);
                if allow_restart && resumed_from > 0 {
                    return self.download(spec, cache, cache_directory, control, final_path, false);
                }
                return Err(AdapterFailure::here());
            }
        };
        guard
            .directory
            .require_open_file(&guard.name, &guard.opened)
            .map_err(|_| AdapterFailure::here())?;
        let (phase, stage, stages) = artifact_progress(spec.kind);
        control.report(
            WorkProgress::for_phase(phase)
                .with_file_bytes(
                    output.metadata().map_err(|_| AdapterFailure::here())?.len(),
                    known_size(spec),
                )
                .with_overall(ProgressUnit::Stages, stage, Some(stages))
                .with_verification(1, Some(1)),
        );
        let observed_size = output.metadata().map_err(|_| AdapterFailure::here())?.len();
        cache_directory
            .atomic_write_from_open(OsStr::new(cache_name(spec)), &output)
            .map_err(|_| AdapterFailure::here())?;
        guard.remove()?;
        remove_partial_binding(cache_directory, spec.kind)?;
        let acquired = AcquiredArtifact::new(
            final_path,
            spec.request_url(),
            final_url,
            observed_attachment,
        );
        if receipt_bound(spec) {
            write_cache_receipt(cache_directory, spec, &acquired, &verified, observed_size)?;
            return Ok(acquired.with_retained_receipt_sha256(verified.observed_sha256));
        }
        Ok(acquired)
    }
}

/// A Blizzard wrapper embeds a signed download authorization that expires
/// about a week after the wrapper was fetched; an older cached wrapper is
/// downloaded again rather than run.
const WRAPPER_REUSE_LIMIT: Duration = Duration::from_secs(6 * 24 * 60 * 60);

/// A 200 answer for a wrapper shorter than this (and than the wrapper's
/// smallest size) is getLegacy's rate limit, not a wrapper.
const RATE_LIMITED_WRAPPER_BYTES: u64 = 1024;

fn expired_wrapper(cached: &File) -> bool {
    cached
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|fetched| SystemTime::now().duration_since(fetched).ok())
        .is_some_and(|age| age > WRAPPER_REUSE_LIMIT)
}

/// Which installer choice a cached downloader wrapper must come from.
enum WrapperExpectation {
    /// The exact bytes of the file in the selected folder.
    Selected(String),
    /// A download from the effective (built-in) Blizzard source.
    ApprovedNetwork,
}

impl WrapperExpectation {
    fn accepts(&self, cached: &AcquiredArtifact) -> bool {
        match self {
            Self::Selected(sha256) => {
                cached.origin == ArtifactOrigin::UserSelected
                    && cached.retained_receipt_sha256.as_deref() == Some(sha256.as_str())
            }
            Self::ApprovedNetwork => cached.origin == ArtifactOrigin::ApprovedNetwork,
        }
    }
}

/// SHA-256 of the wrapper in the selected folder, after the same validation
/// the import applies.
fn selected_installer_sha256(
    installer_media: &InstallerMedia,
    spec: &ArtifactSpec,
) -> Result<String, AdapterFailure> {
    let filename = spec
        .expected_filename
        .ok_or_else(|| AdapterFailure::here())?;
    let mut source = Directory::open(installer_media.directory())
        .and_then(|directory| directory.open_file(Path::new(filename)))
        .map_err(|_| AdapterFailure::here())?;
    validate_selected_installer(spec, &mut source)?;
    sha256_open_file(&mut source)
}

fn validate_selected_installer(spec: &ArtifactSpec, file: &mut File) -> Result<(), AdapterFailure> {
    if !volatile_wrapper(spec.kind) {
        return Err(AdapterFailure::here());
    }
    let metadata = file.metadata().map_err(|_| AdapterFailure::here())?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(AdapterFailure::here());
    }
    match spec.size {
        SizePolicy::Exact(expected) if metadata.len() != expected => {
            return Err(AdapterFailure::here());
        }
        SizePolicy::Bounded { min, max } if metadata.len() < min || metadata.len() > max => {
            return Err(AdapterFailure::here());
        }
        SizePolicy::Exact(_) | SizePolicy::Bounded { .. } => {}
    }
    validate_open_pe(file).map_err(|_| AdapterFailure::here())
}

fn validate_complete_payload_directory(
    directory: &Directory,
    identity: CompletePayloadIdentity,
    allow_known_source_directories: bool,
) -> Result<(), AdapterFailure> {
    check_complete_payload_directory(directory, identity, allow_known_source_directories)
        .map_err(|_| AdapterFailure::here())
}

/// Why a complete payload did not verify: the file, relative to the payload
/// folder, and the check it failed (`unreadable`, `missing`, `partial` (a
/// `.part` the downloader had not finished), `unexpected`, `not-a-file`,
/// `size`, `not-a-program` or `sha256`), with the size found where it
/// matters. File names come from Blizzard's downloader, never the player.
#[derive(Clone, Debug, Eq, PartialEq)]
struct PayloadProblem {
    file: String,
    check: &'static str,
    size: Option<u64>,
}

impl PayloadProblem {
    fn new(file: impl Into<String>, check: &'static str) -> Self {
        Self {
            file: file.into(),
            check,
            size: None,
        }
    }

    /// Whether the download looks unfinished (a file missing or still a
    /// `.part`), rather than finished with other contents.
    fn incomplete(&self) -> bool {
        matches!(self.check, "missing" | "partial")
    }
}

fn check_complete_payload_directory(
    directory: &Directory,
    identity: CompletePayloadIdentity,
    allow_known_source_directories: bool,
) -> Result<(), PayloadProblem> {
    check_complete_payload_directory_until(
        directory,
        identity,
        allow_known_source_directories,
        &|| false,
    )
}

/// The same check, ended between reads once `stop` says so: its problem
/// is then `stopped`.
fn check_complete_payload_directory_until(
    directory: &Directory,
    identity: CompletePayloadIdentity,
    allow_known_source_directories: bool,
    stop: &dyn Fn() -> bool,
) -> Result<(), PayloadProblem> {
    let mut files = BTreeSet::new();
    let mut directx = None;
    for name in directory
        .names()
        .map_err(|_| PayloadProblem::new(".", "unreadable"))?
    {
        let shown = name.to_string_lossy().into_owned();
        if matches!(name.to_str(), Some(PAYLOAD_INSTALLER | PAYLOAD_TOME)) {
            directory
                .open_file(Path::new(&name))
                .map_err(|_| PayloadProblem::new(shown, "not-a-file"))?;
            files.insert(name);
            continue;
        }
        if name == OsStr::new(DIRECTX_DIRECTORY) {
            directx = Some(
                directory
                    .open_directory(Path::new(&name))
                    .map_err(|_| PayloadProblem::new(shown, "unreadable"))?,
            );
            continue;
        }
        if allow_known_source_directories && name == OsStr::new("Installer.app") {
            directory
                .open_directory(Path::new(&name))
                .map_err(|_| PayloadProblem::new(shown, "unreadable"))?;
            continue;
        }
        return Err(unexpected_payload_entry(shown));
    }
    for expected in [PAYLOAD_INSTALLER, PAYLOAD_TOME] {
        if !files.contains(OsStr::new(expected)) {
            return Err(PayloadProblem::new(expected, "missing"));
        }
    }
    check_complete_payload_file(
        directory,
        "",
        PAYLOAD_INSTALLER,
        identity.installer_size,
        identity.installer_sha256,
        true,
        stop,
    )?;
    check_complete_payload_file(
        directory,
        "",
        PAYLOAD_TOME,
        identity.tome_size,
        identity.tome_sha256,
        false,
        stop,
    )?;
    check_payload_file_set(
        &directx.ok_or_else(|| PayloadProblem::new(DIRECTX_DIRECTORY, "missing"))?,
        identity.directx_files,
        stop,
    )
}

/// Remove a payload file that finished with other contents (`size`,
/// `sha256`, `not-a-program`), only that file and only when it is still the
/// regular file checked; a failure leaves it for the next check to report.
fn discard_payload_file(root: &Directory, payload: &Path, problem: &PayloadProblem) {
    if !matches!(problem.check, "size" | "sha256" | "not-a-program") {
        return;
    }
    let path = payload.join(&problem.file);
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let removed = root.open_directory(parent).and_then(|folder| {
        let file = folder.open_file(Path::new(name))?;
        folder.remove_open_file_bounded(name, &file)
    });
    if let Err(error) = removed {
        eprintln!(
            "pd2-launcher: payload file {} was not removed: {}",
            problem.file,
            error.kind()
        );
    }
}

/// An entry a complete payload does not have; a `.part` is a file the
/// downloader had not finished.
fn unexpected_payload_entry(shown: String) -> PayloadProblem {
    let check = if shown.ends_with(".part") {
        "partial"
    } else {
        "unexpected"
    };
    PayloadProblem::new(shown, check)
}

fn check_payload_file_set(
    directory: &Directory,
    identities: &[PayloadFileIdentity],
    stop: &dyn Fn() -> bool,
) -> Result<(), PayloadProblem> {
    let inside = |name: &str| format!("{DIRECTX_DIRECTORY}/{name}");
    let observed = directory
        .names()
        .map_err(|_| PayloadProblem::new(DIRECTX_DIRECTORY, "unreadable"))?
        .into_iter()
        .collect::<BTreeSet<_>>();
    for name in &observed {
        if !identities
            .iter()
            .any(|identity| OsStr::new(identity.name) == name.as_os_str())
        {
            return Err(unexpected_payload_entry(inside(&name.to_string_lossy())));
        }
    }
    for identity in identities {
        if !observed.contains(OsStr::new(identity.name)) {
            return Err(PayloadProblem::new(inside(identity.name), "missing"));
        }
    }
    for identity in identities {
        check_complete_payload_file(
            directory,
            DIRECTX_DIRECTORY,
            identity.name,
            identity.size,
            identity.sha256,
            false,
            stop,
        )?;
    }
    Ok(())
}

fn check_complete_payload_file(
    directory: &Directory,
    folder: &str,
    name: &str,
    expected_size: u64,
    expected_sha256: &str,
    executable: bool,
    stop: &dyn Fn() -> bool,
) -> Result<(), PayloadProblem> {
    let shown = if folder.is_empty() {
        name.to_owned()
    } else {
        format!("{folder}/{name}")
    };
    let problem = |check| PayloadProblem::new(shown.clone(), check);
    let mut file = directory
        .open_file(Path::new(name))
        .map_err(|_| problem("not-a-file"))?;
    let metadata = file.metadata().map_err(|_| problem("unreadable"))?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(problem("not-a-file"));
    }
    if metadata.len() != expected_size {
        return Err(PayloadProblem {
            size: Some(metadata.len()),
            ..problem("size")
        });
    }
    if executable {
        validate_open_pe(&mut file).map_err(|_| problem("not-a-program"))?;
    }
    // The same file, unchanged, that this process checked in full: not
    // read again. The program itself is always hashed again (it is small,
    // and it may be run).
    if !executable && SESSION_CHECKS.trusts(&metadata, expected_sha256) {
        return Ok(());
    }
    match sha256_open_file_until(&mut file, stop).map_err(|_| problem("unreadable"))? {
        None => return Err(problem("stopped")),
        Some(sha256) if sha256 != expected_sha256 => return Err(problem("sha256")),
        Some(_) => {}
    }
    if !executable {
        SESSION_CHECKS.record(&metadata, expected_sha256);
    }
    Ok(())
}

/// The payload files this engine process checked in full: the same file,
/// unchanged, is trusted for the rest of the process, so Install Diablo II
/// does not read the 1.5 GB Tome again right after the download checked it.
/// A new process checks once again. A file is the same while its device,
/// inode, size, modification time and change time are; any write changes
/// the times.
static SESSION_CHECKS: SessionChecks = SessionChecks::new();

struct SessionChecks(std::sync::Mutex<Vec<(SessionFile, String)>>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SessionFile {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl SessionFile {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

impl SessionChecks {
    /// The pinned payload has 11 files per product.
    const MAX: usize = 64;

    const fn new() -> Self {
        Self(std::sync::Mutex::new(Vec::new()))
    }

    fn record(&self, metadata: &std::fs::Metadata, sha256: &str) {
        let file = SessionFile::of(metadata);
        let mut checked = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        checked.retain(|(known, _)| (known.device, known.inode) != (file.device, file.inode));
        if checked.len() >= Self::MAX {
            checked.remove(0);
        }
        checked.push((file, sha256.to_owned()));
    }

    fn trusts(&self, metadata: &std::fs::Metadata, sha256: &str) -> bool {
        let file = SessionFile::of(metadata);
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|(known, known_sha256)| *known == file && known_sha256 == sha256)
    }
}

/// A product's complete installer already imported into the cache, when it
/// still matches its frozen identity.
fn cached_complete_payload(
    spec: &ArtifactSpec,
    cache: &Path,
    cache_directory: &Directory,
) -> Result<Option<AcquiredArtifact>, AdapterFailure> {
    let Some(identity) = complete_payload_identity(spec.kind) else {
        return Ok(None);
    };
    let retained = match cache_directory.open_directory(Path::new(identity.cache_directory)) {
        Ok(retained) => retained,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(AdapterFailure::here()),
    };
    if validate_complete_payload_directory(&retained, identity, false).is_err() {
        return Ok(None);
    }
    let path = cache.join(identity.cache_directory).join(PAYLOAD_INSTALLER);
    Ok(Some(
        AcquiredArtifact::user_selected(path, spec.expected_filename.unwrap_or(PAYLOAD_INSTALLER))
            .with_retained_receipt_sha256(identity.installer_sha256),
    ))
}

fn import_selected_complete_payload(
    installer_media: &InstallerMedia,
    spec: &ArtifactSpec,
    cache: &Path,
    cache_directory: &Directory,
    control: &OperationControl,
) -> Result<AcquiredArtifact, AdapterFailure> {
    reject_artifact_cancelled(control)?;
    let identity = complete_payload_identity(spec.kind).ok_or_else(|| AdapterFailure::here())?;
    let selected_root =
        Directory::open(installer_media.directory()).map_err(|_| AdapterFailure::here())?;
    let selected = selected_root
        .open_directory(Path::new(identity.source_directory))
        .map_err(|_| AdapterFailure::here())?;
    import_complete_payload(&selected, identity, cache_directory, control)?;
    reject_artifact_cancelled(control)?;
    let path = cache.join(identity.cache_directory).join(PAYLOAD_INSTALLER);
    Ok(AcquiredArtifact::user_selected(
        path,
        spec.expected_filename
            .ok_or_else(|| AdapterFailure::here())?,
    )
    .with_retained_receipt_sha256(identity.installer_sha256))
}

/// Import the complete payload in `selected` into the product's cache
/// folder, where no extra entry is accepted, and verify it there.
fn import_complete_payload(
    selected: &Directory,
    identity: CompletePayloadIdentity,
    cache_directory: &Directory,
    control: &OperationControl,
) -> Result<(), AdapterFailure> {
    validate_complete_payload_directory(selected, identity, true)?;

    let retained = cache_directory
        .ensure_directory(Path::new(identity.cache_directory))
        .map_err(|_| AdapterFailure::here())?;
    let retained_directx = retained
        .ensure_directory(Path::new(DIRECTX_DIRECTORY))
        .map_err(|_| AdapterFailure::here())?;
    for name in retained.names().map_err(|_| AdapterFailure::here())? {
        if name == OsStr::new(DIRECTX_DIRECTORY) {
            retained
                .open_directory(Path::new(&name))
                .map_err(|_| AdapterFailure::here())?;
            continue;
        }
        if name != OsStr::new(PAYLOAD_INSTALLER) && name != OsStr::new(PAYLOAD_TOME) {
            return Err(AdapterFailure::here());
        }
        retained
            .open_file(Path::new(&name))
            .map_err(|_| AdapterFailure::here())?;
    }
    let expected_directx = identity
        .directx_files
        .iter()
        .map(|identity| OsString::from(identity.name))
        .collect::<BTreeSet<_>>();
    for name in retained_directx
        .names()
        .map_err(|_| AdapterFailure::here())?
    {
        if !expected_directx.contains(&name) {
            return Err(AdapterFailure::here());
        }
        retained_directx
            .open_file(Path::new(&name))
            .map_err(|_| AdapterFailure::here())?;
    }
    if validate_complete_payload_directory(&retained, identity, false).is_err() {
        for name in [PAYLOAD_INSTALLER, PAYLOAD_TOME] {
            reject_artifact_cancelled(control)?;
            let source = selected
                .open_file(Path::new(name))
                .map_err(|_| AdapterFailure::here())?;
            retained
                .atomic_write_from_open(OsStr::new(name), &source)
                .map_err(|_| AdapterFailure::here())?;
        }
        let selected_directx = selected
            .open_directory(Path::new(DIRECTX_DIRECTORY))
            .map_err(|_| AdapterFailure::here())?;
        for file in identity.directx_files {
            reject_artifact_cancelled(control)?;
            let source = selected_directx
                .open_file(Path::new(file.name))
                .map_err(|_| AdapterFailure::here())?;
            retained_directx
                .atomic_write_from_open(OsStr::new(file.name), &source)
                .map_err(|_| AdapterFailure::here())?;
        }
        validate_complete_payload_directory(&retained, identity, false)?;
    }
    // Imported (or from Blizzard's downloader): not the direct download's,
    // so a discard keeps it.
    remove_file_if_present(cache_directory, &direct_marker(&identity))
        .map_err(|_| AdapterFailure::here())?;
    Ok(())
}

/// The file that says the direct download made `identity`'s complete
/// installer in the cache.
fn direct_marker(identity: &CompletePayloadIdentity) -> String {
    format!("{}.direct", identity.cache_directory)
}

fn remove_file_if_present(directory: &Directory, name: &str) -> io::Result<()> {
    match directory.open_file(Path::new(name)) {
        Ok(file) => directory.remove_open_file_bounded(OsStr::new(name), &file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Remove what this Launcher's own downloads made, for a player who imports
/// the game instead (the Control channel's `discard_downloads`): Blizzard's
/// downloader programs it downloaded (one the player chose from a folder
/// stays), the custom downloads of them and of Project Diablo 2's installer,
/// Project Diablo 2's installer and client files, their partial downloads,
/// the direct download's folders and the complete installers it made, and
/// the Download's record. Nothing imported is removed. The caller holds the
/// installation lease.
pub(crate) fn discard_downloads(paths: &ManagedPaths) -> io::Result<()> {
    let root = paths.directory();
    direct::discard(paths)?;
    let open = |path: &str| match root.open_directory(Path::new(path)) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    };
    if let Some(cache) = open("cache/artifacts")? {
        for kind in [
            ArtifactKind::BaseGameWrapper,
            ArtifactKind::ExpansionWrapper,
            ArtifactKind::ClientBootstrap,
        ] {
            let pinned = pinned_cache_name(kind);
            let receipt = format!("{pinned}.provenance.json");
            // A Blizzard downloader program says where it came from: only a
            // downloaded one goes. The pinned PD2 installer has no receipt.
            let downloaded = kind == ArtifactKind::ClientBootstrap
                || read_cache_receipt(&cache, &receipt)
                    .is_some_and(|origin| origin == ArtifactOrigin::ApprovedNetwork);
            if downloaded {
                remove_file_if_present(&cache, pinned)?;
                remove_file_if_present(&cache, &receipt)?;
            }
            let custom = custom_cache_name(kind);
            remove_file_if_present(&cache, custom)?;
            remove_file_if_present(&cache, &format!("{custom}.provenance.json"))?;
            let partial = artifact_partial_name(kind);
            remove_file_if_present(&cache, partial)?;
            remove_file_if_present(&cache, &format!("{partial}.source"))?;
        }
    }
    if let Some(cache) = open("cache")? {
        match cache.open_directory(Path::new("client")) {
            Ok(_) => cache.remove_owned_tree(OsStr::new("client"))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    crate::downloads::clear(root)?;
    eprintln!("pd2-launcher: the Launcher's own downloads were removed for an import");
    Ok(())
}

/// The origin a cache receipt names; `None` when there is none or it does
/// not read.
fn read_cache_receipt(cache: &Directory, name: &str) -> Option<ArtifactOrigin> {
    let mut file = cache.open_file(Path::new(name)).ok()?;
    if file.metadata().ok()?.len() > 16 * 1024 {
        return None;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    serde_json::from_slice::<CacheReceipt>(&bytes)
        .ok()
        .map(|receipt| receipt.origin)
}

fn import_selected_installer(
    installer_media: &InstallerMedia,
    spec: &ArtifactSpec,
    cache: &Path,
    cache_directory: &Directory,
    control: &OperationControl,
) -> Result<AcquiredArtifact, AdapterFailure> {
    reject_artifact_cancelled(control)?;
    let filename = spec
        .expected_filename
        .ok_or_else(|| AdapterFailure::here())?;
    let source_directory =
        Directory::open(installer_media.directory()).map_err(|_| AdapterFailure::here())?;
    let mut source = source_directory
        .open_file(Path::new(filename))
        .map_err(|_| AdapterFailure::here())?;
    validate_selected_installer(spec, &mut source)?;
    let source_size = source.metadata().map_err(|_| AdapterFailure::here())?.len();
    source.rewind().map_err(|_| AdapterFailure::here())?;

    let partial_name = artifact_partial_name(spec.kind).to_owned();
    let partial = cache.join(&partial_name);
    let mut output = cache_directory
        .open_or_create_file(OsStr::new(&partial_name))
        .map_err(|_| AdapterFailure::here())?;
    output.set_len(0).map_err(|_| AdapterFailure::here())?;
    output.rewind().map_err(|_| AdapterFailure::here())?;
    let mut guard = PartialFileGuard::new(cache_directory, OsStr::new(&partial_name), &output)?;
    let mut bounded = source.take(spec.maximum_bytes().saturating_add(1));
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        reject_artifact_cancelled_with_guard(control, &mut guard)?;
        let count = bounded
            .read(&mut buffer)
            .map_err(|_| AdapterFailure::here())?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|_| AdapterFailure::here())?;
        copied = copied
            .checked_add(count as u64)
            .ok_or_else(|| AdapterFailure::here())?;
        let (phase, stage, stages) = artifact_progress(spec.kind);
        control.report(
            WorkProgress::for_phase(phase)
                .with_file_bytes(copied, Some(source_size))
                .with_overall(ProgressUnit::Stages, stage, Some(stages))
                .with_verification(0, Some(1)),
        );
    }
    if copied != source_size || copied > spec.maximum_bytes() {
        return Err(AdapterFailure::here());
    }
    output.flush().map_err(|_| AdapterFailure::here())?;
    output.sync_all().map_err(|_| AdapterFailure::here())?;
    let mut selected = AcquiredArtifact::user_selected(partial.clone(), filename);
    let (observed_sha256, _) =
        hash_open_artifact(&mut output).map_err(|_| AdapterFailure::here())?;
    selected = selected.with_retained_receipt_sha256(observed_sha256);
    let verified = verify_open_artifact(spec, selected, partial, &mut output)
        .map_err(|_| AdapterFailure::here())?;
    guard
        .directory
        .require_open_file(&guard.name, &guard.opened)
        .map_err(|_| AdapterFailure::here())?;
    cache_directory
        .atomic_write_from_open(OsStr::new(cache_name(spec)), &output)
        .map_err(|_| AdapterFailure::here())?;
    guard.remove()?;

    let acquired = AcquiredArtifact::user_selected(cache.join(cache_name(spec)), filename)
        .with_retained_receipt_sha256(verified.observed_sha256.clone());
    write_cache_receipt(cache_directory, spec, &acquired, &verified, source_size)?;
    let (phase, stage, stages) = artifact_progress(spec.kind);
    control.report(
        WorkProgress::for_phase(phase)
            .with_file_bytes(source_size, Some(source_size))
            .with_overall(ProgressUnit::Stages, stage, Some(stages))
            .with_verification(1, Some(1)),
    );
    Ok(acquired)
}

/// Seed a new managed root's artifact cache from an old root on another
/// filesystem. Only pinned downloads (exact size and digest) are
/// copied, and each copy is re-verified against its spec before it is
/// published under its cache name; anything that does not verify is
/// dropped and downloaded again later. Returns the kinds that were seeded.
pub(crate) fn seed_verified_cache(
    catalog: &ArtifactCatalog,
    from: &Directory,
    to: &Directory,
    control: &OperationControl,
) -> Result<Vec<ArtifactKind>, AdapterFailure> {
    let source = match from.open_directory(Path::new("cache/artifacts")) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(AdapterFailure::here()),
    };
    let cache = to
        .ensure_directory(Path::new("cache/artifacts"))
        .map_err(|_| AdapterFailure::here())?;
    validate_artifact_inventory(&cache).map_err(|_| AdapterFailure::here())?;
    let mut seeded = Vec::new();
    for kind in [
        ArtifactKind::ManagedRuntime,
        ArtifactKind::Gecko,
        ArtifactKind::WineMono,
        ArtifactKind::DotnetDesktop,
        ArtifactKind::ClientBootstrap,
    ] {
        reject_artifact_cancelled(control)?;
        let spec = catalog.spec(kind);
        if spec.is_custom() || known_size(spec).is_none() {
            continue;
        }
        let final_name = OsStr::new(cache_name(spec));
        if cache.open_file(Path::new(final_name)).is_ok() {
            continue;
        }
        let mut input = match source.open_file(Path::new(final_name)) {
            Ok(input) => input,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return Err(AdapterFailure::here()),
        };
        let partial_name = artifact_partial_name(kind);
        let mut output = cache
            .open_or_create_file(OsStr::new(partial_name))
            .map_err(|_| AdapterFailure::here())?;
        output.set_len(0).map_err(|_| AdapterFailure::here())?;
        output.rewind().map_err(|_| AdapterFailure::here())?;
        let mut guard = PartialFileGuard::new(&cache, OsStr::new(partial_name), &output)?;
        let total = known_size(spec);
        let mut bounded = (&mut input).take(spec.maximum_bytes().saturating_add(1));
        let mut copied = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            reject_artifact_cancelled_with_guard(control, &mut guard)?;
            let count = bounded
                .read(&mut buffer)
                .map_err(|_| AdapterFailure::here())?;
            if count == 0 {
                break;
            }
            output
                .write_all(&buffer[..count])
                .map_err(|_| AdapterFailure::here())?;
            copied = copied.saturating_add(count as u64);
            let (phase, stage, stages) = artifact_progress(kind);
            control.report(
                WorkProgress::for_phase(phase)
                    .with_file_bytes(copied, total)
                    .with_overall(ProgressUnit::Stages, stage, Some(stages)),
            );
        }
        output.sync_all().map_err(|_| AdapterFailure::here())?;
        let copy = AcquiredArtifact::new(
            PathBuf::from(partial_name),
            spec.request_url(),
            spec.request_url(),
            None,
        );
        if verify_open_artifact(spec, copy, PathBuf::from(partial_name), &mut output).is_err() {
            guard.discard()?;
            continue;
        }
        cache
            .atomic_write_from_open(final_name, &output)
            .map_err(|_| AdapterFailure::here())?;
        guard.remove()?;
        seeded.push(kind);
    }
    Ok(seeded)
}

fn known_size(spec: &ArtifactSpec) -> Option<u64> {
    match spec.size {
        SizePolicy::Exact(size) => Some(size),
        SizePolicy::Bounded { .. } => None,
    }
}

fn artifact_progress(kind: ArtifactKind) -> (WorkPhase, u64, u64) {
    match kind {
        ArtifactKind::ManagedRuntime
        | ArtifactKind::Gecko
        | ArtifactKind::WineMono
        | ArtifactKind::DotnetDesktop
        | ArtifactKind::FexPackage
        | ArtifactKind::FexRootFs => (WorkPhase::RuntimeAcquisition, 0, 4),
        // Downloads and imports only: installing reports its own phases.
        ArtifactKind::BaseGameWrapper => (WorkPhase::BaseGameDownload, 1, 4),
        ArtifactKind::ExpansionWrapper => (WorkPhase::ExpansionDownload, 2, 4),
        ArtifactKind::ClientBootstrap => (WorkPhase::ClientDownload, 3, 4),
    }
}

fn reject_artifact_cancelled(control: &OperationControl) -> Result<(), AdapterFailure> {
    if control.is_cancelled() {
        control.mark_cancelled_safe();
        Err(AdapterFailure::here())
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CacheReceipt {
    schema: u32,
    origin: ArtifactOrigin,
    requested_url: String,
    final_url: String,
    attachment: String,
    observed_size: u64,
    observed_sha256: String,
}

/// Whether a cached object is reusable only with a matching provenance
/// receipt: volatile Blizzard wrappers (no published digest) and every custom
/// source, whose receipt binds the cached bytes to the exact custom URL.
fn receipt_bound(spec: &ArtifactSpec) -> bool {
    volatile_wrapper(spec.kind) || spec.is_custom()
}

fn volatile_wrapper(kind: ArtifactKind) -> bool {
    matches!(
        kind,
        ArtifactKind::BaseGameWrapper | ArtifactKind::ExpansionWrapper
    )
}

fn cache_receipt_name(spec: &ArtifactSpec) -> String {
    format!("{}.provenance.json", cache_name(spec))
}

fn verify_cached_artifact(
    spec: &ArtifactSpec,
    mut cached: AcquiredArtifact,
    cached_file: &File,
    receipt_file: Option<&File>,
) -> Result<AcquiredArtifact, ()> {
    if receipt_bound(spec) {
        let mut receipt_file = receipt_file.ok_or(())?.try_clone().map_err(|_| ())?;
        let receipt_metadata = receipt_file.metadata().map_err(|_| ())?;
        if !receipt_metadata.is_file()
            || receipt_metadata.nlink() != 1
            || receipt_metadata.len() > 16 * 1024
        {
            return Err(());
        }
        let mut bytes = Vec::with_capacity(receipt_metadata.len() as usize);
        receipt_file.rewind().map_err(|_| ())?;
        receipt_file
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ())?;
        if bytes.len() > 16 * 1024 {
            return Err(());
        }
        let receipt: CacheReceipt = serde_json::from_slice(&bytes).map_err(|_| ())?;
        if receipt.schema != CACHE_RECEIPT_SCHEMA
            || spec
                .expected_filename
                .is_some_and(|expected| receipt.attachment != expected)
            || (spec.is_custom()
                && receipt.requested_url != custom_url_fingerprint(spec.request_url()))
        {
            return Err(());
        }
        cached.origin = receipt.origin;
        // A custom receipt holds only the URL's fingerprint, proven equal to
        // the configured URL above.
        cached.requested_url = if spec.is_custom() {
            spec.request_url().to_owned()
        } else {
            receipt.requested_url.clone()
        };
        cached.final_url = receipt.final_url.clone();
        cached.suggested_filename =
            (!receipt.attachment.is_empty()).then(|| receipt.attachment.clone());
        cached.retained_receipt_sha256 = Some(receipt.observed_sha256.clone());
        let mut verified_file = cached_file.try_clone().map_err(|_| ())?;
        let retained_path = cached.path.clone();
        let verified =
            verify_open_artifact(spec, cached.clone(), retained_path, &mut verified_file)
                .map_err(|_| ())?;
        let size = cached_file.metadata().map_err(|_| ())?.len();
        if receipt.observed_size != size || receipt.observed_sha256 != verified.observed_sha256 {
            return Err(());
        }
        Ok(cached)
    } else {
        let mut verified_file = cached_file.try_clone().map_err(|_| ())?;
        let retained_path = cached.path.clone();
        verify_open_artifact(spec, cached.clone(), retained_path, &mut verified_file)
            .map(|_| cached)
            .map_err(|_| ())
    }
}

fn write_cache_receipt(
    cache: &Directory,
    spec: &ArtifactSpec,
    acquired: &AcquiredArtifact,
    verified: &VerifiedArtifact,
    observed_size: u64,
) -> Result<(), AdapterFailure> {
    let attachment = match (&acquired.suggested_filename, spec.is_custom()) {
        (Some(attachment), _) => attachment.clone(),
        (None, true) => String::new(),
        (None, false) => return Err(AdapterFailure::here()),
    };
    // A custom URL may carry a signed-URL token or credentials: its receipt
    // keeps only the requested URL's SHA-256 (to match the same configured
    // URL) and the final URL without credentials, query, or fragment.
    let (requested_url, final_url) = if spec.is_custom() {
        (
            custom_url_fingerprint(&acquired.requested_url),
            crate::sources::redacted_url(&acquired.final_url),
        )
    } else {
        (acquired.requested_url.clone(), acquired.final_url.clone())
    };
    let receipt = CacheReceipt {
        schema: CACHE_RECEIPT_SCHEMA,
        origin: acquired.origin,
        requested_url,
        final_url,
        attachment,
        observed_size,
        observed_sha256: verified.observed_sha256.clone(),
    };
    let bytes = serde_json::to_vec(&receipt).map_err(|_| AdapterFailure::here())?;
    cache
        .atomic_write(OsStr::new(&cache_receipt_name(spec)), &bytes)
        .map_err(|_| AdapterFailure::here())
}

fn custom_url_fingerprint(url: &str) -> String {
    format!("sha256:{}", hex(&Sha256::digest(url.as_bytes())))
}

/// Remove a cached built-in downloader wrapper and its receipt, so the next
/// acquisition downloads a fresh one.
pub(super) fn discard_cached_wrapper(
    spec: &ArtifactSpec,
    cache_directory: &Directory,
) -> Result<(), AdapterFailure> {
    if !volatile_wrapper(spec.kind) || spec.is_custom() {
        return Err(AdapterFailure::here());
    }
    for name in [cache_name(spec).to_owned(), cache_receipt_name(spec)] {
        match cache_directory.open_file(Path::new(&name)) {
            Ok(file) => remove_regular_cache_entry(cache_directory, OsStr::new(&name), &file)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(AdapterFailure::here()),
        }
    }
    Ok(())
}

fn remove_regular_cache_entry(
    directory: &Directory,
    name: &OsStr,
    opened: &File,
) -> Result<(), AdapterFailure> {
    directory
        .forget_atomic_target(name, opened)
        .map_err(|_| AdapterFailure::here())?;
    directory
        .truncate_open_file(name, opened)
        .map_err(|_| AdapterFailure::here())
}

fn cache_name(spec: &ArtifactSpec) -> &'static str {
    if spec.is_custom() {
        return custom_cache_name(spec.kind);
    }
    pinned_cache_name(spec.kind)
}

/// The names under which the artifact cache (`cache/artifacts`) holds a
/// verified `kind`, as the Download records it: its pinned artifact's and a
/// custom source's.
pub(crate) fn recorded_cache_names(kind: ArtifactKind) -> [&'static str; 2] {
    [pinned_cache_name(kind), custom_cache_name(kind)]
}

fn pinned_cache_name(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::ManagedRuntime => managed_runtime().cache_name,
        ArtifactKind::Gecko => "wine-gecko-2.47.4-x86.verified",
        ArtifactKind::WineMono => "wine-mono-11.3.0-x86.verified",
        ArtifactKind::DotnetDesktop => "windowsdesktop-runtime-6.0.36-win-x86.verified",
        ArtifactKind::BaseGameWrapper => "d2-wrapper.verified",
        ArtifactKind::ExpansionWrapper => "lod-wrapper.verified",
        ArtifactKind::ClientBootstrap => "pd2-bootstrap.verified",
        ArtifactKind::FexPackage => crate::builtin_fex::FexVariant::current().cache_name(),
        ArtifactKind::FexRootFs => crate::builtin_fex::ROOTFS_CACHE_NAME,
    }
}

/// Custom-source objects never share a cache name with a pinned artifact, so
/// switching back to the built-in source always re-verifies from scratch.
fn custom_cache_name(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::ManagedRuntime => "custom-wine.download",
        ArtifactKind::Gecko => "custom-gecko.download",
        ArtifactKind::WineMono => "custom-mono.download",
        ArtifactKind::DotnetDesktop => "custom-dotnet-desktop.download",
        ArtifactKind::BaseGameWrapper => "custom-d2-installer.download",
        ArtifactKind::ExpansionWrapper => "custom-lod-installer.download",
        ArtifactKind::ClientBootstrap => "custom-pd2-installer.download",
        ArtifactKind::FexPackage => "custom-fex.download",
        ArtifactKind::FexRootFs => "custom-fex-rootfs.download",
    }
}

fn artifact_partial_name(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::ManagedRuntime => "artifact-00.partial",
        ArtifactKind::Gecko => "artifact-01.partial",
        ArtifactKind::BaseGameWrapper => "artifact-02.partial",
        ArtifactKind::ExpansionWrapper => "artifact-03.partial",
        ArtifactKind::ClientBootstrap => "artifact-05.partial",
        ArtifactKind::WineMono => "artifact-06.partial",
        ArtifactKind::DotnetDesktop => "artifact-07.partial",
        ArtifactKind::FexPackage => "artifact-08.partial",
        ArtifactKind::FexRootFs => "artifact-09.partial",
    }
}

struct PartialFileGuard {
    directory: Directory,
    name: OsString,
    opened: File,
    persisted: bool,
    /// A resumable download keeps its bytes when it stops early.
    keep_on_stop: bool,
}

impl PartialFileGuard {
    fn new(directory: &Directory, name: &OsStr, opened: &File) -> Result<Self, AdapterFailure> {
        Ok(Self {
            directory: directory.try_clone().map_err(|_| AdapterFailure::here())?,
            name: name.to_owned(),
            opened: opened.try_clone().map_err(|_| AdapterFailure::here())?,
            persisted: false,
            keep_on_stop: false,
        })
    }

    fn keeping_on_stop(mut self, keep: bool) -> Self {
        self.keep_on_stop = keep;
        self
    }

    /// Keep the exact partial durably for a later resume.
    fn keep(&mut self) -> Result<(), AdapterFailure> {
        self.directory
            .require_open_file(&self.name, &self.opened)
            .map_err(|_| AdapterFailure::here())?;
        self.opened.sync_all().map_err(|_| AdapterFailure::here())?;
        self.persisted = true;
        Ok(())
    }

    /// Remove the slot once its bytes were published: an empty slot left
    /// behind has nothing to resume.
    fn remove(&mut self) -> Result<(), AdapterFailure> {
        self.directory
            .remove_open_file_bounded(&self.name, &self.opened)
            .map_err(|_| AdapterFailure::here())?;
        self.persisted = true;
        Ok(())
    }

    fn discard(&mut self) -> Result<(), AdapterFailure> {
        self.directory
            .truncate_open_file(&self.name, &self.opened)
            .map_err(|_| AdapterFailure::here())?;
        self.persisted = true;
        Ok(())
    }
}

/// Remove every slot that holds no bytes (older releases left them behind).
/// Acquisition runs under the exclusive installation lease, so no slot is
/// being written; a slot with bytes is a resumable partial and is kept.
fn remove_empty_partials(cache: &Directory) -> Result<(), AdapterFailure> {
    for kind in [
        ArtifactKind::ManagedRuntime,
        ArtifactKind::Gecko,
        ArtifactKind::WineMono,
        ArtifactKind::DotnetDesktop,
        ArtifactKind::BaseGameWrapper,
        ArtifactKind::ExpansionWrapper,
        ArtifactKind::ClientBootstrap,
    ] {
        let name = artifact_partial_name(kind);
        let slot = match cache.open_file(Path::new(name)) {
            Ok(slot) => slot,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return Err(AdapterFailure::here()),
        };
        if slot.metadata().map_err(|_| AdapterFailure::here())?.len() != 0 {
            continue;
        }
        cache
            .remove_open_file_bounded(OsStr::new(name), &slot)
            .map_err(|_| AdapterFailure::here())?;
        remove_partial_binding(cache, kind)?;
    }
    Ok(())
}

fn reject_artifact_cancelled_with_guard(
    control: &OperationControl,
    guard: &mut PartialFileGuard,
) -> Result<(), AdapterFailure> {
    if !control.is_cancelled() {
        return Ok(());
    }
    let settled = if guard.keep_on_stop {
        guard.keep()
    } else {
        guard.discard()
    };
    if settled.is_ok() {
        control.mark_cancelled_safe();
    } else {
        control.mark_cancel_unproven();
    }
    Err(AdapterFailure::here())
}

impl Drop for PartialFileGuard {
    fn drop(&mut self) {
        if !self.persisted {
            if self.keep_on_stop {
                let _ = self.opened.sync_all();
            } else {
                let _ = self.directory.truncate_open_file(&self.name, &self.opened);
            }
        }
    }
}

/// Whether a stopped download keeps its partial for a later `Range`
/// resume: a pinned artifact with an exact size (its digest still decides),
/// or a custom source. Downloader wrappers are small and PE-validated from
/// one response, so they always start over.
fn resumable_download(spec: &ArtifactSpec) -> bool {
    !volatile_wrapper(spec.kind) && (known_size(spec).is_some() || spec.is_custom())
}

/// Binds a kept partial to the exact source that produced it, so bytes
/// from another URL, size, or digest are never continued.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PartialBinding {
    schema: u32,
    fingerprint: String,
    /// The complete length the source declared, when known.
    total: Option<u64>,
}

fn partial_fingerprint(spec: &ArtifactSpec) -> String {
    let identity = format!(
        "{}\n{:?}\n{:?}\n{:?}",
        spec.request_url(),
        spec.size,
        spec.integrity,
        spec.redirects
    );
    format!("sha256:{}", hex(&Sha256::digest(identity.as_bytes())))
}

fn partial_binding_name(kind: ArtifactKind) -> String {
    format!("{}.source", artifact_partial_name(kind))
}

fn read_partial_binding(cache: &Directory, kind: ArtifactKind) -> Option<PartialBinding> {
    let file = cache
        .open_file(Path::new(&partial_binding_name(kind)))
        .ok()?;
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes).ok()?;
    serde_json::from_slice::<PartialBinding>(&bytes)
        .ok()
        .filter(|binding| binding.schema == 1 && bytes.len() <= 4096)
}

fn remove_partial_binding(cache: &Directory, kind: ArtifactKind) -> Result<(), AdapterFailure> {
    let name = partial_binding_name(kind);
    match cache.open_file(Path::new(&name)) {
        Ok(file) => cache
            .remove_open_file_bounded(OsStr::new(&name), &file)
            .map_err(|_| AdapterFailure::here()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(AdapterFailure::here()),
    }
}

fn write_partial_binding(
    cache: &Directory,
    kind: ArtifactKind,
    fingerprint: &str,
    total: Option<u64>,
) -> Result<(), AdapterFailure> {
    let bytes = serde_json::to_vec(&PartialBinding {
        schema: 1,
        fingerprint: fingerprint.to_owned(),
        total,
    })
    .map_err(|_| AdapterFailure::here())?;
    cache
        .atomic_write(OsStr::new(&partial_binding_name(kind)), &bytes)
        .map_err(|_| AdapterFailure::here())
}

/// Guest Wine implementation for the accepted unattended stage contracts.
#[derive(Clone)]
pub(crate) struct ProductionInstallDriver {
    host: Arc<dyn HostExecution>,
    custom_wine: bool,
    wpf_software_rendering: bool,
    /// A game that the official launcher starts gets Box64's strict x86
    /// memory order ([`crate::box64::Run::OfficialLauncher`]).
    game_strict_memory_order: bool,
    /// The official launcher's Wine, and a game that it starts, log in detail
    /// (the Android app's Detailed logging).
    detailed_logging: bool,
    download_stall_limit: Duration,
    download_resume_limit: Duration,
    download_empty_limit: Duration,
    /// The host's received-byte count ([`RECEIVED_BYTES_FILE`]).
    received_bytes: Option<PathBuf>,
    /// The Launcher engine runs on the Android host
    /// ([`crate::android_host`], read when the driver is made): the app's
    /// runtime, its prefix and the Android Wine settings. Tests set it for
    /// either host.
    android: bool,
    /// The official launcher's x86 .NET runs translated
    /// ([`TRANSLATED_DOTNET`]). Tests set it for either kind of host.
    translated_dotnet: bool,
    /// The program [`Self::run_official_launcher`] runs, a path in the game
    /// folder ([`crate::persistence::program_path`]).
    official_launcher: PathBuf,
    /// The player's own start options for it, each one argument.
    official_launcher_arguments: Vec<OsString>,
}

impl ProductionInstallDriver {
    pub(crate) fn new(host: Arc<dyn HostExecution>) -> Self {
        Self {
            host,
            custom_wine: false,
            wpf_software_rendering: true,
            game_strict_memory_order: false,
            detailed_logging: false,
            download_stall_limit: DOWNLOAD_STALL_LIMIT,
            download_resume_limit: DOWNLOAD_RESUME_LIMIT,
            download_empty_limit: DOWNLOAD_EMPTY_LIMIT,
            received_bytes: env::var_os(RECEIVED_BYTES_FILE)
                .filter(|path| !path.is_empty())
                .map(PathBuf::from),
            android: crate::android_host(),
            translated_dotnet: TRANSLATED_DOTNET,
            official_launcher: PathBuf::from(crate::persistence::DEFAULT_LAUNCHER_PROGRAM),
            official_launcher_arguments: Vec::new(),
        }
    }

    /// The driver on the Android host (`android`) or the desktop, for tests:
    /// the host switch is the whole process's. The functions of other
    /// modules it calls still read that switch (the game folder copy of an
    /// import, `game_folder`), so a test of those parts stays the desktop's.
    #[cfg(test)]
    fn on_android_host(mut self, android: bool) -> Self {
        self.android = android;
        self
    }

    /// The driver of a host whose official launcher's .NET runs translated
    /// (`translated`, ARM64) or natively, for tests on any host.
    #[cfg(test)]
    fn with_translated_dotnet(mut self, translated: bool) -> Self {
        self.translated_dotnet = translated;
        self
    }

    /// Another [`DOWNLOAD_STALL_LIMIT`], also for an empty folder, for tests
    /// that cannot wait for them.
    #[cfg(test)]
    fn with_download_stall_limit(mut self, limit: Duration) -> Self {
        self.download_stall_limit = limit;
        self.download_resume_limit = limit;
        self.download_empty_limit = limit;
        self
    }

    /// Another received-byte count file, for tests.
    #[cfg(test)]
    fn with_received_bytes(mut self, file: PathBuf) -> Self {
        self.received_bytes = Some(file);
        self
    }

    /// The official-launcher software rendering setting: written into a new
    /// prefix, and synced into the prefix before the official launcher opens.
    pub(crate) fn with_wpf_software_rendering(mut self, enabled: bool) -> Self {
        self.wpf_software_rendering = enabled;
        self
    }

    /// Whether a game that the official launcher starts gets Box64's strict
    /// x86 memory order (the Android app's Strict CPU emulation).
    pub(crate) fn with_game_strict_memory_order(mut self, enabled: bool) -> Self {
        self.game_strict_memory_order = enabled;
        self
    }

    /// Whether the official launcher's Wine, and a game that it starts, log
    /// in detail (the Android app's Detailed logging).
    pub(crate) fn with_detailed_logging(mut self, enabled: bool) -> Self {
        self.detailed_logging = enabled;
        self
    }

    /// The program the official launcher job runs, a path in the game folder
    /// that [`crate::persistence::program_path`] accepted, and the player's
    /// own start options for it, split on whitespace.
    pub(crate) fn with_official_launcher(mut self, program: &str, arguments: &str) -> Self {
        self.official_launcher = PathBuf::from(program);
        self.official_launcher_arguments =
            arguments.split_whitespace().map(OsString::from).collect();
        self
    }

    /// Set or remove the WPF software rendering value in the prefix to match
    /// the setting, with Wine's `reg` only when the prefix's `user.reg` does
    /// not match it already. Each `reg` run starts the prefix and then waits
    /// for its wineserver to exit, which took about 5 s of each official
    /// launcher start on a slow phone.
    fn sync_wpf_software_rendering(
        &self,
        paths: &ManagedPaths,
        runtime: InstalledRuntime,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        if prefix_user_reg(paths).is_some_and(|user_reg| {
            wpf_software_rendering_matches(&user_reg, self.wpf_software_rendering)
        }) {
            return Ok(());
        }
        self.run_wine(WineInvocation {
            paths,
            runtime,
            working_directory: paths.root(),
            program: WineProgram::Builtin(OsString::from("reg")),
            verified_argument: None,
            arguments: wpf_software_rendering_arguments(self.wpf_software_rendering),
            profile: WineProfile::Install,
            timeout: RUNTIME_TIMEOUT,
            mutation_lease,
            control,
            ui_helper: None,
            stall: None,
            accepted_exit_codes: if self.wpf_software_rendering {
                &[]
            } else {
                &[REG_VALUE_ABSENT_STATUS]
            },
        })
    }

    /// GLX for the prefix's OpenGL ([`X11_DRIVER_KEY`]), set by Wine's
    /// `reg add` unless its `user.reg` has it already. The app's prefix
    /// template carries `"UseEGL"="N"`, so a copy of it needs no Wine for it.
    /// This runs only while the audit rejects the runtime or the prefix: a
    /// prefix the audit accepts (one kept from FEX 0.2.x, say) stays as it
    /// is.
    fn sync_x11_glx(
        &self,
        paths: &ManagedPaths,
        runtime: InstalledRuntime,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        if prefix_user_reg(paths).is_some_and(|user_reg| x11_glx_set(&user_reg)) {
            return Ok(());
        }
        self.run_wine(WineInvocation {
            paths,
            runtime,
            working_directory: paths.root(),
            program: WineProgram::Builtin(OsString::from("reg")),
            verified_argument: None,
            arguments: x11_glx_arguments(),
            profile: WineProfile::Install,
            timeout: RUNTIME_TIMEOUT,
            mutation_lease,
            control,
            ui_helper: None,
            stall: None,
            accepted_exit_codes: &[],
        })
    }

    /// The phone's own fonts in the prefix ([`crate::phone_fonts`]), on the
    /// Android host: Wine's `reg import` runs only when the prefix's
    /// `system.reg` lacks the values of the app's current list (the first
    /// time, after a prefix copy, after the phone's fonts change). The log
    /// names the font of each main language and the languages without one.
    /// [`Self::sync_phone_fonts`] for the managed root at `managed_root`,
    /// before a Play.
    pub(crate) fn sync_phone_fonts_at(
        &self,
        managed_root: &Path,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        if !self.android {
            return Ok(());
        }
        let paths = ManagedPaths::prepare_on(managed_root, self.android)
            .map_err(|_| AdapterFailure::here())?;
        self.sync_phone_fonts(&paths, mutation_lease, control)
    }

    fn sync_phone_fonts(
        &self,
        paths: &ManagedPaths,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        if !self.android {
            return Ok(());
        }
        let Some(list) = env::var_os(crate::phone_fonts::LIST_VARIABLE) else {
            eprintln!("pd2-launcher: the app sent no list of the phone's fonts");
            return Ok(());
        };
        let text = fs::read_to_string(&list).unwrap_or_default();
        let (fonts, unreadable) =
            crate::phone_fonts::from_list(&text, crate::phone_fonts::family_name);
        for line in crate::phone_fonts::log_lines(&fonts) {
            eprintln!("{line}");
        }
        if !unreadable.is_empty() {
            eprintln!(
                "pd2-launcher: phone fonts without a readable name, not used: {}",
                unreadable.join("; ")
            );
        }
        let Some((file, marker)) = crate::phone_fonts::import_file(&fonts) else {
            return Ok(());
        };
        if prefix_system_reg(paths)
            .is_some_and(|system_reg| crate::phone_fonts::applied(&system_reg, &marker))
        {
            return Ok(());
        }
        let name = OsStr::new("phone-fonts.reg");
        let remove = || {
            if let Ok(old) = paths
                .directory()
                .open_beneath(Path::new(name), OFlags::RDONLY)
            {
                let _ = paths.directory().remove_open_file_bounded(name, &old);
            }
        };
        remove();
        let mut written = paths
            .directory()
            .create_file(name)
            .map_err(|_| AdapterFailure::here())?;
        written
            .write_all(&file)
            .map_err(|_| AdapterFailure::here())?;
        drop(written);
        let windows = format!(
            "Z:{}",
            paths
                .root()
                .join(name)
                .display()
                .to_string()
                .replace('/', "\\")
        );
        let result = self.run_wine(WineInvocation {
            paths,
            runtime: self.installed_runtime(paths),
            working_directory: paths.root(),
            program: WineProgram::Builtin(OsString::from("reg")),
            verified_argument: None,
            arguments: vec![OsString::from("import"), OsString::from(windows)],
            profile: WineProfile::Install,
            timeout: RUNTIME_TIMEOUT,
            mutation_lease,
            control,
            ui_helper: None,
            stall: None,
            accepted_exit_codes: &[],
        });
        remove();
        match &result {
            Ok(()) => eprintln!("pd2-launcher: the phone's fonts are set in Wine"),
            Err(_) => eprintln!("pd2-launcher: the phone's fonts were not set in Wine"),
        }
        result
    }

    /// Whether this job's Wine download source is custom. It
    /// selects the runtime exactly as the job's audit does.
    pub(crate) fn with_custom_wine(mut self, custom_wine: bool) -> Self {
        self.custom_wine = custom_wine;
        self
    }

    /// The runtime later stages execute: the one the preceding audit
    /// accepted for this installation and its configured Wine source (on
    /// the Android host the app's, as the audit selects it there).
    fn installed_runtime(&self, paths: &ManagedPaths) -> InstalledRuntime {
        if self.android {
            return InstalledRuntime::App;
        }
        InstalledRuntime::select(paths.directory(), self.custom_wine)
    }

    /// Install `gecko` into the prefix and record its provenance last: the
    /// pinned receipt after the pinned `xul.dll` identity is proven, or the
    /// custom-unverified receipt. The other kind of receipt is then removed.
    fn install_gecko(
        &self,
        paths: &ManagedPaths,
        runtime: InstalledRuntime,
        gecko: &VerifiedArtifact,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        self.run_wine(WineInvocation {
            paths,
            runtime,
            working_directory: paths.root(),
            program: WineProgram::Builtin(OsString::from("msiexec")),
            verified_argument: Some(gecko),
            arguments: vec![
                OsString::from("/i"),
                OsString::from(format!("/proc/self/fd/{CHILD_PROGRAM_FD}")),
                OsString::from("/qn"),
            ],
            profile: WineProfile::Install,
            timeout: RUNTIME_TIMEOUT,
            mutation_lease,
            control,
            ui_helper: None,
            stall: None,
            accepted_exit_codes: &[],
        })?;
        let prefix = paths
            .directory()
            .open_directory(Path::new("prefix"))
            .map_err(|_| AdapterFailure::here())?;
        if let Some(url) = gecko.custom_source() {
            // A custom Gecko build has no pinned xul.dll identity; record its
            // unverified provenance instead of the pinned receipt.
            write_custom_receipt_at(
                &prefix,
                CUSTOM_GECKO_RECEIPT,
                &CustomSourceReceipt::new(DownloadArtifact::Gecko, url, gecko.content_sha256()),
            )?;
            return remove_receipt_at(&prefix, GECKO_RECEIPT);
        }
        validate_file_at(
            &prefix,
            Path::new("drive_c/windows/syswow64/gecko/2.47.4/wine_gecko/xul.dll"),
            71_766_016,
            GECKO_XUL_SHA256,
            false,
        )?;
        write_receipt_last_at(&prefix, GECKO_RECEIPT, GECKO_SHA256)?;
        remove_receipt_at(&prefix, CUSTOM_GECKO_RECEIPT)
    }

    /// Install Wine Mono from its MSI without any prompt, then record its
    /// provenance last once the Mono tree exists.
    fn install_mono(
        &self,
        paths: &ManagedPaths,
        runtime: InstalledRuntime,
        mono: &VerifiedArtifact,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        self.run_wine(WineInvocation {
            paths,
            runtime,
            working_directory: paths.root(),
            program: WineProgram::Builtin(OsString::from("msiexec")),
            verified_argument: Some(mono),
            arguments: vec![
                OsString::from("/i"),
                OsString::from(format!("/proc/self/fd/{CHILD_PROGRAM_FD}")),
                OsString::from("/qn"),
            ],
            profile: WineProfile::Install,
            timeout: RUNTIME_TIMEOUT,
            mutation_lease,
            control,
            ui_helper: None,
            stall: None,
            accepted_exit_codes: &[],
        })?;
        record_dependency_provenance(paths, PrefixDependency::WineMono, mono)
    }

    /// Install the .NET 6 Desktop Runtime (x86) the official PD2 launcher
    /// needs, without input or a restart, then record its provenance.
    fn install_dotnet_desktop(
        &self,
        paths: &ManagedPaths,
        runtime: InstalledRuntime,
        dotnet: &VerifiedArtifact,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        self.run_wine(WineInvocation {
            paths,
            runtime,
            working_directory: paths.root(),
            program: WineProgram::Verified(dotnet),
            verified_argument: None,
            arguments: dotnet_desktop_arguments(self.android),
            profile: WineProfile::Install,
            timeout: RUNTIME_TIMEOUT,
            mutation_lease,
            control,
            ui_helper: None,
            stall: None,
            // 3010 "restart required" is success for an unattended install; the
            // receipt still needs the pinned runtime folder.
            accepted_exit_codes: &[DOTNET_RESTART_REQUIRED_STATUS],
        })?;
        record_dependency_provenance(paths, PrefixDependency::DotnetDesktop, dotnet)
    }

    /// Wine updates an existing prefix by itself the first time a different
    /// Wine build runs in it (its `.update-timestamp` holds the mtime of the
    /// `wine.inf` that last updated it), and that update may open Wine's
    /// interactive Mono or Gecko installer. Before any other Launcher-run
    /// Wine process, the Launcher runs that update itself, with the same
    /// overrides that keep prefix creation free of those prompts.
    fn update_prefix_for_runtime(
        &self,
        paths: &ManagedPaths,
        runtime: InstalledRuntime,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        let Ok(prefix) = paths.directory().open_directory(Path::new("prefix")) else {
            return Ok(());
        };
        let inf = Path::new("runner")
            .join(runtime.directory())
            .join("share/wine/wine.inf");
        let Ok(inf) = paths.directory().open_file(&inf) else {
            // Without its wine.inf, Wine cannot update the prefix either.
            return Ok(());
        };
        let inf_mtime = inf.metadata().map_err(|_| AdapterFailure::here())?.mtime();
        let recorded = prefix
            .open_file(Path::new(".update-timestamp"))
            .ok()
            .and_then(|file| {
                let mut text = String::new();
                file.take(64).read_to_string(&mut text).ok()?;
                Some(text.trim().to_owned())
            });
        // "disable" (the Android app's prefix template): Wine never updates
        // the prefix by itself, and on the app's runtime neither does the
        // Launcher. For any other runtime (the desktop's) it is no timestamp
        // of that Wine, so the update runs.
        if (runtime == InstalledRuntime::App && recorded.as_deref() == Some("disable"))
            || recorded.and_then(|text| text.parse::<i64>().ok()) == Some(inf_mtime)
            || !initialized_prefix(&prefix)
        {
            return Ok(());
        }
        self.run_wine(WineInvocation {
            paths,
            runtime,
            working_directory: paths.root(),
            program: WineProgram::Builtin(OsString::from("wineboot")),
            verified_argument: None,
            arguments: vec![OsString::from("-u")],
            profile: WineProfile::PrefixInitialization,
            timeout: RUNTIME_TIMEOUT,
            mutation_lease,
            control,
            ui_helper: None,
            stall: None,
            accepted_exit_codes: &[],
        })
    }

    fn run_wine(&self, invocation: WineInvocation<'_>) -> Result<(), AdapterFailure> {
        self.run_wine_observing_helper(invocation, &HelperExit::default())
    }

    /// [`Self::run_wine`], recording the UI helper's own exit status in
    /// `helper_exit` so a caller can tell which reviewed outcome ended it.
    fn run_wine_observing_helper(
        &self,
        invocation: WineInvocation<'_>,
        helper_exit: &HelperExit,
    ) -> Result<(), AdapterFailure> {
        if invocation.profile != WineProfile::PrefixInitialization {
            self.update_prefix_for_runtime(
                invocation.paths,
                invocation.runtime,
                invocation.mutation_lease,
                invocation.control,
            )?;
        }
        let handles = RuntimeHandles::open(invocation.paths, invocation.runtime)?;
        // Named for the failure record by what runs, never by its arguments.
        let label = match &invocation.program {
            WineProgram::Builtin(program) => format!("wine {}", program.to_string_lossy()),
            WineProgram::Verified(artifact) | WineProgram::ManagedVerified(artifact) => {
                format!("wine {:?}", artifact.kind())
            }
        };
        let client_bootstrap = matches!(
            &invocation.program,
            WineProgram::Builtin(program) if program == ANDROID_CLIENT_INSTALLER
        );
        let mut managed_program = None;
        let (program, verified_object) = match (invocation.program, invocation.verified_argument) {
            (WineProgram::Builtin(program), None) => (program, None),
            (WineProgram::Builtin(program), Some(artifact)) => {
                (program, Some(open_verified_program(artifact)?))
            }
            (WineProgram::Verified(artifact), None) => (
                OsString::from(format!("/proc/self/fd/{CHILD_PROGRAM_FD}")),
                Some(open_verified_program(artifact)?),
            ),
            (WineProgram::ManagedVerified(artifact), None) => {
                let relative = artifact
                    .path()
                    .strip_prefix(invocation.paths.root())
                    .map_err(|_| AdapterFailure::here())?;
                // Verified through the same retained root the child names it
                // beneath, and re-proven by identity just before the spawn.
                let verified = open_verified_program_beneath(
                    invocation.paths.directory(),
                    relative,
                    artifact,
                )?;
                managed_program = Some((
                    relative.to_owned(),
                    verified.try_clone().map_err(|_| AdapterFailure::here())?,
                ));
                let mut anchored = PathBuf::from(format!("/proc/self/fd/{CHILD_MANAGED_ROOT_FD}"));
                anchored.push(relative);
                (anchored.into_os_string(), Some(verified))
            }
            (WineProgram::Verified(_) | WineProgram::ManagedVerified(_), Some(_)) => {
                return Err(AdapterFailure::here());
            }
        };
        // The Box64 line names the program: a descriptor's by what it runs.
        let shown = managed_program.as_ref().map_or_else(
            || {
                if program.as_encoded_bytes().starts_with(b"/proc/") {
                    label.trim_start_matches("wine ").to_owned()
                } else {
                    program.to_string_lossy().into_owned()
                }
            },
            |(relative, _)| relative.to_string_lossy().into_owned(),
        );
        let home = validated_directory_environment("HOME")?;
        let working = match invocation
            .working_directory
            .strip_prefix(invocation.paths.root())
        {
            Ok(relative) if relative.as_os_str().is_empty() => {
                invocation.paths.directory().try_clone()
            }
            Ok(relative) => invocation.paths.directory().open_directory(relative),
            // A game folder used in place lies outside the root (the
            // official launcher's ProjectD2): opened as the recorded folder.
            Err(_) => {
                let relative = invocation
                    .working_directory
                    .strip_prefix(invocation.paths.game())
                    .map_err(|_| AdapterFailure::here())?;
                invocation.paths.open_game().and_then(|game| {
                    if relative.as_os_str().is_empty() {
                        Ok(game)
                    } else {
                        game.open_directory(relative)
                    }
                })
            }
        }
        .map_err(|_| AdapterFailure::here())?;
        invocation
            .paths
            .directory()
            .ensure_directory(Path::new(crate::persistence::WINE_CACHE_DIRECTORY))
            .map_err(|_| AdapterFailure::here())?;
        // Wine can resolve the executable in an already-running server, where
        // /proc/self names that server rather than the new helper child. Keep
        // the sealed object open in this process until helper supervision ends.
        let helper_image = invocation
            .ui_helper
            .map(UiHelper::sealed_file)
            .transpose()?;
        let helper_copy = match (invocation.ui_helper, &helper_image) {
            (Some(kind), Some(helper)) if self.android => {
                Some(android_helper_copy(invocation.paths, kind, helper)?)
            }
            _ => None,
        };
        let helper_request = helper_image
            .as_ref()
            .map(|helper| {
                self.ui_helper_request(
                    helper,
                    helper_copy.as_ref().map(|(program, _)| program.clone()),
                    invocation.paths,
                    invocation.mutation_lease,
                    &handles,
                    &home,
                )
            })
            .transpose()?;
        // The pinned loader descriptor (`wine-preloader`, or `wine` for a
        // runtime without one) is the guest ELF; Host execution's executor
        // decides whether it is translated or executed directly.
        let mut request = ProcessRequest::guest(format!("/proc/self/fd/{CHILD_PRELOADER_FD}"))
            .with_arguments(loader_arguments(handles.runtime))
            .with_argument(program)
            .with_working_directory_handle(
                invocation.working_directory,
                working
                    .try_clone_file()
                    .map_err(|_| AdapterFailure::here())?,
            )
            .with_cleared_environment()
            .with_output_limit(PROCESS_OUTPUT_LIMIT)
            .with_detailed_output(
                self.detailed_logging && invocation.profile == WineProfile::OfficialLauncher,
            );
        use crate::wine_environment::{Kind, Setting};
        let mut settings = wine_settings(home);
        match invocation.profile {
            WineProfile::Install => {
                // Installers stay silent only with these overrides.
                settings.push(Setting::new(
                    "WINEDLLOVERRIDES",
                    WINE_DLL_OVERRIDES,
                    Kind::Required,
                ));
                settings.extend(fex_strict_settings_on(self.android));
                if client_bootstrap {
                    settings.extend(crate::box64::settings(
                        crate::box64::Run::Installer,
                        self.android,
                    ));
                }
            }
            WineProfile::PrefixInitialization => {
                // Creating the prefix must not run Wine's interactive Mono or
                // Gecko installers; both are installed silently afterwards.
                settings.push(Setting::new("WINEARCH", "win64", Kind::Required));
                settings.push(Setting::new(
                    "WINEDLLOVERRIDES",
                    PREFIX_INITIALIZATION_DLL_OVERRIDES,
                    Kind::Required,
                ));
                settings.extend(fex_strict_settings_on(self.android));
            }
            WineProfile::OfficialLauncher => {
                // The game's, with Android's.
                settings.push(Setting::new(
                    "WINEDLLOVERRIDES",
                    crate::process::game_dll_overrides(self.android),
                    Kind::Preference,
                ));
                // Its .NET runtime is multithreaded x86 code that relies on
                // x86 memory ordering (JIT-compiled code, the garbage
                // collector, the thread pool), and a game it starts inherits
                // this environment.
                settings.extend(fex_strict_settings_on(self.android));
                settings.extend(crate::box64::settings(
                    crate::box64::Run::OfficialLauncher {
                        game_strict_memory_order: self.game_strict_memory_order,
                    },
                    self.android,
                ));
                settings.extend(crate::wine_environment::detailed_settings(
                    self.detailed_logging,
                    crate::dev_build::SETUP_WINEDEBUG,
                ));
                // Only where its .NET runs translated (ARM64: FEX, or Box64
                // on the Android host); on x86-64 it runs natively and needs
                // neither.
                if self.translated_dotnet {
                    settings.push(Setting::new(
                        DOTNET_WRITE_XOR_EXECUTE,
                        "0",
                        Kind::Preference,
                    ));
                    settings.extend(
                        dotnet_tiered_compilation(self.android)
                            .iter()
                            .map(|(name, value)| Setting::new(*name, *value, Kind::Preference)),
                    );
                }
            }
        }
        let environment =
            crate::wine_environment::compose_on(env::vars_os(), &settings, self.android);
        if let Some(line) = crate::wine_environment::box64_line(&shown, &environment) {
            eprintln!("{line}");
        }
        for (name, value) in environment {
            request = request.with_environment(name, value);
        }
        // A program that starts in the game folder outside the Android
        // host's prefix sees it as C:\Program Files (x86)\Diablo II, not on
        // the root drive (`game_folder::wine_pwd`).
        if let Some(pwd) = crate::game_folder::wine_pwd(
            invocation.paths.root(),
            invocation.working_directory,
            invocation.paths.android(),
        ) {
            request = request.with_environment("PWD", pwd);
        }
        for argument in invocation.arguments {
            request = request.with_argument(argument);
        }
        request = request
            .with_inherited_file(
                invocation
                    .paths
                    .directory()
                    .try_clone_file()
                    .map_err(|_| AdapterFailure::here())?,
                CHILD_MANAGED_ROOT_FD,
            )
            .map_err(|_| AdapterFailure::here())?;
        request = request
            .with_inherited_file(handles.preloader, CHILD_PRELOADER_FD)
            .map_err(|_| AdapterFailure::here())?;
        request = request
            .with_inherited_file(handles.wine, CHILD_WINE_PROOF_FD)
            .map_err(|_| AdapterFailure::here())?;
        request = request
            .with_inherited_file(
                handles
                    .bin
                    .try_clone_file()
                    .map_err(|_| AdapterFailure::here())?,
                CHILD_RUNTIME_BIN_FD,
            )
            .map_err(|_| AdapterFailure::here())?;
        request = request
            .with_inherited_file(
                invocation
                    .mutation_lease
                    .try_clone()
                    .map_err(|_| AdapterFailure::here())?,
                CHILD_MUTATION_LEASE_FD,
            )
            .map_err(|_| AdapterFailure::here())?;
        if let Some(program) = verified_object {
            request = request
                .with_inherited_file(program, CHILD_PROGRAM_FD)
                .map_err(|_| AdapterFailure::here())?;
        }
        // One deadline bounds the child and the prefix becoming quiet.
        let deadline = Instant::now() + invocation.timeout;
        // The helper starts before the child and outlives it: the vendor
        // installer hands off to descendants and exits within seconds, while the dialogs the helper answers appear later.
        // It is stopped only after `wineserver -w` proves the prefix quiet.
        let mut helper = match helper_request {
            Some(request) => Some(
                helper_exit.observe(
                    self.host
                        .spawn(request)
                        .map_err(|_| AdapterFailure::here())?,
                    invocation
                        .ui_helper
                        .map_or("installer UI helper", UiHelper::label),
                ),
            ),
            None => None,
        };
        // The child executes a managed program by its name beneath the
        // root: that name must still be the verified, singly linked object.
        if let Some((relative, verified)) = &managed_program
            && require_named_program(invocation.paths.directory(), relative, verified).is_err()
        {
            stop_owned_process(&mut helper);
            let _ = self.quiesce_wine(
                invocation.paths,
                invocation.runtime,
                invocation.mutation_lease,
            );
            return Err(AdapterFailure::here());
        }
        let child = self.spawn_and_wait_accepting(
            &label,
            request,
            deadline,
            invocation.stall,
            invocation.control,
            &mut helper,
            invocation.profile == WineProfile::OfficialLauncher,
            invocation.accepted_exit_codes,
        );
        let cancellation_requested = invocation.control.is_cancelled();
        if child.is_err() || cancellation_requested {
            let helper_stopped = stop_owned_process(&mut helper);
            let quiesced = self.quiesce_wine(
                invocation.paths,
                invocation.runtime,
                invocation.mutation_lease,
            );
            if cancellation_requested {
                if child == Ok(ProcessWait::CancelledAndReaped)
                    && helper_stopped
                    && quiesced.is_ok()
                {
                    invocation.control.mark_cancelled_safe();
                } else {
                    invocation.control.mark_cancel_unproven();
                }
            }
            return Err(AdapterFailure::here());
        }
        let waited = self.run_wineserver_wait(
            invocation.paths,
            invocation.runtime,
            invocation.mutation_lease,
            "-w",
            deadline,
            invocation.control,
            &mut helper,
            false,
        );
        if invocation.control.is_cancelled() {
            let helper_stopped = stop_owned_process(&mut helper);
            let quiesced = self.quiesce_wine(
                invocation.paths,
                invocation.runtime,
                invocation.mutation_lease,
            );
            if waited == Ok(ProcessWait::CancelledAndReaped) && helper_stopped && quiesced.is_ok() {
                invocation.control.mark_cancelled_safe();
            } else {
                invocation.control.mark_cancel_unproven();
            }
            return Err(AdapterFailure::here());
        }
        if waited != Ok(ProcessWait::Succeeded) {
            // A timed-out or failed wait leaves no process (such as a dialog
            // nobody answered) running in the prefix.
            stop_owned_process(&mut helper);
            let _ = self.quiesce_wine(
                invocation.paths,
                invocation.runtime,
                invocation.mutation_lease,
            );
            return Err(AdapterFailure::here());
        }
        finish_helper(&mut helper)
    }

    fn ui_helper_request(
        &self,
        helper: &File,
        program: Option<OsString>,
        paths: &ManagedPaths,
        mutation_lease: &File,
        handles: &RuntimeHandles,
        home: &OsStr,
    ) -> Result<ProcessRequest, AdapterFailure> {
        let mut request = ProcessRequest::guest(format!("/proc/self/fd/{CHILD_PRELOADER_FD}"))
            .with_arguments(loader_arguments(handles.runtime))
            .with_argument(program.unwrap_or_else(|| {
                OsString::from(format!(
                    "/proc/{}/fd/{}",
                    std::process::id(),
                    helper.as_raw_fd()
                ))
            }))
            .with_working_directory_handle(
                paths.root(),
                paths
                    .directory()
                    .try_clone_file()
                    .map_err(|_| AdapterFailure::here())?,
            )
            .with_cleared_environment()
            .with_output_limit(PROCESS_OUTPUT_LIMIT);
        let mut settings = wine_settings(home);
        settings.push(crate::wine_environment::Setting::new(
            "WINEDLLOVERRIDES",
            WINE_DLL_OVERRIDES,
            crate::wine_environment::Kind::Required,
        ));
        settings.extend(fex_strict_settings_on(self.android));
        for (name, value) in
            crate::wine_environment::compose_on(env::vars_os(), &settings, self.android)
        {
            request = request.with_environment(name, value);
        }
        request = request
            .with_inherited_file(
                paths
                    .directory()
                    .try_clone_file()
                    .map_err(|_| AdapterFailure::here())?,
                CHILD_MANAGED_ROOT_FD,
            )
            .map_err(|_| AdapterFailure::here())?;
        request = request
            .with_inherited_file(
                handles
                    .preloader
                    .try_clone()
                    .map_err(|_| AdapterFailure::here())?,
                CHILD_PRELOADER_FD,
            )
            .map_err(|_| AdapterFailure::here())?;
        request = request
            .with_inherited_file(
                handles
                    .wine
                    .try_clone()
                    .map_err(|_| AdapterFailure::here())?,
                CHILD_WINE_PROOF_FD,
            )
            .map_err(|_| AdapterFailure::here())?;
        request = request
            .with_inherited_file(
                handles
                    .bin
                    .try_clone_file()
                    .map_err(|_| AdapterFailure::here())?,
                CHILD_RUNTIME_BIN_FD,
            )
            .map_err(|_| AdapterFailure::here())?;
        request = request
            .with_inherited_file(
                mutation_lease
                    .try_clone()
                    .map_err(|_| AdapterFailure::here())?,
                CHILD_MUTATION_LEASE_FD,
            )
            .map_err(|_| AdapterFailure::here())?;
        request
            .with_inherited_file(
                helper.try_clone().map_err(|_| AdapterFailure::here())?,
                CHILD_VENDOR_UI_HELPER_FD,
            )
            .map_err(|_| AdapterFailure::here())
    }

    fn quiesce_wine(
        &self,
        paths: &ManagedPaths,
        runtime: InstalledRuntime,
        mutation_lease: &File,
    ) -> Result<(), AdapterFailure> {
        let control = OperationControl::default();
        // `wineserver -k` exits nonzero when no server is running, which is
        // the common case once the stopped processes were its last clients.
        // Its exit status therefore proves nothing either way; it must only
        // run and be reaped. The prefix is proven quiet solely by `-w`.
        self.run_wineserver_wait(
            paths,
            runtime,
            mutation_lease,
            "-k",
            Instant::now() + RUNTIME_TIMEOUT,
            &control,
            &mut None,
            true,
        )
        .and_then(expect_wineserver_success)?;
        self.run_wineserver_wait(
            paths,
            runtime,
            mutation_lease,
            "-w",
            Instant::now() + RUNTIME_TIMEOUT,
            &control,
            &mut None,
            false,
        )
        .and_then(expect_wineserver_success)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_wineserver_wait(
        &self,
        paths: &ManagedPaths,
        runtime: InstalledRuntime,
        mutation_lease: &File,
        action: &'static str,
        deadline: Instant,
        control: &OperationControl,
        helper: &mut Option<Box<dyn SupervisedProcess>>,
        any_exit: bool,
    ) -> Result<ProcessWait, AdapterFailure> {
        let handles = RuntimeHandles::open(paths, runtime)?;
        let home = validated_directory_environment("HOME")?;
        let mut request = ProcessRequest::guest(format!("/proc/self/fd/{CHILD_WINESERVER_FD}"))
            .with_argument(action)
            .with_working_directory_handle(
                paths.root(),
                paths
                    .directory()
                    .try_clone_file()
                    .map_err(|_| AdapterFailure::here())?,
            )
            .with_cleared_environment()
            .with_output_limit(PROCESS_OUTPUT_LIMIT);
        for (name, value) in
            crate::wine_environment::compose_on(env::vars_os(), &wine_settings(home), self.android)
        {
            request = request.with_environment(name, value);
        }
        request = request
            .with_inherited_file(
                paths
                    .directory()
                    .try_clone_file()
                    .map_err(|_| AdapterFailure::here())?,
                CHILD_MANAGED_ROOT_FD,
            )
            .map_err(|_| AdapterFailure::here())?;
        request = request
            .with_inherited_file(handles.wineserver, CHILD_WINESERVER_FD)
            .map_err(|_| AdapterFailure::here())?;
        request = request
            .with_inherited_file(
                mutation_lease
                    .try_clone()
                    .map_err(|_| AdapterFailure::here())?,
                CHILD_MUTATION_LEASE_FD,
            )
            .map_err(|_| AdapterFailure::here())?;
        self.spawn_and_wait(request, deadline, control, helper, any_exit)
    }

    /// Run `request` until it exits, `deadline` passes, or `control` is
    /// cancelled, while supervising `helper`. A helper that finishes its
    /// reviewed sequence successfully is released; one that fails fails the
    /// child. A still-running helper is left to the caller.
    fn spawn_and_wait(
        &self,
        request: ProcessRequest,
        deadline: Instant,
        control: &OperationControl,
        helper: &mut Option<Box<dyn SupervisedProcess>>,
        any_exit: bool,
    ) -> Result<ProcessWait, AdapterFailure> {
        let label = crate::activity::process_label(request.executable(), request.arguments());
        self.spawn_and_wait_accepting(
            &label,
            request,
            deadline,
            None,
            control,
            helper,
            any_exit,
            &[],
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_and_wait_accepting(
        &self,
        label: &str,
        request: ProcessRequest,
        deadline: Instant,
        stall: Option<&DownloadStall>,
        control: &OperationControl,
        helper: &mut Option<Box<dyn SupervisedProcess>>,
        any_exit: bool,
        accepted_exit_codes: &[i32],
    ) -> Result<ProcessWait, AdapterFailure> {
        let mut process = Some(self.host.spawn(request).map_err(|_| {
            crate::activity::record_process(
                label,
                "spawn-failed",
                None,
                &crate::host_execution::ProcessOutput::default(),
            );
            AdapterFailure
        })?);
        // The stall clock counts from here, not from the setup before it.
        if let Some(stall) = stall {
            stall.arm();
        }
        loop {
            if control.is_cancelled() {
                let process_stopped = stop_owned_process(&mut process);
                let helper_stopped = stop_owned_process(helper);
                return if process_stopped && helper_stopped {
                    Ok(ProcessWait::CancelledAndReaped)
                } else {
                    control.mark_cancel_unproven();
                    Err(AdapterFailure::here())
                };
            }
            match process
                .as_mut()
                .ok_or_else(|| AdapterFailure::here())?
                .try_wait()
            {
                Ok(Some(exit))
                    if exit.success()
                        || any_exit
                        || exit
                            .code()
                            .is_some_and(|code| accepted_exit_codes.contains(&code)) =>
                {
                    return Ok(ProcessWait::Succeeded);
                }
                Ok(Some(exit)) => {
                    if let Some(process) = process.as_ref() {
                        crate::activity::record_process(
                            label,
                            "exited",
                            exit.code(),
                            &process.output(),
                        );
                    }
                    return Err(AdapterFailure::here());
                }
                Err(_) => {
                    if let Some(process) = process.as_ref() {
                        crate::activity::record_process(
                            label,
                            "wait-failed",
                            None,
                            &process.output(),
                        );
                    }
                    stop_owned_process(&mut process);
                    return Err(AdapterFailure::here());
                }
                Ok(None) if Instant::now() < deadline => {}
                Ok(None) => {
                    if let Some(process) = process.as_ref() {
                        crate::activity::record_process(
                            label,
                            "timed-out",
                            None,
                            &process.output(),
                        );
                    }
                    stop_owned_process(&mut process);
                    return Err(AdapterFailure::here());
                }
            }
            if let Some(helper_process) = helper.as_mut() {
                match helper_process.try_wait() {
                    Ok(Some(exit)) if exit.success() => *helper = None,
                    Ok(Some(exit)) => {
                        crate::activity::record_process(
                            helper_process.helper_label(),
                            "exited",
                            exit.code(),
                            &helper_process.output(),
                        );
                        *helper = None;
                        stop_owned_process(&mut process);
                        return Err(AdapterFailure::here());
                    }
                    Err(_) => {
                        stop_owned_process(&mut process);
                        return Err(AdapterFailure::here());
                    }
                    Ok(None) => {}
                }
            }
            // A download that stopped changing ends like a timeout, under its
            // own name in the activity log; after the helper's check, so an
            // error the helper saw in the same poll is the one reported.
            if let Some(stall) = stall
                && stall.is_stalled()
            {
                stall.mark_ended();
                if let Some(process) = process.as_ref() {
                    crate::activity::record_process(label, "stalled", None, &process.output());
                }
                stop_owned_process(&mut process);
                return Err(AdapterFailure::here());
            }
            thread::sleep(PROCESS_POLL_INTERVAL);
        }
    }
}

/// The sealed downloader helper saw a message box: Blizzard's downloader
/// reported an error (the helper prints its text).
const DOWNLOADER_HELPER_ERROR_DIALOG: i32 = 23;
/// The sealed vendor helper saw the installer's generic `Installer` error
/// dialog.
const VENDOR_HELPER_ERROR_DIALOG: i32 = 20;
/// The same dialog, and its text names a key: the installer rejected the
/// product key it was given.
const VENDOR_HELPER_KEY_REJECTED: i32 = 22;
/// The installer stayed at its main menu (its license or destination dialog
/// was declined) instead of installing.
const VENDOR_HELPER_INSTALLER_MENU: i32 = 23;

/// The last exit status a supervised UI helper reported, if any.
#[derive(Clone)]
struct HelperExit(Arc<AtomicI32>);

impl HelperExit {
    const NONE: i32 = i32::MIN;

    fn code(&self) -> Option<i32> {
        Some(self.0.load(Ordering::SeqCst)).filter(|code| *code != Self::NONE)
    }

    fn observe(
        &self,
        inner: Box<dyn SupervisedProcess>,
        label: &'static str,
    ) -> Box<dyn SupervisedProcess> {
        Box::new(ObservedHelper {
            inner,
            exit: self.clone(),
            label,
        })
    }

    fn record(&self, exit: &io::Result<Option<crate::host_execution::ProcessExit>>) {
        if let Ok(Some(exit)) = exit
            && let Some(code) = exit.code()
        {
            self.0.store(code, Ordering::SeqCst);
        }
    }
}

impl Default for HelperExit {
    fn default() -> Self {
        Self(Arc::new(AtomicI32::new(Self::NONE)))
    }
}

/// A helper process whose exit status is recorded as it is observed, and
/// which the activity log names by its kind ([`UiHelper::label`]).
struct ObservedHelper {
    inner: Box<dyn SupervisedProcess>,
    exit: HelperExit,
    label: &'static str,
}

impl SupervisedProcess for ObservedHelper {
    fn try_wait(&mut self) -> io::Result<Option<crate::host_execution::ProcessExit>> {
        let exit = self.inner.try_wait();
        self.exit.record(&exit);
        exit
    }

    fn terminate(&mut self) -> io::Result<()> {
        self.inner.terminate()
    }

    fn wait(&mut self) -> io::Result<crate::host_execution::ProcessExit> {
        let exit = self.inner.wait();
        if let Ok(exit) = exit {
            self.exit.record(&Ok(Some(exit)));
        }
        exit
    }

    fn output(&self) -> crate::host_execution::ProcessOutput {
        self.inner.output()
    }

    fn helper_label(&self) -> &'static str {
        self.label
    }
}

/// Grace for the kernel to report a helper's exit after the prefix went
/// quiet: its last Wine call can return before its process is reapable.
const HELPER_REAP_GRACE: Duration = Duration::from_secs(2);

/// After `wineserver -w` proved the prefix quiet, a UI helper must already
/// have exited successfully: it is itself a Wine client, so a quiet prefix
/// with a helper still running after a short reaping grace, or a failed
/// helper, is contradictory evidence.
fn finish_helper(helper: &mut Option<Box<dyn SupervisedProcess>>) -> Result<(), AdapterFailure> {
    finish_helper_within(helper, HELPER_REAP_GRACE)
}

fn finish_helper_within(
    helper: &mut Option<Box<dyn SupervisedProcess>>,
    grace: Duration,
) -> Result<(), AdapterFailure> {
    let Some(process) = helper.as_mut() else {
        return Ok(());
    };
    let deadline = Instant::now() + grace;
    let exited = loop {
        match process.try_wait() {
            Ok(None) if Instant::now() < deadline => thread::sleep(PROCESS_POLL_INTERVAL),
            other => break other,
        }
    };
    match exited {
        Ok(Some(exit)) if exit.success() => {
            *helper = None;
            Ok(())
        }
        Ok(Some(exit)) => {
            crate::activity::record_process(
                process.helper_label(),
                "exited",
                exit.code(),
                &process.output(),
            );
            *helper = None;
            Err(AdapterFailure::here())
        }
        Ok(None) | Err(_) => {
            crate::activity::record_process(
                process.helper_label(),
                "did-not-exit",
                None,
                &process.output(),
            );
            stop_owned_process(helper);
            Err(AdapterFailure::here())
        }
    }
}

/// The Launcher-owned `XDG_CACHE_HOME`, beneath the inherited managed root.
fn wine_cache_home() -> String {
    format!(
        "/proc/self/fd/{CHILD_MANAGED_ROOT_FD}/{}",
        crate::persistence::WINE_CACHE_DIRECTORY
    )
}

fn stop_owned_process(process: &mut Option<Box<dyn SupervisedProcess>>) -> bool {
    let Some(mut process) = process.take() else {
        return true;
    };
    let terminated = process.terminate().is_ok();
    let reaped = process.wait().is_ok();
    terminated && reaped
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessWait {
    Succeeded,
    CancelledAndReaped,
}

fn expect_wineserver_success(outcome: ProcessWait) -> Result<(), AdapterFailure> {
    match outcome {
        ProcessWait::Succeeded => Ok(()),
        ProcessWait::CancelledAndReaped => Err(AdapterFailure::here()),
    }
}

struct WineInvocation<'a> {
    paths: &'a ManagedPaths,
    /// The validated runtime this invocation executes.
    runtime: InstalledRuntime,
    working_directory: &'a Path,
    program: WineProgram<'a>,
    verified_argument: Option<&'a VerifiedArtifact>,
    arguments: Vec<OsString>,
    profile: WineProfile,
    timeout: Duration,
    mutation_lease: &'a File,
    control: &'a OperationControl,
    /// The sealed helper that answers this program's reviewed dialogs.
    ui_helper: Option<UiHelper>,
    /// Unix exit statuses of the child accepted besides success.
    accepted_exit_codes: &'static [i32],
    /// Ends the child early once its download stopped changing.
    stall: Option<&'a DownloadStall>,
}

/// Environment and exit policy of one Wine invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WineProfile {
    /// A Launcher installation step: DirectSound disabled, the program must
    /// exit successfully.
    Install,
    /// `wineboot` creating the prefix: WoW64 architecture, and Mono and
    /// Gecko disabled so Wine does not prompt to install them.
    PrefixInitialization,
    /// The official PD2 launcher, with the game launch profile. Its exit
    /// status is the user's choice, not evidence; the prefix must still go
    /// quiet.
    OfficialLauncher,
}

/// A frozen, sealed Win32 helper started in the same prefix as a program
/// whose silent mode still leaves reviewed dialogs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UiHelper {
    /// Blizzard installer consent and destination dialogs.
    Vendor,
    /// The Blizzard Downloader wrapper's completed download.
    Downloader,
}

impl UiHelper {
    /// Its name in the activity log; the report reads its exit codes by it.
    fn label(self) -> &'static str {
        match self {
            Self::Vendor => "installer UI helper",
            Self::Downloader => "downloader UI helper",
        }
    }

    /// The file name of its copy on the Android host
    /// ([`android_helper_copy`]).
    fn file_name(self) -> &'static str {
        match self {
            Self::Vendor => "pd2launcher-vendor-ui-helper.exe",
            Self::Downloader => "pd2launcher-downloader-ui-helper.exe",
        }
    }

    fn sha256(self) -> &'static str {
        match self {
            Self::Vendor => VENDOR_UI_HELPER_SHA256,
            Self::Downloader => DOWNLOADER_UI_HELPER_SHA256,
        }
    }

    fn sealed_file(self) -> Result<File, AdapterFailure> {
        match self {
            Self::Vendor => sealed_helper_file(
                VENDOR_UI_HELPER_XZ_HEX,
                VENDOR_UI_HELPER_SIZE,
                VENDOR_UI_HELPER_SHA256,
                "pd2launcher-vendor-ui-helper",
            ),
            Self::Downloader => sealed_helper_file(
                DOWNLOADER_UI_HELPER_XZ_HEX,
                DOWNLOADER_UI_HELPER_SIZE,
                DOWNLOADER_UI_HELPER_SHA256,
                "pd2launcher-downloader-ui-helper",
            ),
        }
    }
}

enum WineProgram<'a> {
    Builtin(OsString),
    Verified(&'a VerifiedArtifact),
    ManagedVerified(&'a VerifiedArtifact),
}

/// The sealed OpenGL probe (`tools/gl-probe-helper.c`), verified against
/// its frozen identity.
pub(crate) fn gl_probe_helper() -> Option<File> {
    sealed_helper_file(
        GL_PROBE_HELPER_XZ_HEX,
        GL_PROBE_HELPER_SIZE,
        GL_PROBE_HELPER_SHA256,
        "pd2launcher-gl-probe-helper",
    )
    .ok()
}

/// Copies the Android app's prefix template `template` (PLAIN,
/// `runtime::APP_PREFIX_TEMPLATE`, or GOLDEN) to `prefix` in the managed
/// root `root`: folders, files with their modes,
/// and links as they are (most of its files are relative links into the
/// app's Wine). The copy is made under a temporary name, every file and
/// folder of it is synced, and only then is it renamed into place, so a
/// stop or a crash part way leaves no prefix, and never a short hive under
/// the prefix's name. A folder gets its mode after its entries, so a
/// template folder without the owner's write is copied too.
fn copy_app_prefix_template(
    root: &Path,
    template: crate::runtime::PrefixTemplate,
) -> io::Result<()> {
    fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
        fs::create_dir(to)?;
        // Opened while the owner can write it: its mode comes last.
        let folder = File::open(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let target = to.join(entry.file_name());
            if kind.is_symlink() {
                std::os::unix::fs::symlink(fs::read_link(entry.path())?, &target)?;
            } else if kind.is_dir() {
                copy_tree(&entry.path(), &target)?;
            } else if kind.is_file() {
                fs::copy(entry.path(), &target)?;
                File::open(&target)?.sync_all()?;
            }
        }
        folder.set_permissions(fs::metadata(from)?.permissions())?;
        folder.sync_all()
    }
    let template = root.join(template.directory());
    if !template.join("system.reg").is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the app's prefix template is not there",
        ));
    }
    let partial = OsStr::new(".prefix.partial");
    // A copy cut short before, whose folders may lack the owner's write.
    let directory = Directory::open(root)?;
    match directory.remove_owned_tree(partial) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    copy_tree(&template, &root.join(partial))?;
    fs::rename(root.join(partial), root.join("prefix"))?;
    // The rename, synced too.
    directory.sync()
}

/// The link to the Android app's .NET in the prefix
/// (`runtime::APP_DOTNET_LINK`), made when it is not there, and made again
/// when it leads to another target (one an earlier release made, should
/// `runtime::APP_DOTNET_LINK_TARGET` change). Only a link is replaced: a
/// real file or folder at its place is kept.
fn link_app_dotnet(root: &Path) -> io::Result<()> {
    let link = root.join(crate::runtime::APP_DOTNET_LINK);
    let target = Path::new(crate::runtime::APP_DOTNET_LINK_TARGET);
    match fs::symlink_metadata(&link) {
        Ok(metadata) if !metadata.file_type().is_symlink() => return Ok(()),
        Ok(_) => {
            if fs::read_link(&link)? == target {
                return Ok(());
            }
            // The link goes, never what it leads to.
            fs::remove_file(&link)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Some(folder) = link.parent() {
                fs::create_dir_all(folder)?;
            }
        }
        Err(error) => return Err(error),
    }
    std::os::unix::fs::symlink(target, &link)
}

/// Puts the sealed XInput bridge (`tools/xinput-bridge.c`) in the prefix as
/// the game's `xinput1_4.dll` unless it is there already, replacing Wine's
/// own. The Android host only; it needs `xinput1_4=n`.
pub(crate) fn install_xinput_bridge(root: &Directory) -> io::Result<()> {
    let mut bridge = Vec::with_capacity(XINPUT_BRIDGE_SIZE);
    sealed_helper_file(
        XINPUT_BRIDGE_XZ_HEX,
        XINPUT_BRIDGE_SIZE,
        XINPUT_BRIDGE_SHA256,
        "pd2launcher-xinput-bridge",
    )
    .map_err(|_| io::Error::other("the sealed XInput bridge did not verify"))?
    .read_to_end(&mut bridge)?;
    let path = Path::new(XINPUT_BRIDGE_PATH);
    let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::other("no XInput bridge path"));
    };
    let folder = root.open_directory(folder)?;
    let mut installed = Vec::new();
    match folder.open_file(Path::new(name)) {
        Ok(file) => {
            file.take(XINPUT_BRIDGE_SIZE as u64 + 1)
                .read_to_end(&mut installed)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if installed == bridge {
        return Ok(());
    }
    folder.atomic_write(name, &bridge)
}

fn sealed_helper_file(
    encoded: &str,
    size: usize,
    sha256: &str,
    name: &str,
) -> Result<File, AdapterFailure> {
    let compressed = decode_static_hex(encoded)?;
    let mut helper = Vec::with_capacity(size);
    XzDecoder::new(compressed.as_slice())
        .take(size as u64 + 1)
        .read_to_end(&mut helper)
        .map_err(|_| AdapterFailure::here())?;
    if helper.len() != size || hex(&Sha256::digest(&helper)) != sha256 {
        return Err(AdapterFailure::here());
    }
    let descriptor = memfd_create(name, MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)
        .map_err(|_| AdapterFailure::here())?;
    let mut file = File::from(descriptor);
    file.write_all(&helper)
        .map_err(|_| AdapterFailure::here())?;
    file.flush().map_err(|_| AdapterFailure::here())?;
    fcntl_add_seals(
        &file,
        SealFlags::SEAL | SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE,
    )
    .map_err(|_| AdapterFailure::here())?;
    file.rewind().map_err(|_| AdapterFailure::here())?;
    Ok(file)
}

fn decode_static_hex(encoded: &str) -> Result<Vec<u8>, AdapterFailure> {
    let digits = encoded
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    if digits.len() % 2 != 0 || digits.len() > 16 * 1024 {
        return Err(AdapterFailure::here());
    }
    let mut decoded = Vec::with_capacity(digits.len() / 2);
    for pair in digits.as_chunks::<2>().0 {
        let high = decode_hex_nibble(pair[0]).ok_or_else(|| AdapterFailure::here())?;
        let low = decode_hex_nibble(pair[1]).ok_or_else(|| AdapterFailure::here())?;
        decoded.push((high << 4) | low);
    }
    Ok(decoded)
}

fn decode_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// PD2's installer's file name on the Android host ([`android_client_installer`]).
const ANDROID_CLIENT_INSTALLER_NAME: &str = "ProjectD2Installer.exe";

/// Where the Android host runs PD2's installer from, as Windows names it: its
/// checked copy in [`ANDROID_PROGRAM_COPIES`] (named
/// [`ANDROID_CLIENT_INSTALLER_NAME`]).
const ANDROID_CLIENT_INSTALLER: &str = r"C:\pd2launcher-programs\ProjectD2Installer.exe";

/// PD2's installer, copied from its verified artifact to
/// [`ANDROID_CLIENT_INSTALLER`] and checked again there; returns the copy's
/// Windows path and the guard that removes it. Under Box64 its Inno Setup
/// program and the `.tmp` copy that one runs fail with an access violation
/// when the installer runs as a descriptor (`/proc/self/fd/N`), even with
/// Box64's exact flags; under its own file name they install. The copy is
/// kept out of [`WRAPPER_DOWNLOADS`], like every program copy, so a copy left
/// by a stopped app never counts as parts of a download.
fn android_client_installer(
    paths: &ManagedPaths,
    bootstrap: &VerifiedArtifact,
) -> Result<(OsString, RemoveOnDrop), AdapterFailure> {
    let mut source = open_verified_program(bootstrap)?;
    let sha256 = bootstrap
        .execution_sha256()
        .map_err(|_| AdapterFailure::here())?;
    android_program_copy(
        paths.directory(),
        paths.root(),
        &mut source,
        ANDROID_CLIENT_INSTALLER_NAME,
        sha256,
    )
}

/// On the Android host, the sealed UI helper is copied to
/// [`ANDROID_PROGRAM_COPIES`] under its own name, checked again there, and
/// started by that name: Wine on Android 17 cannot start it as
/// `/proc/<pid>/fd/N` (start.exe: "ShellExecuteEx failed: File not found"),
/// so a fresh download stopped at once. The copy is removed when the
/// returned guard drops.
fn android_helper_copy(
    paths: &ManagedPaths,
    helper: UiHelper,
    sealed: &File,
) -> Result<(OsString, RemoveOnDrop), AdapterFailure> {
    let mut source = sealed.try_clone().map_err(|_| AdapterFailure::here())?;
    source
        .seek(SeekFrom::Start(0))
        .map_err(|_| AdapterFailure::here())?;
    android_program_copy(
        paths.directory(),
        paths.root(),
        &mut source,
        helper.file_name(),
        helper.sha256(),
    )
}

/// The GL probe ([`gl_probe_helper`]) for the Android host: copied to
/// [`ANDROID_PROGRAM_COPIES`] of the managed root `root` (opened as
/// `directory`) and checked again there; returns its Windows path and the
/// guard that removes it. Wine on Android 17 cannot start the sealed probe
/// as `/proc/<pid>/fd/N`, as with the UI helpers ([`android_helper_copy`]),
/// so the probe found no renderer there.
pub(crate) fn android_gl_probe_copy(
    directory: &Directory,
    root: &Path,
    probe: &File,
) -> Result<(OsString, RemoveOnDrop), AdapterFailure> {
    let mut source = probe.try_clone().map_err(|_| AdapterFailure::here())?;
    source
        .seek(SeekFrom::Start(0))
        .map_err(|_| AdapterFailure::here())?;
    android_program_copy(
        directory,
        root,
        &mut source,
        "pd2launcher-gl-probe.exe",
        GL_PROBE_HELPER_SHA256,
    )
}

/// Where the Android host keeps the checked copies of programs it starts by
/// name ([`android_program_copy`]): a Launcher-owned folder of the prefix's
/// `C:` drive. It is not [`WRAPPER_DOWNLOADS`], whose files the download
/// watch counts (a helper there would look like parts of an earlier
/// download).
const ANDROID_PROGRAM_COPIES: &str = "prefix/drive_c/pd2launcher-programs";
const WINDOWS_ANDROID_PROGRAM_COPIES: &str = r"C:\pd2launcher-programs";

/// Copy `source` to [`ANDROID_PROGRAM_COPIES`] as `name`, check the copy's
/// SHA-256 against `sha256`, and return its Windows path with a guard that
/// removes it (and the folder, once empty) when it drops.
fn android_program_copy(
    directory: &Directory,
    root: &Path,
    source: &mut File,
    name: &str,
    sha256: &str,
) -> Result<(OsString, RemoveOnDrop), AdapterFailure> {
    directory
        .ensure_directory(Path::new(ANDROID_PROGRAM_COPIES))
        .map_err(|_| AdapterFailure::here())?;
    let copy = RemoveOnDrop(root.join(ANDROID_PROGRAM_COPIES).join(name));
    let _ = fs::remove_file(&copy.0);
    let mut out = File::create(&copy.0).map_err(|_| AdapterFailure::here())?;
    io::copy(source, &mut out).map_err(|_| AdapterFailure::here())?;
    out.sync_all().map_err(|_| AdapterFailure::here())?;
    drop(out);
    let mut written = File::open(&copy.0).map_err(|_| AdapterFailure::here())?;
    if sha256_open_file(&mut written)? != sha256 {
        return Err(AdapterFailure::here());
    }
    let program = OsString::from(format!("{WINDOWS_ANDROID_PROGRAM_COPIES}\\{name}"));
    Ok((program, copy))
}

/// A program copy removed when this drops, with its folder once empty.
pub(crate) struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        if let Some(folder) = self.0.parent() {
            let _ = fs::remove_dir(folder);
        }
    }
}

/// What older releases put in the Android app's prefix for Wine Gecko and
/// Wine Mono, below `prefix`: the Launcher's receipts, Wine Mono's folder,
/// and the folders that hold each version of Gecko.
const ANDROID_RETIRED_RECEIPTS: [&str; 4] = [
    GECKO_RECEIPT,
    CUSTOM_GECKO_RECEIPT,
    MONO_RECEIPT,
    CUSTOM_MONO_RECEIPT,
];
const ANDROID_RETIRED_MONO: (&str, &str) = ("drive_c/windows", "mono");
const ANDROID_RETIRED_GECKO_PARENTS: [&str; 2] = [
    "drive_c/windows/syswow64/gecko",
    "drive_c/windows/system32/gecko",
];

/// Removes Wine Gecko and Wine Mono from the Android app's prefix (`prefix`,
/// opened beneath the managed root): no program the Launcher runs there
/// needs them, and every Wine process there has mscoree and mshtml off. The
/// Launcher's receipts for them go first, then Wine Mono's folder and each
/// Gecko version folder (a name of digits and dots; Wine's own `plugin`
/// folder stays). Nothing else is touched, links are never followed, and a
/// stop part way leaves only parts of them, which the next call removes.
/// Whether anything was removed. The caller holds the installation
/// exclusively.
pub(crate) fn remove_wine_gecko_and_mono(prefix: &Directory) -> io::Result<bool> {
    let mut removed = false;
    for name in ANDROID_RETIRED_RECEIPTS {
        match prefix.open_file(Path::new(name)) {
            Ok(file) => {
                prefix.remove_open_file_bounded(OsStr::new(name), &file)?;
                removed = true;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let (parent, mono) = ANDROID_RETIRED_MONO;
    removed |= remove_tree_if_present(prefix, parent, |name| name == mono)?;
    for parent in ANDROID_RETIRED_GECKO_PARENTS {
        removed |= remove_tree_if_present(prefix, parent, |name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'.')
        })?;
    }
    Ok(removed)
}

/// Removes each directory of `parent` (below `prefix`) whose name `matches`.
fn remove_tree_if_present(
    prefix: &Directory,
    parent: &str,
    matches: impl Fn(&str) -> bool,
) -> io::Result<bool> {
    let parent = match prefix.open_directory(Path::new(parent)) {
        Ok(parent) => parent,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let mut removed = false;
    for name in parent.names()? {
        if name.to_str().is_some_and(&matches) {
            parent.remove_owned_tree(&name)?;
            removed = true;
        }
    }
    Ok(removed)
}

fn open_verified_program(artifact: &VerifiedArtifact) -> Result<File, AdapterFailure> {
    let mut file = open(
        artifact.path(),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| AdapterFailure::here())?;
    let metadata = file.metadata().map_err(|_| AdapterFailure::here())?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(AdapterFailure::here());
    }
    let observed = sha256_open_file(&mut file)?;
    if observed
        != artifact
            .execution_sha256()
            .map_err(|_| AdapterFailure::here())?
    {
        return Err(AdapterFailure::here());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| AdapterFailure::here())?;
    Ok(file)
}

/// Open a Launcher-managed program beneath the retained managed root (no
/// link is followed at any component) and prove it is the verified object.
fn open_verified_program_beneath(
    root: &Directory,
    relative: &Path,
    artifact: &VerifiedArtifact,
) -> Result<File, AdapterFailure> {
    let mut file = root
        .open_beneath(relative, OFlags::RDONLY)
        .map_err(|_| AdapterFailure::here())?;
    let metadata = file.metadata().map_err(|_| AdapterFailure::here())?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(AdapterFailure::here());
    }
    if sha256_open_file(&mut file)?
        != artifact
            .execution_sha256()
            .map_err(|_| AdapterFailure::here())?
    {
        return Err(AdapterFailure::here());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| AdapterFailure::here())?;
    Ok(file)
}

/// Prove that `relative` beneath `root` still names exactly `verified`: the
/// same device and inode, singly linked.
fn require_named_program(
    root: &Directory,
    relative: &Path,
    verified: &File,
) -> Result<(), AdapterFailure> {
    let named = root
        .open_beneath(relative, OFlags::PATH)
        .map_err(|_| AdapterFailure::here())?;
    let named = rustix::fs::fstat(&named).map_err(|_| AdapterFailure::here())?;
    let verified = rustix::fs::fstat(verified).map_err(|_| AdapterFailure::here())?;
    if named.st_dev == verified.st_dev
        && named.st_ino == verified.st_ino
        && named.st_nlink == 1
        && FileType::from_raw_mode(named.st_mode) == FileType::RegularFile
    {
        Ok(())
    } else {
        Err(AdapterFailure::here())
    }
}

fn sha256_open_file(file: &mut File) -> Result<String, AdapterFailure> {
    sha256_open_file_until(file, &|| false)?.ok_or_else(|| AdapterFailure::here())
}

/// The SHA-256 of `file`, or `None` once `stop` says so between reads.
fn sha256_open_file_until(
    file: &mut File,
    stop: &dyn Fn() -> bool,
) -> Result<Option<String>, AdapterFailure> {
    file.seek(SeekFrom::Start(0))
        .map_err(|_| AdapterFailure::here())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if stop() {
            return Ok(None);
        }
        let read = file.read(&mut buffer).map_err(|_| AdapterFailure::here())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Some(hex(&hasher.finalize())))
}

fn managed_complete_payload_directory(
    artifact: &VerifiedArtifact,
    paths: &ManagedPaths,
) -> Result<Option<PathBuf>, AdapterFailure> {
    managed_complete_payload_directory_until(artifact, paths, &|| false)
}

/// The same, ended between reads once `stop` says so (a failure).
fn managed_complete_payload_directory_until(
    artifact: &VerifiedArtifact,
    paths: &ManagedPaths,
    stop: &dyn Fn() -> bool,
) -> Result<Option<PathBuf>, AdapterFailure> {
    let Some(identity) = complete_payload_identity(artifact.kind()) else {
        return Ok(None);
    };
    let directory = paths.cache.join(identity.cache_directory);
    if artifact.path() != directory.join(PAYLOAD_INSTALLER) {
        return Ok(None);
    }
    let opened = Directory::open(&directory).map_err(|_| AdapterFailure::here())?;
    check_complete_payload_directory_until(&opened, identity, false, stop)
        .map_err(|_| AdapterFailure::here())?;
    Ok(Some(directory))
}

/// .NET's W^X writes generated code through a second mapping of the memory
/// it executes, which FEX's code-change tracking can miss. With it on, the
/// official launcher's .NET runtime crashes (`c0000005`) under FEX; tiered
/// compilation must not replace code either ([`dotnet_tiered_compilation`]).
const DOTNET_WRITE_XOR_EXECUTE: &str = "DOTNET_EnableWriteXorExecute";
/// Tiered compilation later replaces a method's quick code and repoints its
/// entry jumps while other threads run it; under FEX on ARM64 the official
/// launcher then jumps into unmapped code (an execute fault) as soon as
/// its filter list loads. With it off, each method is compiled once.
const DOTNET_TIERED_COMPILATION: &str = "DOTNET_TieredCompilation";
/// Call counting is what makes tiered compilation compile a method again,
/// with full optimization: without it, and without on-stack replacement
/// (which .NET 6 on x86 does not have), no method is compiled again.
const DOTNET_TIERED_CALL_COUNTING: &str = "DOTNET_TC_CallCounting";
/// The ARM64 row runs the official launcher's x86 .NET through FEX, and the
/// Android host through Box64.
const TRANSLATED_DOTNET: bool = cfg!(target_arch = "aarch64");

/// The tiered compilation settings of the official launcher's translated
/// .NET (its .NET 6 for x86). Under FEX (the desktop's ARM64 row) tiered
/// compilation is off. On the Android host (Box64) it is on without call
/// counting: a method without precompiled code is still compiled only
/// once, by the quick JIT unless it has a loop (as with tiered compilation
/// off), and its code is never replaced, so its entry jump does not change
/// again while other threads run it. The launcher's window then showed
/// 1 to 2 s sooner on a slow phone than with tiered compilation off, when
/// every such method is compiled with full optimization.
fn dotnet_tiered_compilation(android: bool) -> &'static [(&'static str, &'static str)] {
    if android {
        &[
            (DOTNET_TIERED_COMPILATION, "1"),
            (DOTNET_TIERED_CALL_COUNTING, "0"),
        ]
    } else {
        &[(DOTNET_TIERED_COMPILATION, "0")]
    }
}

/// The WPF value that draws the official PD2 launcher in software for the
/// prefix's user. With WPF's hardware rendering its window stays black
/// under FEX on ARM64.
const WPF_GRAPHICS_KEY: &str = r"HKCU\Software\Microsoft\Avalon.Graphics";
const WPF_SOFTWARE_RENDERING_VALUE: &str = "DisableHWAcceleration";
/// `reg delete` exits 1 when the value is already absent.
const REG_VALUE_ABSENT_STATUS: i32 = 1;
/// The Android app's prefixes draw OpenGL through GLX: Wine's X11 driver
/// with `UseEGL` "N" (Wine 11 takes EGL by default). With the app's Mesa
/// (Zink on Xvfb, Vulkan's WSI copying each frame), EGL's way to the screen
/// drew 43.6 frames a second on a Mali-G715 where GLX drew 55.2 to 55.4.
const X11_DRIVER_KEY: &str = r"HKCU\Software\Wine\X11 Driver";
const X11_USE_EGL_VALUE: &str = "UseEGL";

/// `reg` arguments that turn WPF software rendering on (`reg add` of
/// `DisableHWAcceleration=1`) or off (`reg delete` of the value).
fn wpf_software_rendering_arguments(enabled: bool) -> Vec<OsString> {
    let arguments: &[&str] = if enabled {
        &[
            "add",
            WPF_GRAPHICS_KEY,
            "/v",
            WPF_SOFTWARE_RENDERING_VALUE,
            "/t",
            "REG_DWORD",
            "/d",
            "1",
            "/f",
        ]
    } else {
        &[
            "delete",
            WPF_GRAPHICS_KEY,
            "/v",
            WPF_SOFTWARE_RENDERING_VALUE,
            "/f",
        ]
    };
    arguments.iter().map(OsString::from).collect()
}

/// `reg` setting the X11 driver's `UseEGL` to "N" ([`X11_DRIVER_KEY`]).
fn x11_glx_arguments() -> Vec<OsString> {
    [
        "add",
        X11_DRIVER_KEY,
        "/v",
        X11_USE_EGL_VALUE,
        "/t",
        "REG_SZ",
        "/d",
        "N",
        "/f",
    ]
    .iter()
    .map(OsString::from)
    .collect()
}

/// The text of the prefix's `user.reg`, the registry of the prefix's user
/// as its last wineserver saved it. `None` when it cannot be read.
fn prefix_user_reg(paths: &ManagedPaths) -> Option<String> {
    let mut user_reg = Vec::new();
    paths
        .directory()
        .open_beneath(Path::new("prefix/user.reg"), OFlags::RDONLY)
        .and_then(|mut file| file.read_to_end(&mut user_reg))
        .ok()?;
    Some(String::from_utf8_lossy(&user_reg).into_owned())
}

/// The text of the prefix's `system.reg`, as its last wineserver saved it.
/// `None` when it cannot be read.
fn prefix_system_reg(paths: &ManagedPaths) -> Option<String> {
    let mut system_reg = Vec::new();
    paths
        .directory()
        .open_beneath(Path::new("prefix/system.reg"), OFlags::RDONLY)
        .and_then(|mut file| file.read_to_end(&mut system_reg))
        .ok()?;
    Some(String::from_utf8_lossy(&system_reg).into_owned())
}

/// The values named `value` in the section of `key` of a prefix's
/// `user.reg` text: what follows `"<value>"=` on each such line. `key` is
/// written as Wine writes key names there, with doubled backslashes. Key
/// and value names are compared without their case, as Windows does, and
/// spaces at the start of a line do not count.
fn user_reg_values<'a>(user_reg: &'a str, key: &str, value: &str) -> Vec<&'a str> {
    let section = format!("{}]", key.to_ascii_lowercase());
    let name = format!(r#""{}"="#, value.to_ascii_lowercase());
    let mut in_section = false;
    let mut values = Vec::new();
    for line in user_reg.lines().map(str::trim_start) {
        if let Some(found) = line.strip_prefix('[') {
            in_section = found.to_ascii_lowercase().starts_with(&section);
        } else if in_section && line.to_ascii_lowercase().starts_with(&name) {
            values.push(line[name.len()..].trim_end());
        }
    }
    values
}

/// Whether the text of a prefix's `user.reg` already matches the WPF
/// software rendering setting: with `enabled`, its
/// `[Software\\Microsoft\\Avalon.Graphics]` section has
/// `"DisableHWAcceleration"=dword:00000001` and no other value of that
/// name; without, the section has no value of that name. Any other text is
/// no match, so Wine's `reg` sets the value.
fn wpf_software_rendering_matches(user_reg: &str, enabled: bool) -> bool {
    let values = user_reg_values(
        user_reg,
        r"Software\\Microsoft\\Avalon.Graphics",
        WPF_SOFTWARE_RENDERING_VALUE,
    );
    if enabled {
        !values.is_empty() && values.iter().all(|value| *value == "dword:00000001")
    } else {
        values.is_empty()
    }
}

/// Whether the text of a prefix's `user.reg` has the X11 driver's `UseEGL`
/// set to "N": `"UseEGL"="N"`, and no other value of that name, in its
/// `[Software\\Wine\\X11 Driver]` section.
fn x11_glx_set(user_reg: &str) -> bool {
    let values = user_reg_values(user_reg, r"Software\\Wine\\X11 Driver", X11_USE_EGL_VALUE);
    !values.is_empty() && values.iter().all(|value| *value == r#""N""#)
}

/// The Windows path of a folder used in place (`in_place_drive`), for the
/// import record: on the Android host (`android`) the `G:` drive is linked
/// to its parent first, so the game starts from there.
fn in_place_install_path(
    paths: &ManagedPaths,
    folder: &Path,
    android: bool,
) -> Result<String, AdapterFailure> {
    let install_path = crate::in_place_drive::windows_path(folder, android)
        .ok_or_else(|| AdapterFailure::here())?;
    if install_path.starts_with(r"G:\") {
        crate::in_place_drive::link(paths.directory(), folder)
            .map_err(|_| AdapterFailure::here())?;
    }
    eprintln!("pd2-launcher: the folder used in place is {install_path}");
    Ok(install_path)
}

/// The Android host's import: the app's checked copy `existing` (opened as
/// `source`, its identity proven) becomes the Managed game folder outside the
/// prefix by one rename (both are on the app's data partition), and the
/// prefix's link leads to it. An empty game folder (the setup made it) gives
/// way first; one with files refuses the import, and so does a copy that is
/// on another file system: nothing is copied or removed then.
fn adopt_android_import(
    paths: &ManagedPaths,
    existing: &crate::game_folder::ExistingInstall,
    source: &Directory,
) -> Result<(), AdapterFailure> {
    let path = existing.path();
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(AdapterFailure::here());
    };
    let parent = Directory::open(parent).map_err(|_| AdapterFailure::here())?;
    let root = paths.directory();
    let target = OsStr::new(crate::game_folder::ANDROID_GAME_RELATIVE);
    match root.open_directory(Path::new(target)) {
        Ok(folder) if folder.names().is_ok_and(|names| names.is_empty()) => root
            .remove_empty_directory(target)
            .map_err(|_| AdapterFailure::here())?,
        Ok(_) => {
            eprintln!("pd2-launcher: the game folder has files already; the import stops");
            return Err(AdapterFailure::here());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(AdapterFailure::here()),
    }
    parent
        .rename_directory_noreplace(name, source, root, target)
        .map_err(|error| {
            eprintln!("pd2-launcher: the copy did not move into the game folder: {error}");
            AdapterFailure::here()
        })?;
    crate::game_folder::link_game(root).map_err(|_| AdapterFailure::here())?;
    eprintln!("pd2-launcher: the imported copy is the game folder now");
    Ok(())
}

/// A folder used in place is reached through Wine's `Z:` drive, which must
/// map the host root in this prefix.
fn require_host_root_drive(paths: &ManagedPaths) -> Result<(), AdapterFailure> {
    let devices = paths
        .directory()
        .open_directory(Path::new("prefix/dosdevices"))
        .and_then(|devices| devices.try_clone_file())
        .map_err(|_| AdapterFailure::here())?;
    let target =
        rustix::fs::readlinkat(&devices, "z:", Vec::new()).map_err(|_| AdapterFailure::here())?;
    if target.as_bytes() == b"/" {
        Ok(())
    } else {
        Err(AdapterFailure::here())
    }
}

/// The .NET Desktop Runtime installer's arguments: on Android `/passive`,
/// its progress window without any input (it closes by itself), which the
/// app's live installer window shows during the game environment's last
/// part; elsewhere `/quiet`. Wine's msiexec draws nothing for `/qb-`, so
/// Gecko and Wine Mono stay `/qn`.
fn dotnet_desktop_arguments(android: bool) -> Vec<OsString> {
    let display = if android { "/passive" } else { "/quiet" };
    ["/install", display, "/norestart"]
        .into_iter()
        .map(OsString::from)
        .collect()
}

/// The components a new prefix is made with, or `None` when one it needs is
/// missing, before anything runs: on the desktop Gecko, Wine Mono and the
/// .NET Desktop Runtime, all three; on the Android host only the .NET
/// Desktop Runtime where the audit asks for it, and never Gecko or Wine
/// Mono, which no program the Launcher runs there needs.
fn new_prefix_components<'a>(
    gecko: Option<&'a VerifiedArtifact>,
    mono: Option<&'a VerifiedArtifact>,
    dotnet: Option<&'a VerifiedArtifact>,
    android: bool,
) -> Option<(
    Option<&'a VerifiedArtifact>,
    Option<&'a VerifiedArtifact>,
    Option<&'a VerifiedArtifact>,
)> {
    if android {
        return Some((None, None, dotnet));
    }
    match (gecko, mono, dotnet) {
        (Some(gecko), Some(mono), Some(dotnet)) => Some((Some(gecko), Some(mono), Some(dotnet))),
        _ => None,
    }
}

/// `wineboot`'s arguments when it makes a new prefix: `-u` on the desktop.
/// On the Android host none: Wine's own first start in the new prefix
/// (`wineboot --init`, before the requested program runs) makes it, with
/// this request's environment, and `wineboot -u` would make it all over
/// again, which under FEX doubles the time this part takes.
fn new_prefix_wineboot_arguments(android: bool) -> Vec<OsString> {
    if android {
        Vec::new()
    } else {
        vec![OsString::from("-u")]
    }
}

/// `reg add` arguments selecting the prefix-wide Windows version.
fn windows_version_arguments(version: &str) -> Vec<OsString> {
    [
        "add",
        r"HKCU\Software\Wine",
        "/v",
        "Version",
        "/d",
        version,
        "/f",
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

fn complete_payload_arguments(key: &str) -> Vec<OsString> {
    vec![
        OsString::from("--user=Player"),
        OsString::from(format!("--key1={key}")),
        OsString::from(format!("--destination={WINDOWS_GAME_DIRECTORY}")),
        OsString::from("--silent=true"),
        OsString::from("--noui=true"),
        OsString::from("--skippostlaunch=true"),
        OsString::from("--nopatchnotes=true"),
    ]
}

/// Blizzard Downloader 2.2 options (its popt table): `--saveas` names the
/// folder, so its "Browse for Folder" prompt never opens, and
/// `--autolaunch=0` keeps it from starting the downloaded, not yet verified
/// installer. The wrapper never receives a product key.
fn wrapper_arguments() -> Vec<OsString> {
    vec![
        OsString::from(format!("--saveas={WINDOWS_WRAPPER_DOWNLOADS}")),
        OsString::from("--autolaunch=0"),
    ]
}

impl ProductionInstallDriver {
    /// The Android app's prefix, while the audit rejects the runtime or the
    /// prefix, or the prefix was made from the other template before the
    /// game was there (only then does the setup come here): a new one is a
    /// copy of the app's prefix template `template` (made by the same Wine,
    /// so no `wineboot` runs on the phone; it carries `"UseEGL"="N"`), with
    /// the XInput bridge and the official launcher's WPF setting. The prefix
    /// gets GLX for OpenGL ([`X11_DRIVER_KEY`]) when its `user.reg` lacks
    /// it, the link to the app's .NET and, on the Android host's layout, the
    /// link to the game folder outside it
    /// ([`crate::game_folder::prepare_game_location`]). A prefix Wine never
    /// finished is made again; any other one is kept as it is.
    #[allow(clippy::too_many_arguments)]
    fn provision_app_prefix(
        &self,
        paths: &ManagedPaths,
        runtime: InstalledRuntime,
        new_prefix: bool,
        template: crate::runtime::PrefixTemplate,
        parts: &mut EnvironmentParts<'_>,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        if new_prefix {
            parts.start(EnvironmentPart::Windows);
            let root = paths.root();
            if paths
                .directory()
                .open_directory(Path::new("prefix"))
                .is_ok()
            {
                paths
                    .directory()
                    .remove_owned_tree(OsStr::new("prefix"))
                    .map_err(|_| AdapterFailure::here())?;
            }
            copy_app_prefix_template(root, template).map_err(|error| {
                eprintln!("pd2-launcher: the prefix template was not copied: {error}");
                AdapterFailure::here()
            })?;
            eprintln!(
                "pd2-launcher: the prefix is a copy of the {} template",
                template.mark()
            );
            install_xinput_bridge(paths.directory()).map_err(|_| AdapterFailure::here())?;
            if self.wpf_software_rendering {
                let _ = self.sync_wpf_software_rendering(paths, runtime, mutation_lease, control);
                if control.is_cancelled() {
                    return Err(AdapterFailure::here());
                }
            }
        }
        // A copy of the template has it, so no Wine runs for it; a kept prefix
        // gets it here only when the audit rejected the runtime or the
        // prefix. Without it the game still draws, through EGL.
        if self
            .sync_x11_glx(paths, runtime, mutation_lease, control)
            .is_err()
        {
            if control.is_cancelled() {
                return Err(AdapterFailure::here());
            }
            eprintln!("pd2-launcher: UseEGL=N was not set; OpenGL goes through EGL");
        }
        link_app_dotnet(paths.root()).map_err(|error| {
            eprintln!("pd2-launcher: the .NET link was not made: {error}");
            AdapterFailure::here()
        })?;
        crate::game_folder::prepare_game_location(paths.directory(), paths.android()).map_err(
            |error| {
                eprintln!("pd2-launcher: the game folder's link was not made: {error}");
                AdapterFailure::here()
            },
        )
    }

    /// Run a downloader wrapper to fetch its complete payload into
    /// [`WRAPPER_DOWNLOADS`], then verify the payload against its frozen
    /// identity exactly as a selected complete payload. Nothing the
    /// download produced runs before that. A payload that does not verify
    /// is kept for the downloader to resume on the next attempt.
    /// Blizzard's downloader for `wrapper`: its verified payload folder and
    /// installer. How it failed, when it did, is told apart for the stage
    /// and the player: a download that stopped changing for the stall limit
    /// ([`VendorFailure::DownloadStalled`], a connection problem), an error
    /// box its helper saw ([`VendorFailure::DownloaderErrorDialog`], whose
    /// text the helper prints), a download that ended unfinished or whose
    /// files differ from their identity ([`VendorFailure::PayloadIncomplete`],
    /// [`VendorFailure::PayloadRejected`], each recorded with the file and
    /// the check), and any other end of the downloader
    /// ([`VendorFailure::WrapperDownload`]).
    fn download_wrapper_payload(
        &self,
        paths: &ManagedPaths,
        wrapper: &VerifiedArtifact,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(PathBuf, VerifiedArtifact), VendorFailure> {
        let identity = complete_payload_identity(wrapper.kind()).ok_or(VendorFailure::Install)?;
        paths
            .directory()
            .ensure_directory(Path::new(WRAPPER_DOWNLOADS))
            .map_err(|_| VendorFailure::WrapperDownload)?;
        let stall = DownloadStall::default();
        let helper_exit = HelperExit::default();
        // The Blizzard Downloader reports no progress of its own; the
        // growth of its download folder is the only honest measure, and a
        // folder that stops changing while no data comes in ends it
        // ([`DOWNLOAD_STALL_LIMIT`], [`DOWNLOAD_ACTIVITY_BYTES`]).
        let _watcher = DownloadFolderWatcher::start(
            paths.root().join(WRAPPER_DOWNLOADS),
            identity.installer_size + identity.tome_size,
            match wrapper.kind() {
                ArtifactKind::ExpansionWrapper => WorkPhase::ExpansionDownload,
                _ => WorkPhase::BaseGameDownload,
            },
            control.clone(),
            GrowthClock::new(
                self.download_stall_limit,
                self.download_resume_limit,
                self.download_empty_limit,
                Instant::now(),
            ),
            stall.clone(),
            self.received_bytes.clone(),
        );
        // The wrapper re-reads its own image for the embedded download
        // description. A UI helper starts the prefix's wineserver first, and
        // a Windows process started then does not inherit a program-only
        // descriptor, so the wrapper is named beneath the inherited
        // managed-root descriptor; the verified object stays inherited as
        // proof.
        self.run_wine_observing_helper(
            WineInvocation {
                paths,
                runtime: self.installed_runtime(paths),
                working_directory: paths.root(),
                program: WineProgram::ManagedVerified(wrapper),
                verified_argument: None,
                arguments: wrapper_arguments(),
                profile: WineProfile::Install,
                timeout: INSTALL_TIMEOUT,
                mutation_lease,
                control,
                ui_helper: Some(UiHelper::Downloader),
                stall: Some(&stall),
                accepted_exit_codes: &[],
            },
            &helper_exit,
        )
        .map_err(|_| {
            if stall.ended_it() {
                VendorFailure::DownloadStalled
            } else if helper_exit.code() == Some(DOWNLOADER_HELPER_ERROR_DIALOG) {
                VendorFailure::DownloaderErrorDialog
            } else {
                VendorFailure::WrapperDownload
            }
        })?;
        let relative = Path::new(WRAPPER_DOWNLOADS).join(identity.source_directory);
        // What the downloader left: unfinished, or finished with other files.
        let checked = paths
            .directory()
            .open_directory(&relative)
            .map_err(|_| PayloadProblem::new(".", "missing"))
            .and_then(|payload| check_complete_payload_directory(&payload, identity, true));
        if let Err(problem) = checked {
            crate::activity::record_payload_problem(
                identity.source_directory,
                &problem.file,
                problem.check,
                problem.size,
            );
            if problem.incomplete() {
                return Err(VendorFailure::PayloadIncomplete);
            }
            // A finished file the downloader would keep on its next run:
            // removed, so the retry or Try again downloads it again.
            discard_payload_file(paths.directory(), &relative, &problem);
            return Err(VendorFailure::PayloadRejected);
        }
        let directory = paths.root().join(&relative);
        let installer = VerifiedArtifact {
            kind: wrapper.kind(),
            path: directory.join(PAYLOAD_INSTALLER),
            origin: wrapper.origin,
            observed_sha256: identity.installer_sha256.to_owned(),
            retained_receipt_sha256: Some(identity.installer_sha256.to_owned()),
            requested_url: wrapper.requested_url.clone(),
            final_url: wrapper.final_url.clone(),
            custom_source: None,
        };
        Ok((directory, installer))
    }

    /// Run the official PD2 launcher (`ProjectD2\\PD2Launcher.exe`, or the
    /// program [`Self::with_official_launcher`] names, with the player's
    /// start options) in the managed prefix with the game's environment, until
    /// it and everything it started (its updater, or a game it launched)
    /// have exited and the prefix is quiet. The caller holds the exclusive
    /// installation lease, which the Wine process family inherits.
    /// Cancellation closes it and stops the prefix.
    pub(crate) fn run_official_launcher(
        &self,
        managed_root: &Path,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        let paths = ManagedPaths::prepare_on(managed_root, self.android)
            .map_err(|_| AdapterFailure::here())?;
        // Its own folder in the game folder is its working directory.
        let folder = self.official_launcher.parent().unwrap_or(Path::new(""));
        let Some(program) = self.official_launcher.file_name().map(OsStr::to_owned) else {
            return Err(AdapterFailure::here());
        };
        let project = paths.game().join(folder);
        // It updates itself, so it has no pin; it must still be a regular
        // Windows executable inside the game folder.
        let mut launcher = paths
            .open_game()
            .and_then(|game| game.open_file(&self.official_launcher))
            .map_err(|_| AdapterFailure::here())?;
        let mut magic = [0_u8; 2];
        if !launcher.metadata().is_ok_and(|metadata| metadata.is_file())
            || launcher.read_exact(&mut magic).is_err()
            || &magic != b"MZ"
        {
            return Err(AdapterFailure::here());
        }
        drop(launcher);
        let runtime = self.installed_runtime(&paths);
        // A changed setting takes effect here, and a prefix without the
        // setting gets it. Best effort: if it fails the launcher
        // still opens, though its window may stay black.
        let _ = self.sync_wpf_software_rendering(&paths, runtime, mutation_lease, control);
        // Best effort too: without them, letters the Windows fonts lack show
        // as boxes.
        let _ = self.sync_phone_fonts(&paths, mutation_lease, control);
        // The game folder's drive (`in_place_drive`), as before the game.
        if let Some(folder) = crate::in_place_drive::folder(self.android, paths.game_folder())
            && let Err(error) = crate::in_place_drive::link(paths.directory(), &folder)
        {
            eprintln!("pd2-launcher: the G: drive was not linked: {error}");
        }
        // The prefix's link to the game folder outside it, as before the game.
        if paths.android()
            && let Err(error) = crate::game_folder::link_game(paths.directory())
        {
            eprintln!("pd2-launcher: the game folder's link was not made: {error}");
        }
        // It can start the game, whose D2GL loads the DLLs d2gl.json names:
        // as before a Play, they must be PD2's own.
        if self.android
            && let Err(error) = paths
                .open_game()
                .and_then(|game| crate::process::pin_d2gl_dlls(&game))
        {
            eprintln!(
                "pd2-launcher: official launcher not opened, d2gl.json may name other DLLs: {error}"
            );
            return Err(AdapterFailure::here());
        }
        if control.is_cancelled() {
            return Err(AdapterFailure::here());
        }
        self.run_wine(WineInvocation {
            paths: &paths,
            runtime,
            working_directory: &project,
            program: WineProgram::Builtin(program),
            verified_argument: None,
            arguments: self.official_launcher_arguments.clone(),
            profile: WineProfile::OfficialLauncher,
            timeout: OFFICIAL_LAUNCHER_TIMEOUT,
            mutation_lease,
            control,
            ui_helper: None,
            stall: None,
            accepted_exit_codes: &[],
        })
    }
}

/// The parts of one setup of the game environment: each is reported as it
/// starts, and how long it took goes to standard error (the engine log on
/// Android).
struct EnvironmentParts<'a> {
    control: &'a OperationControl,
    planned: Vec<EnvironmentPart>,
    running: Option<(EnvironmentPart, Instant)>,
}

impl EnvironmentParts<'_> {
    fn start(&mut self, part: EnvironmentPart) {
        self.finish();
        self.control.report(
            WorkProgress::for_phase(WorkPhase::PrefixInitialization)
                .with_environment(EnvironmentProgress::new(part, &self.planned)),
        );
        self.running = Some((part, Instant::now()));
    }

    fn finish(&mut self) {
        if let Some((part, since)) = self.running.take() {
            eprintln!(
                "pd2-launcher: game environment: {part:?} ended after {:.1} s",
                since.elapsed().as_secs_f64()
            );
        }
    }
}

impl Drop for EnvironmentParts<'_> {
    fn drop(&mut self) {
        self.finish();
    }
}

impl InstallDriver for ProductionInstallDriver {
    fn provision_runtime(
        &self,
        paths: &ManagedPaths,
        wine: Option<&VerifiedArtifact>,
        prefix_artifacts: PrefixArtifacts<'_>,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        let PrefixArtifacts {
            gecko,
            mono,
            dotnet,
            template,
        } = prefix_artifacts;
        if wine.is_some_and(|wine| wine.kind() != ArtifactKind::ManagedRuntime)
            || gecko.is_some_and(|gecko| gecko.kind() != ArtifactKind::Gecko)
            || mono.is_some_and(|mono| mono.kind() != ArtifactKind::WineMono)
            || dotnet.is_some_and(|dotnet| dotnet.kind() != ArtifactKind::DotnetDesktop)
        {
            return Err(AdapterFailure::here());
        }
        // The parts this setup runs, reported as each starts. A prefix that
        // is not there yet (or is made again below) is made in the first.
        let new_prefix = match paths.directory().open_directory(Path::new("prefix")) {
            Ok(prefix) => {
                abandoned_prefix(&prefix)
                    || crate::install::prefix_has_other_template(paths, template)
            }
            Err(error) => error.kind() == io::ErrorKind::NotFound,
        };
        let mut parts = EnvironmentParts {
            control,
            planned: [
                (wine.is_some() || new_prefix, EnvironmentPart::Windows),
                (gecko.is_some(), EnvironmentPart::WebComponents),
                (mono.is_some(), EnvironmentPart::Dotnet),
                (dotnet.is_some(), EnvironmentPart::LauncherSupport),
            ]
            .into_iter()
            .filter_map(|(runs, part)| runs.then_some(part))
            .collect(),
            running: None,
        };
        if wine.is_some() {
            parts.start(EnvironmentPart::Windows);
        }
        // Only the components the audit rejected are acquired and replaced;
        // an accepted runtime is used exactly as audited.
        let runtime = match wine {
            Some(wine) => ensure_runtime(paths, wine, control)?,
            None => self.installed_runtime(paths),
        };
        if runtime == InstalledRuntime::App {
            return self.provision_app_prefix(
                paths,
                runtime,
                new_prefix,
                template,
                &mut parts,
                mutation_lease,
                control,
            );
        }
        if gecko.is_none() && mono.is_none() && dotnet.is_none() {
            return Ok(());
        }
        // A prefix Wine never finished (its first wineboot failed) holds
        // nothing to keep: it is made again rather than refused. Any other
        // prefix that fails the check (a hive that can't be opened, a game
        // or saves inside) is refused below, never removed.
        if paths
            .directory()
            .open_directory(Path::new("prefix"))
            .is_ok_and(|prefix| abandoned_prefix(&prefix))
        {
            paths
                .directory()
                .remove_owned_tree(OsStr::new("prefix"))
                .map_err(|_| AdapterFailure::here())?;
        }
        match paths.directory().open_directory(Path::new("prefix")) {
            // An existing Launcher prefix is never recreated: each rejected
            // component is installed into it in place, keeping the game.
            Ok(prefix) => {
                if !initialized_prefix(&prefix) {
                    return Err(AdapterFailure::here());
                }
                if let Some(gecko) = gecko
                    && !gecko_installed(&prefix, gecko.custom_source().is_some())
                {
                    // Only a Gecko from a custom source, while the built-in
                    // Gecko is configured, is replaced in place.
                    if gecko.custom_source().is_some() || !custom_gecko_prefix(&prefix) {
                        return Err(AdapterFailure::here());
                    }
                    parts.start(EnvironmentPart::WebComponents);
                    self.install_gecko(paths, runtime, gecko, mutation_lease, control)?;
                }
                if let Some(mono) = mono
                    && !PrefixDependency::WineMono
                        .installed(&prefix, mono.custom_source().is_some())
                {
                    parts.start(EnvironmentPart::Dotnet);
                    self.install_mono(paths, runtime, mono, mutation_lease, control)?;
                }
                if let Some(dotnet) = dotnet
                    && !PrefixDependency::DotnetDesktop
                        .installed(&prefix, dotnet.custom_source().is_some())
                {
                    parts.start(EnvironmentPart::LauncherSupport);
                    self.install_dotnet_desktop(paths, runtime, dotnet, mutation_lease, control)?;
                }
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(AdapterFailure::here()),
        }
        let android = self.android;
        let Some((gecko, mono, dotnet)) = new_prefix_components(gecko, mono, dotnet, android)
        else {
            return Err(AdapterFailure::here());
        };
        if wine.is_none() {
            parts.start(EnvironmentPart::Windows);
        }
        self.run_wine(WineInvocation {
            paths,
            runtime,
            working_directory: paths.root(),
            program: WineProgram::Builtin(OsString::from("wineboot")),
            verified_argument: None,
            arguments: new_prefix_wineboot_arguments(android),
            profile: WineProfile::PrefixInitialization,
            timeout: RUNTIME_TIMEOUT,
            mutation_lease,
            control,
            ui_helper: None,
            stall: None,
            accepted_exit_codes: &[],
        })?;
        if let Some(version) = runtime.windows_version() {
            self.run_wine(WineInvocation {
                paths,
                runtime,
                working_directory: paths.root(),
                program: WineProgram::Builtin(OsString::from("reg")),
                verified_argument: None,
                arguments: windows_version_arguments(version),
                profile: WineProfile::Install,
                timeout: RUNTIME_TIMEOUT,
                mutation_lease,
                control,
                ui_helper: None,
                stall: None,
                accepted_exit_codes: &[],
            })?;
        }
        // The game's XInput bridge on Android; the game start puts it back
        // if it is missing.
        if android {
            install_xinput_bridge(paths.directory()).map_err(|_| AdapterFailure::here())?;
        }
        // Best effort, like the sync before each official-launcher session,
        // which sets it again if this fails.
        if self.wpf_software_rendering {
            let _ = self.sync_wpf_software_rendering(paths, runtime, mutation_lease, control);
            if control.is_cancelled() {
                return Err(AdapterFailure::here());
            }
        }
        if let Some(gecko) = gecko {
            parts.start(EnvironmentPart::WebComponents);
            self.install_gecko(paths, runtime, gecko, mutation_lease, control)?;
        }
        if let Some(mono) = mono {
            parts.start(EnvironmentPart::Dotnet);
            self.install_mono(paths, runtime, mono, mutation_lease, control)?;
        }
        match dotnet {
            Some(dotnet) => {
                parts.start(EnvironmentPart::LauncherSupport);
                self.install_dotnet_desktop(paths, runtime, dotnet, mutation_lease, control)
            }
            None => Ok(()),
        }
    }

    fn download_vendor_payload(
        &self,
        paths: &ManagedPaths,
        wrapper: &VerifiedArtifact,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), VendorFailure> {
        let identity = complete_payload_identity(wrapper.kind()).ok_or(VendorFailure::Install)?;
        if managed_complete_payload_directory(wrapper, paths)?.is_some() {
            return Ok(());
        }
        self.download_wrapper_payload(paths, wrapper, mutation_lease, control)?;
        // The prefix is quiet: keep the verified payload as the product's
        // complete installer in the artifact cache, then remove the
        // download folder. Nothing it fetched has run.
        let downloaded = paths
            .directory()
            .open_directory(&Path::new(WRAPPER_DOWNLOADS).join(identity.source_directory))
            .map_err(|_| VendorFailure::Install)?;
        let cache = paths
            .directory()
            .open_directory(Path::new("cache/artifacts"))
            .map_err(|_| VendorFailure::Install)?;
        import_complete_payload(&downloaded, identity, &cache, control)
            .map_err(|_| VendorFailure::Install)?;
        drop(downloaded);
        remove_wrapper_downloads(paths)?;
        Ok(())
    }

    fn download_vendor_payloads_direct(
        &self,
        paths: &ManagedPaths,
        wrappers: &[&VerifiedArtifact],
        control: &OperationControl,
    ) -> Vec<(ArtifactKind, VendorFailure)> {
        direct::download_payloads(paths, wrappers, control)
    }

    fn discard_downloads(&self, paths: &ManagedPaths) -> Result<(), VendorFailure> {
        discard_downloads(paths).map_err(|error| {
            eprintln!(
                "pd2-launcher: the Launcher's downloads were not all removed: {}",
                error.kind()
            );
            VendorFailure::Install
        })
    }

    fn install_vendor_native(
        &self,
        request: VendorInstallRequest<'_>,
        control: &OperationControl,
    ) -> Result<(), VendorFailure> {
        direct::install_native(request, control)
    }

    fn import_existing(
        &self,
        paths: &ManagedPaths,
        existing: &crate::game_folder::ExistingInstall,
        mode: crate::game_folder::ImportMode,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        use crate::game_folder::ImportMode;
        let source = existing
            .folder()
            .open()
            .map_err(|_| AdapterFailure::here())?;
        let install_path = match mode {
            // On the Android host's layout the app's checked copy becomes the
            // Managed game folder, outside the prefix: one rename, no copy, no
            // G: drive and no record of a folder used in place.
            ImportMode::InPlace if paths.android() => {
                if !crate::game_folder::usable_in_place(existing) {
                    return Err(AdapterFailure::here());
                }
                adopt_android_import(paths, existing, &source)?;
                crate::game_folder::MANAGED_WINDOWS_PATH.to_owned()
            }
            ImportMode::Copy => {
                let program_files = paths
                    .directory()
                    .ensure_directory(Path::new("prefix/drive_c/Program Files (x86)"))
                    .map_err(|_| AdapterFailure::here())?;
                let (destination, _) = program_files
                    .create_or_open_empty_directory(OsStr::new("Diablo II"))
                    .map_err(|_| AdapterFailure::here())?;
                crate::game_folder::copy_game_folder(
                    &source,
                    &destination,
                    existing.copy_bytes(),
                    crate::game_folder::copies_project_d2(),
                    control,
                )
                .map_err(|_| AdapterFailure::here())?;
                crate::game_folder::MANAGED_WINDOWS_PATH.to_owned()
            }
            ImportMode::InPlace => {
                // Wine reaches the folder through its own Z: drive, the
                // host root, which wineboot created; on the Android host,
                // whose root reports no free space, through the G: drive
                // the Launcher links to the folder's parent
                // (`in_place_drive`).
                require_host_root_drive(paths)?;
                if !crate::game_folder::usable_in_place(existing) {
                    return Err(AdapterFailure::here());
                }
                in_place_install_path(paths, existing.path(), self.android)?
            }
        };
        reject_artifact_cancelled(control)?;
        // No registry value is written: the game writes `InstallPath` when
        // the value is missing (the Android host's GOLDEN template has the
        // installers' value), and the files prove the game.
        if mode == ImportMode::InPlace && !paths.android() {
            let prefix = paths
                .directory()
                .open_directory(Path::new("prefix"))
                .map_err(|_| AdapterFailure::here())?;
            crate::game_folder::write_pointer(&prefix, existing.folder())
                .map_err(|_| AdapterFailure::here())?;
        }
        // Last, the record that the game was imported, for the Android app.
        let records = paths.records().map_err(|_| AdapterFailure::here())?;
        crate::game_folder::write_import_record(&records, mode, &install_path)
            .map_err(|_| AdapterFailure::here())
    }

    fn install_vendor(
        &self,
        request: VendorInstallRequest<'_>,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), VendorFailure> {
        let expected_kind = match request.product() {
            VendorProduct::BaseGame => ArtifactKind::BaseGameWrapper,
            VendorProduct::Expansion => ArtifactKind::ExpansionWrapper,
        };
        if request.artifact().kind() != expected_kind {
            return Err(VendorFailure::Install);
        }
        // Blizzard's installer installs into C:\Program Files (x86)\Diablo II:
        // on the Android host's layout, the link to the game folder outside
        // the prefix, and that folder.
        crate::game_folder::prepare_game_location(
            request.paths().directory(),
            request.paths().android(),
        )
        .map_err(|error| {
            eprintln!("pd2-launcher: the game folder's link was not made: {error}");
            VendorFailure::Install
        })?;
        let payload_directory =
            managed_complete_payload_directory(request.artifact(), request.paths())?;
        let downloaded;
        let (payload_directory, installer) = match payload_directory {
            Some(directory) => (directory, request.artifact()),
            None => {
                downloaded = self.download_wrapper_payload(
                    request.paths(),
                    request.artifact(),
                    mutation_lease,
                    control,
                )?;
                (downloaded.0.clone(), &downloaded.1)
            }
        };
        let helper_exit = HelperExit::default();
        request
            .with_product_key(|key| {
                let key = std::str::from_utf8(key).map_err(|_| AdapterFailure::here())?;
                // The complete payload is the second-stage installer. Each
                // product consumes its own key as key1; passing the base key as
                // LoD key1 is rejected by the vendor validator even when a valid
                // LoD key is in key2. Named beneath the managed root, as
                // the downloader wrapper is.
                self.run_wine_observing_helper(
                    WineInvocation {
                        paths: request.paths(),
                        runtime: self.installed_runtime(request.paths()),
                        working_directory: &payload_directory,
                        program: WineProgram::ManagedVerified(installer),
                        verified_argument: None,
                        arguments: complete_payload_arguments(key),
                        profile: WineProfile::Install,
                        timeout: INSTALL_TIMEOUT,
                        mutation_lease,
                        control,
                        ui_helper: Some(UiHelper::Vendor),
                        stall: None,
                        accepted_exit_codes: &[],
                    },
                    &helper_exit,
                )
            })
            .map_err(|_| match helper_exit.code() {
                Some(VENDOR_HELPER_KEY_REJECTED) => VendorFailure::KeyRejected,
                Some(VENDOR_HELPER_ERROR_DIALOG) => VendorFailure::ErrorDialog,
                Some(VENDOR_HELPER_INSTALLER_MENU) => VendorFailure::InstallerMenu,
                _ => VendorFailure::Install,
            })?;
        if installer.path() != request.artifact().path() {
            // The payload installed; the prefix is quiet.
            remove_wrapper_downloads(request.paths())?;
        }
        Ok(())
    }

    fn install_client_bootstrap(
        &self,
        paths: &ManagedPaths,
        bootstrap: &VerifiedArtifact,
        mutation_lease: &File,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        if bootstrap.kind() != ArtifactKind::ClientBootstrap {
            return Err(AdapterFailure::here());
        }
        // It installs into the game folder (`/DIR=`): on the Android host's
        // layout, through the prefix's link to the folder outside it.
        crate::game_folder::prepare_game_location(paths.directory(), paths.android()).map_err(
            |error| {
                eprintln!("pd2-launcher: the game folder's link was not made: {error}");
                AdapterFailure::here()
            },
        )?;
        // On the Android host it runs from a checked copy under its own file
        // name (`android_client_installer`).
        let android_copy = if self.android {
            Some(android_client_installer(paths, bootstrap)?)
        } else {
            None
        };
        let program = match &android_copy {
            Some((copy, _)) => WineProgram::Builtin(copy.clone()),
            None => WineProgram::Verified(bootstrap),
        };
        // The accepted installer source uses Inno Setup 5.5.9, disables
        // restart-by-run, and marks the final PD2Launcher launch skipifsilent.
        let result = self.run_wine(WineInvocation {
            paths,
            runtime: self.installed_runtime(paths),
            working_directory: paths.root(),
            program,
            verified_argument: None,
            arguments: vec![
                OsString::from("/SP-"),
                OsString::from("/VERYSILENT"),
                OsString::from("/SUPPRESSMSGBOXES"),
                OsString::from("/NORESTART"),
                OsString::from(format!("/DIR={}", paths.windows_game())),
            ],
            profile: WineProfile::Install,
            timeout: INSTALL_TIMEOUT,
            mutation_lease,
            control,
            ui_helper: None,
            stall: None,
            accepted_exit_codes: &[],
        });
        // The copy goes as soon as the installer ends.
        drop(android_copy);
        result?;
        // The pinned bootstrap installed the official launcher: its copy is
        // the one trusted until the official launcher updates itself.
        if bootstrap.custom_source().is_none() {
            crate::audit::record_official_launcher(paths.directory())
                .map_err(|_| AdapterFailure::here())?;
        }
        Ok(())
    }
}

/// Reports the size of the Blizzard Downloader's folder as byte progress
/// while it runs. It only reads sizes (never following a link) and decides
/// nothing: the payload is still verified by its frozen identity.
struct DownloadFolderWatcher {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl DownloadFolderWatcher {
    const INTERVAL: Duration = Duration::from_secs(1);

    /// Reports the folder's size as `phase`'s progress each second and,
    /// once its program runs ([`DownloadStall::arm`]), sets `stall` when
    /// `clock` finds the folder unchanged and no data received (the host's
    /// count in `received`, when it gives one) for its limit.
    fn start(
        folder: PathBuf,
        expected: u64,
        phase: WorkPhase,
        control: OperationControl,
        mut clock: GrowthClock,
        stall: DownloadStall,
        received: Option<PathBuf>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("pd2-download-watch".to_owned())
            .spawn(move || {
                while !stopped.load(Ordering::Acquire) {
                    let size = folder_size(&folder, 4).min(expected);
                    if size > 0 {
                        control.report(
                            crate::operation::WorkProgress::for_phase(phase)
                                .with_file_bytes(size, Some(expected)),
                        );
                    }
                    let count = received.as_deref().and_then(received_bytes);
                    if stall.is_armed()
                        && clock.observe(folder_state(&folder, 8), count, Instant::now())
                        && !stall.is_stalled()
                    {
                        // How much came in while it waited, to check
                        // DOWNLOAD_ACTIVITY_BYTES in the field.
                        eprintln!(
                            "pd2-launcher: the download showed no activity for its limit \
                             (received since the data last counted: {})",
                            clock.received_since_mark(count).map_or_else(
                                || "not counted".to_owned(),
                                |bytes| format!("{bytes} bytes")
                            )
                        );
                        stall.mark_stalled();
                    }
                    let mut waited = Duration::ZERO;
                    while waited < Self::INTERVAL && !stopped.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(100));
                        waited += Duration::from_millis(100);
                    }
                }
            })
            .map_err(|error| {
                // Neither progress nor the stall limit then: the run waits
                // for its own timeout.
                eprintln!("pd2-launcher: the download watcher did not start: {error}");
            })
            .ok();
        Self { stop, thread }
    }
}

/// A watched download's stall, shared by its [`DownloadFolderWatcher`] and
/// the wait for its program ([`WineInvocation::stall`]): the clock counts
/// once the program runs ([`Self::arm`]), the wait ends the program when
/// [`Self::is_stalled`], and [`Self::ended_it`] then tells that end apart
/// from another failure of the same run.
#[derive(Clone, Default)]
struct DownloadStall(Arc<StallState>);

#[derive(Default)]
struct StallState {
    armed: AtomicBool,
    stalled: AtomicBool,
    ended: AtomicBool,
}

impl DownloadStall {
    fn arm(&self) {
        self.0.armed.store(true, Ordering::Release);
    }

    fn is_armed(&self) -> bool {
        self.0.armed.load(Ordering::Acquire)
    }

    fn mark_stalled(&self) {
        self.0.stalled.store(true, Ordering::Release);
    }

    fn is_stalled(&self) -> bool {
        self.0.stalled.load(Ordering::Acquire)
    }

    fn mark_ended(&self) {
        self.0.ended.store(true, Ordering::Release);
    }

    fn ended_it(&self) -> bool {
        self.0.ended.load(Ordering::Acquire)
    }
}

/// What tells a download folder changed: how many files it holds, their
/// total length and their allocated blocks, so a downloader that fills a file
/// of its final length (pieces out of order) still counts as working. Not
/// modification times: a downloader that only rewrites a state file is not
/// downloading.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct FolderState {
    files: u64,
    length: u64,
    blocks: u64,
}

/// [`FolderState`] of the regular files below `folder`, `depth` levels deep.
fn folder_state(folder: &Path, depth: usize) -> FolderState {
    let mut state = FolderState::default();
    let Ok(entries) = fs::read_dir(folder) else {
        return state;
    };
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(kind) if kind.is_file() => {
                if let Ok(metadata) = entry.metadata() {
                    state.files += 1;
                    state.length += metadata.len();
                    state.blocks += metadata.blocks();
                }
            }
            Ok(kind) if kind.is_dir() && depth > 1 => {
                let inner = folder_state(&entry.path(), depth - 1);
                state.files += inner.files;
                state.length += inner.length;
                state.blocks += inner.blocks;
            }
            _ => {}
        }
    }
    state
}

/// The host's received-byte count in `file` ([`RECEIVED_BYTES_FILE`]), if it
/// holds one.
fn received_bytes(file: &Path) -> Option<u64> {
    fs::read_to_string(file).ok()?.trim().parse().ok()
}

/// When a download last showed activity: [`Self::observe`] says whether it
/// has shown none for its limit. Activity is any change of its folder, as a
/// downloader may also start a file again or replace its parts, or
/// [`DOWNLOAD_ACTIVITY_BYTES`] received since the data last counted, when the
/// host counts them (so a slow trickle adds up). The limit is `limit`
/// ([`DOWNLOAD_STALL_LIMIT`]) once the folder has changed; before that the
/// downloader has not started writing: `resume_limit`
/// ([`DOWNLOAD_RESUME_LIMIT`]) while the folder holds parts from an earlier
/// run, `empty_limit` ([`DOWNLOAD_EMPTY_LIMIT`]) while it holds none, as one
/// that never showed its window is the helper's to report (its exit 21).
/// Data only ever moves the start of the wait later: with it a downloader is
/// never stopped sooner than by its folder alone.
struct GrowthClock {
    limit: Duration,
    resume_limit: Duration,
    empty_limit: Duration,
    state: Option<FolderState>,
    /// The received bytes at the last activity.
    received: Option<u64>,
    changed: bool,
    since: Instant,
}

impl GrowthClock {
    fn new(limit: Duration, resume_limit: Duration, empty_limit: Duration, now: Instant) -> Self {
        Self {
            limit,
            resume_limit,
            empty_limit,
            state: None,
            received: None,
            changed: false,
            since: now,
        }
    }

    fn observe(&mut self, state: FolderState, received: Option<u64>, now: Instant) -> bool {
        let first = self.state.is_none();
        let folder = !first && self.state != Some(state);
        let data = match (received, self.received) {
            (Some(now_received), Some(mark)) => {
                now_received.saturating_sub(mark) >= DOWNLOAD_ACTIVITY_BYTES
                    // A count that went down started again (a restarted host).
                    || now_received < mark
            }
            _ => false,
        };
        if first || folder || data {
            self.state = Some(state);
            self.since = now;
        }
        // Only the folder says the downloader writes: data alone keeps the
        // longer limits before that.
        self.changed |= folder;
        // The mark moves when data counted, so a slow trickle adds up; a
        // count that appears later starts it.
        if first || data || self.received.is_none() {
            self.received = received;
        }
        let limit = if self.changed {
            self.limit
        } else if state.files > 0 {
            self.resume_limit
        } else {
            self.empty_limit
        };
        now.saturating_duration_since(self.since) >= limit
    }

    /// The bytes received since the data last counted, when the host counts them.
    fn received_since_mark(&self, received: Option<u64>) -> Option<u64> {
        Some(received?.saturating_sub(self.received?))
    }
}

impl Drop for DownloadFolderWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Total size of the regular files below `folder`, `depth` levels deep.
fn folder_size(folder: &Path, depth: usize) -> u64 {
    let Ok(entries) = fs::read_dir(folder) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |metadata| metadata.len()),
            Ok(kind) if kind.is_dir() && depth > 1 => folder_size(&entry.path(), depth - 1),
            _ => 0,
        })
        .sum()
}

/// Remove the wrapper download folder once its payload has installed.
fn remove_wrapper_downloads(paths: &ManagedPaths) -> Result<(), AdapterFailure> {
    let drive = paths
        .directory()
        .open_directory(
            Path::new(WRAPPER_DOWNLOADS)
                .parent()
                .ok_or_else(|| AdapterFailure::here())?,
        )
        .map_err(|_| AdapterFailure::here())?;
    let name = Path::new(WRAPPER_DOWNLOADS)
        .file_name()
        .ok_or_else(|| AdapterFailure::here())?;
    match drive.remove_owned_tree(name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(AdapterFailure::here()),
    }
}

struct RuntimeHandles {
    runtime: InstalledRuntime,
    preloader: File,
    wine: File,
    wineserver: File,
    bin: Directory,
}

impl RuntimeHandles {
    /// Open exactly `installed`; the caller names the runtime it validated
    /// or audited, so no second detection can substitute another tree.
    fn open(paths: &ManagedPaths, installed: InstalledRuntime) -> Result<Self, AdapterFailure> {
        let runtime = Path::new("runner").join(installed.directory()).join("bin");
        let bin = paths
            .directory()
            .open_directory(&runtime)
            .map_err(|_| AdapterFailure::here())?;
        Ok(Self {
            runtime: installed,
            preloader: open_executable(paths.directory(), runtime.join(installed.loader_name()))?,
            wine: open_executable(paths.directory(), runtime.join("wine"))?,
            wineserver: open_executable(paths.directory(), runtime.join("wineserver"))?,
            bin,
        })
    }
}

fn open_executable(root: &Directory, relative: PathBuf) -> Result<File, AdapterFailure> {
    let file = root
        .open_beneath(&relative, OFlags::RDONLY)
        .map_err(|_| AdapterFailure::here())?;
    let metadata = file.metadata().map_err(|_| AdapterFailure::here())?;
    if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
        Ok(file)
    } else {
        Err(AdapterFailure::here())
    }
}

/// Materialize (or keep) the runtime for `archive` and return the exact
/// runtime that was validated, so the caller executes that runtime and never
/// re-detects another one from the managed tree.
fn ensure_runtime(
    paths: &ManagedPaths,
    archive: &VerifiedArtifact,
    control: &OperationControl,
) -> Result<InstalledRuntime, AdapterFailure> {
    let runner_directory = paths
        .directory()
        .ensure_directory(Path::new("runner"))
        .map_err(|_| AdapterFailure::here())?;
    let builtin = InstalledRuntime::BuiltIn(managed_runtime());
    // A prefix the row's earlier pinned runtime made keeps that runtime
    // while it is intact, as the Auditor selects it; it is never downloaded
    // again, so a damaged one gives way to the current runtime.
    if let InstalledRuntime::BuiltIn(retained) =
        InstalledRuntime::select(paths.directory(), archive.custom_source().is_some())
        && retained != managed_runtime()
        && runner_directory
            .open_directory(Path::new(retained.identity))
            .is_ok_and(|runtime| validate_runtime_directory(&runtime, retained).is_ok())
    {
        return Ok(InstalledRuntime::BuiltIn(retained));
    }
    if let Some(url) = archive.custom_source() {
        // With a custom source, a complete custom runtime is kept as
        // installed, and without one a verified pinned runtime is kept; the
        // selection matches the Auditor's for the same configured source.
        if InstalledRuntime::select(paths.directory(), true).is_custom() {
            if runner_directory
                .open_directory(Path::new(CUSTOM_RUNTIME_DIRECTORY))
                .is_ok_and(|runtime| validate_custom_runtime_directory(&runtime).is_ok())
            {
                return Ok(InstalledRuntime::Custom);
            }
        } else if runner_directory
            .open_directory(Path::new(managed_runtime().identity))
            .is_ok_and(|runtime| validate_runtime_directory(&runtime, managed_runtime()).is_ok())
        {
            return Ok(builtin);
        }
        ensure_custom_runtime(&runner_directory, archive, url, control)?;
        return Ok(InstalledRuntime::Custom);
    }
    // The built-in source never uses a custom runtime: the pinned runtime is
    // materialized (beside any custom one) and verified below.
    let runtime = match runner_directory.open_directory(Path::new(managed_runtime().identity)) {
        Ok(runtime) if validate_runtime_directory(&runtime, managed_runtime()).is_ok() => {
            return Ok(builtin);
        }
        Ok(runtime) => {
            // A canonical directory without this durable, parent-scoped state
            // is not ours to rewrite. A prior Launcher materialization writes
            // the state before exposing/altering the canonical directory.
            validate_receipt_at(
                &runner_directory,
                RUNTIME_MATERIALIZATION_STATE,
                managed_runtime().archive_sha512,
            )?;
            runtime
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            write_receipt_last_at(
                &runner_directory,
                RUNTIME_MATERIALIZATION_STATE,
                managed_runtime().archive_sha512,
            )?;
            runner_directory
                .ensure_directory(Path::new(managed_runtime().identity))
                .map_err(|_| AdapterFailure::here())?
        }
        Err(_) => return Err(AdapterFailure::here()),
    };
    let archive = open_verified_program(archive)?;
    extract_runtime_archive_safely(
        archive,
        &runtime,
        ArchiveRoot::Exact(managed_runtime().identity),
        control,
    )?;
    if control.is_cancelled() {
        // No child or externally observable transaction is active. The
        // durable materialization state makes the partial tree restartable.
        control.mark_cancelled_safe();
        return Err(AdapterFailure::here());
    }
    validate_runtime_payload(&runtime, managed_runtime())?;
    write_receipt_last_at(&runtime, RUNTIME_RECEIPT, managed_runtime().archive_sha512)?;
    runner_directory
        .sync()
        .map_err(|_| AdapterFailure::here())?;
    validate_runtime_directory(&runtime, managed_runtime())?;
    Ok(builtin)
}

/// A runtime materialized from a custom archive. The archive is extracted
/// into the Launcher-owned `runner/custom-wine.staging`, validated there, and
/// only then renamed to `runner/custom-wine`; the receipt is written last. A
/// failed or cancelled extraction therefore never blocks a later archive:
/// its stale staging tree is removed before the next attempt.
fn ensure_custom_runtime(
    runner_directory: &Directory,
    archive: &VerifiedArtifact,
    url: &str,
    control: &OperationControl,
) -> Result<(), AdapterFailure> {
    let sha256 = archive.content_sha256();
    // The materialization state marks custom runtime trees as the Launcher's
    // own; without it an existing tree is not ours to replace.
    let owned = runner_directory
        .open_file(Path::new(CUSTOM_RUNTIME_MATERIALIZATION_STATE))
        .is_ok();
    let existing = match runner_directory.open_directory(Path::new(CUSTOM_RUNTIME_DIRECTORY)) {
        Ok(existing) => Some(existing),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => return Err(AdapterFailure::here()),
    };
    if let Some(existing) = &existing
        && read_custom_receipt_at(existing, CUSTOM_RUNTIME_RECEIPT, DownloadArtifact::Wine).is_err()
    {
        if !owned {
            return Err(AdapterFailure::here());
        }
        // Renamed into place from this archive, but interrupted before the
        // receipt: finish the receipt instead of extracting again.
        if validate_receipt_at(
            runner_directory,
            CUSTOM_RUNTIME_MATERIALIZATION_STATE,
            sha256,
        )
        .is_ok()
            && validate_custom_runtime_payload(existing).is_ok()
        {
            return write_custom_runtime_receipt(runner_directory, existing, url, sha256);
        }
    }
    match runner_directory.open_directory(Path::new(CUSTOM_RUNTIME_STAGING)) {
        Ok(_) if owned => runner_directory
            .remove_owned_tree(OsStr::new(CUSTOM_RUNTIME_STAGING))
            .map_err(|_| AdapterFailure::here())?,
        Ok(_) => return Err(AdapterFailure::here()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(AdapterFailure::here()),
    }
    runner_directory
        .atomic_write(
            OsStr::new(CUSTOM_RUNTIME_MATERIALIZATION_STATE),
            sha256.as_bytes(),
        )
        .map_err(|_| AdapterFailure::here())?;
    let staging = runner_directory
        .ensure_directory(Path::new(CUSTOM_RUNTIME_STAGING))
        .map_err(|_| AdapterFailure::here())?;
    let file = open_verified_program(archive)?;
    extract_runtime_archive_safely(file, &staging, ArchiveRoot::Custom, control)?;
    if control.is_cancelled() {
        control.mark_cancelled_safe();
        return Err(AdapterFailure::here());
    }
    let top = custom_runtime_root(&staging)?;
    let runtime = match &top {
        Some(name) => staging.open_directory(Path::new(name)),
        None => staging.try_clone(),
    }
    .map_err(|_| AdapterFailure::here())?;
    validate_custom_runtime_payload(&runtime)?;
    if existing.is_some() {
        runner_directory
            .remove_owned_tree(OsStr::new(CUSTOM_RUNTIME_DIRECTORY))
            .map_err(|_| AdapterFailure::here())?;
    }
    match &top {
        Some(name) => staging.rename_directory_noreplace(
            name,
            &runtime,
            runner_directory,
            OsStr::new(CUSTOM_RUNTIME_DIRECTORY),
        ),
        None => runner_directory.rename_directory_noreplace(
            OsStr::new(CUSTOM_RUNTIME_STAGING),
            &runtime,
            runner_directory,
            OsStr::new(CUSTOM_RUNTIME_DIRECTORY),
        ),
    }
    .map_err(|_| AdapterFailure::here())?;
    if top.is_some() {
        runner_directory
            .remove_owned_tree(OsStr::new(CUSTOM_RUNTIME_STAGING))
            .map_err(|_| AdapterFailure::here())?;
    }
    let runtime = runner_directory
        .open_directory(Path::new(CUSTOM_RUNTIME_DIRECTORY))
        .map_err(|_| AdapterFailure::here())?;
    write_custom_runtime_receipt(runner_directory, &runtime, url, sha256)
}

fn write_custom_runtime_receipt(
    runner_directory: &Directory,
    runtime: &Directory,
    url: &str,
    sha256: &str,
) -> Result<(), AdapterFailure> {
    write_custom_receipt_at(
        runtime,
        CUSTOM_RUNTIME_RECEIPT,
        &CustomSourceReceipt::new(DownloadArtifact::Wine, url, sha256),
    )?;
    runner_directory
        .sync()
        .map_err(|_| AdapterFailure::here())?;
    validate_custom_runtime_directory(runtime)
}

/// The runtime root inside an extracted custom archive: the tree itself when
/// it holds `bin/`, otherwise its single top-level directory.
fn custom_runtime_root(staging: &Directory) -> Result<Option<OsString>, AdapterFailure> {
    if staging.open_directory(Path::new("bin")).is_ok() {
        return Ok(None);
    }
    match staging
        .names()
        .map_err(|_| AdapterFailure::here())?
        .as_slice()
    {
        [only] if staging.open_directory(Path::new(only)).is_ok() => Ok(Some(only.clone())),
        _ => Err(AdapterFailure::here()),
    }
}

fn validate_custom_runtime_directory(runtime: &Directory) -> Result<(), AdapterFailure> {
    read_custom_receipt_at(runtime, CUSTOM_RUNTIME_RECEIPT, DownloadArtifact::Wine)?;
    validate_custom_runtime_payload(runtime)
}

/// Presence and executable structure only: `bin/wine` must be an executable
/// ELF and `bin/wineserver` executable. Contents are not pinned.
fn validate_custom_runtime_payload(runtime: &Directory) -> Result<(), AdapterFailure> {
    let mut wine = open_executable(runtime, PathBuf::from("bin/wine"))?;
    let mut magic = [0_u8; 4];
    wine.read_exact(&mut magic)
        .map_err(|_| AdapterFailure::here())?;
    if magic != *b"\x7fELF" {
        return Err(AdapterFailure::here());
    }
    open_executable(runtime, PathBuf::from("bin/wineserver")).map(drop)
}

/// A prefix whose Gecko was installed from a custom source: no pinned Gecko
/// receipt, a well-formed custom one, and the prefix hives Wine created.
fn custom_gecko_prefix(prefix: &Directory) -> bool {
    matches!(
        prefix.open_file(Path::new(GECKO_RECEIPT)),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    ) && read_custom_receipt_at(prefix, CUSTOM_GECKO_RECEIPT, DownloadArtifact::Gecko).is_ok()
        && ["system.reg", "user.reg", "userdef.reg"]
            .iter()
            .all(|hive| prefix.open_file(Path::new(hive)).is_ok())
}

/// Remove a Launcher receipt if present; an absent receipt is not an error.
fn remove_receipt_at(directory: &Directory, name: &str) -> Result<(), AdapterFailure> {
    match directory.open_file(Path::new(name)) {
        Ok(file) => directory
            .remove_open_file_bounded(OsStr::new(name), &file)
            .map_err(|_| AdapterFailure::here()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(AdapterFailure::here()),
    }
}

fn write_custom_receipt_at(
    directory: &Directory,
    name: &str,
    receipt: &CustomSourceReceipt,
) -> Result<(), AdapterFailure> {
    directory
        .atomic_write(OsStr::new(name), &receipt.encode())
        .map_err(|_| AdapterFailure::here())
}

fn read_custom_receipt_at(
    directory: &Directory,
    name: &str,
    artifact: DownloadArtifact,
) -> Result<CustomSourceReceipt, AdapterFailure> {
    let file = directory
        .open_file(Path::new(name))
        .map_err(|_| AdapterFailure::here())?;
    let metadata = file.metadata().map_err(|_| AdapterFailure::here())?;
    if !metadata.is_file() || metadata.len() > 8 * 1024 {
        return Err(AdapterFailure::here());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(8 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AdapterFailure::here())?;
    CustomSourceReceipt::decode(&bytes, artifact).ok_or_else(|| AdapterFailure::here())
}

/// How archive paths map into the extraction directory.
#[derive(Clone, Copy)]
enum ArchiveRoot<'a> {
    /// The pinned archive's exact, frozen top-level identity, stripped; only
    /// flat same-directory links to regular files are accepted.
    Exact(&'a str),
    /// A custom archive, extracted with its own layout: a leading `./` and
    /// pax global headers are ignored, and relative links (including
    /// directory links such as `lib64 -> lib`) are accepted when they resolve
    /// inside the extraction root. The runtime root is chosen afterwards.
    Custom,
}

#[cfg(test)]
fn extract_archive_safely(
    source: File,
    destination: &Directory,
    identity: &str,
    control: &OperationControl,
) -> Result<(), AdapterFailure> {
    let runtime = destination
        .ensure_directory(Path::new(identity))
        .map_err(|_| AdapterFailure::here())?;
    extract_runtime_archive_safely(source, &runtime, ArchiveRoot::Exact(identity), control)
}

fn extract_runtime_archive_safely(
    source: File,
    runtime: &Directory,
    root: ArchiveRoot<'_>,
    control: &OperationControl,
) -> Result<(), AdapterFailure> {
    extract_runtime_archive_safely_inner(source, runtime, root, control, |_| Ok(()))
}

fn extract_runtime_archive_safely_inner(
    source: File,
    runtime: &Directory,
    root: ArchiveRoot<'_>,
    control: &OperationControl,
    mut after_file: impl FnMut(&Path) -> Result<(), AdapterFailure>,
) -> Result<(), AdapterFailure> {
    let custom = matches!(root, ArchiveRoot::Custom);
    let decoder = XzDecoder::new(source);
    let mut archive = Archive::new(decoder);
    let mut paths = BTreeSet::new();
    let mut regular_paths = BTreeSet::new();
    let mut directories = BTreeSet::new();
    let mut links = BTreeMap::new();
    let mut count = 0_usize;
    let mut total = 0_u64;
    let entries = archive.entries().map_err(|_| AdapterFailure::here())?;
    for entry in entries {
        if control.is_cancelled() {
            return Err(AdapterFailure::here());
        }
        let mut entry = entry.map_err(|_| AdapterFailure::here())?;
        count = count.checked_add(1).ok_or_else(|| AdapterFailure::here())?;
        if count > MAX_ARCHIVE_ENTRIES {
            return Err(AdapterFailure::here());
        }
        let entry_type = entry.header().entry_type();
        if custom && entry_type == EntryType::XGlobalHeader {
            // `git archive` and pax writers prepend archive-wide metadata; it
            // names no file and is never extracted.
            continue;
        }
        let path = entry
            .path()
            .map_err(|_| AdapterFailure::here())?
            .into_owned();
        let relative = match root {
            ArchiveRoot::Exact(identity) => {
                validate_archive_path(&path, OsStr::new(identity))?;
                path.strip_prefix(identity)
                    .map_err(|_| AdapterFailure::here())?
                    .to_owned()
            }
            ArchiveRoot::Custom => custom_archive_path(&path)?,
        };
        if !paths.insert(relative.clone()) {
            return Err(AdapterFailure::here());
        }
        if !(entry_type.is_dir() || entry_type.is_file() || entry_type.is_symlink()) {
            return Err(AdapterFailure::here());
        }
        if entry_type.is_file() {
            total = total
                .checked_add(entry.size())
                .ok_or_else(|| AdapterFailure::here())?;
            if total > MAX_EXTRACTED_BYTES {
                return Err(AdapterFailure::here());
            }
        }
        directories.extend(relative.ancestors().skip(1).map(Path::to_owned));
        if relative.as_os_str().is_empty() {
            if !entry_type.is_dir() {
                return Err(AdapterFailure::here());
            }
        } else if entry_type.is_dir() {
            runtime
                .ensure_directory(&relative)
                .map_err(|_| AdapterFailure::here())?;
            directories.insert(relative.clone());
        } else if entry_type.is_symlink() {
            let parent = relative.parent().ok_or_else(|| AdapterFailure::here())?;
            let name = relative.file_name().ok_or_else(|| AdapterFailure::here())?;
            let target = entry
                .link_name()
                .map_err(|_| AdapterFailure::here())?
                .ok_or_else(|| AdapterFailure::here())?
                .into_owned();
            let directory = runtime
                .ensure_directory(parent)
                .map_err(|_| AdapterFailure::here())?;
            let resolved = if custom {
                let resolved = resolve_archive_link(&relative, &target)?;
                directory
                    .create_or_validate_relative_symlink(name, &target)
                    .map_err(|_| AdapterFailure::here())?;
                resolved
            } else {
                let mut components = target.components();
                let Some(Component::Normal(target_name)) = components.next() else {
                    return Err(AdapterFailure::here());
                };
                if components.next().is_some() {
                    return Err(AdapterFailure::here());
                }
                directory
                    .create_or_validate_flat_symlink(name, target_name)
                    .map_err(|_| AdapterFailure::here())?;
                ResolvedLink {
                    target: parent.join(target_name),
                    through: Vec::new(),
                }
            };
            links.insert(relative.clone(), resolved);
        } else {
            let parent = relative.parent().ok_or_else(|| AdapterFailure::here())?;
            let name = relative.file_name().ok_or_else(|| AdapterFailure::here())?;
            let directory = runtime
                .ensure_directory(parent)
                .map_err(|_| AdapterFailure::here())?;
            let mut output = directory
                .open_or_create_file(name)
                .map_err(|_| AdapterFailure::here())?;
            output.set_len(0).map_err(|_| AdapterFailure::here())?;
            output
                .seek(SeekFrom::Start(0))
                .map_err(|_| AdapterFailure::here())?;
            let mut copied = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                if control.is_cancelled() {
                    return Err(AdapterFailure::here());
                }
                let read = entry
                    .read(&mut buffer)
                    .map_err(|_| AdapterFailure::here())?;
                if read == 0 {
                    break;
                }
                output
                    .write_all(&buffer[..read])
                    .map_err(|_| AdapterFailure::here())?;
                copied = copied
                    .checked_add(read as u64)
                    .ok_or_else(|| AdapterFailure::here())?;
            }
            if copied != entry.size() {
                return Err(AdapterFailure::here());
            }
            output.flush().map_err(|_| AdapterFailure::here())?;
            let mode = entry.header().mode().map_err(|_| AdapterFailure::here())? & 0o777;
            output
                .set_permissions(fs::Permissions::from_mode(mode))
                .map_err(|_| AdapterFailure::here())?;
            output.sync_all().map_err(|_| AdapterFailure::here())?;
            directory
                .require_open_file(name, &output)
                .map_err(|_| AdapterFailure::here())?;
            regular_paths.insert(relative.clone());
            after_file(&relative)?;
        }
    }
    if count == 0 || paths.is_empty() {
        return Err(AdapterFailure::here());
    }
    // Pinned archives may link only to regular files; custom archives may
    // also link to directories of the extracted tree.
    let link_directories = custom.then_some(&directories);
    validate_archive_link_graph(&links, &regular_paths, link_directories)?;
    runtime.sync().map_err(|_| AdapterFailure::here())?;
    Ok(())
}

/// Where an archive link points, resolved lexically inside the extraction
/// root, plus the directories its target walks through.
struct ResolvedLink {
    target: PathBuf,
    through: Vec<PathBuf>,
}

/// A custom archive path without a leading `./`: only normal components.
fn custom_archive_path(path: &Path) -> Result<PathBuf, AdapterFailure> {
    let mut components = path.components().peekable();
    while components.peek() == Some(&Component::CurDir) {
        components.next();
    }
    components
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            _ => Err(AdapterFailure::here()),
        })
        .collect()
}

/// Resolve `target` of the link at `link` lexically from the link's parent.
/// The result must stay strictly inside the extraction root; absolute
/// targets and any `..` above the root are rejected.
fn resolve_archive_link(link: &Path, target: &Path) -> Result<ResolvedLink, AdapterFailure> {
    let mut current = link
        .parent()
        .ok_or_else(|| AdapterFailure::here())?
        .to_owned();
    let mut through = Vec::new();
    let components = target.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !current.pop() {
                    return Err(AdapterFailure::here());
                }
            }
            Component::Normal(name) => {
                current.push(name);
                if index + 1 < components.len() {
                    through.push(current.clone());
                }
            }
            Component::RootDir | Component::Prefix(_) => return Err(AdapterFailure::here()),
        }
    }
    if current.as_os_str().is_empty() {
        return Err(AdapterFailure::here());
    }
    Ok(ResolvedLink {
        target: current,
        through,
    })
}

/// Every link chain must end, without a cycle, at a regular archive file or
/// (when `directories` is given) an archive directory, and every directory a
/// target walks through must be a real archive directory rather than a link,
/// so the lexical resolution above is also the physical one.
fn validate_archive_link_graph(
    links: &BTreeMap<PathBuf, ResolvedLink>,
    regular_paths: &BTreeSet<PathBuf>,
    directories: Option<&BTreeSet<PathBuf>>,
) -> Result<(), AdapterFailure> {
    for link in links.keys() {
        let mut current = link;
        let mut visited = BTreeSet::new();
        while let Some(next) = links.get(current) {
            if !visited.insert(current) {
                return Err(AdapterFailure::here());
            }
            if !next.through.iter().all(|directory| {
                directories.is_some_and(|directories| directories.contains(directory))
                    && !links.contains_key(directory)
            }) {
                return Err(AdapterFailure::here());
            }
            current = &next.target;
        }
        let is_directory = directories.is_some_and(|directories| {
            directories.contains(current) && !current.as_os_str().is_empty()
        });
        if !regular_paths.contains(current) && !is_directory {
            return Err(AdapterFailure::here());
        }
    }
    Ok(())
}

fn validate_archive_path(path: &Path, identity: &OsStr) -> Result<(), AdapterFailure> {
    let mut components = path.components();
    if components.next() != Some(Component::Normal(identity))
        || components.any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(AdapterFailure::here());
    }
    Ok(())
}

fn validate_runtime_directory(
    runtime: &Directory,
    profile: &ManagedRuntime,
) -> Result<(), AdapterFailure> {
    validate_receipt_at(runtime, RUNTIME_RECEIPT, profile.archive_sha512)?;
    validate_runtime_payload(runtime, profile)
}

fn validate_runtime_payload(
    runtime: &Directory,
    profile: &ManagedRuntime,
) -> Result<(), AdapterFailure> {
    validate_file_at(
        runtime,
        Path::new("bin/wine"),
        profile.wine.size,
        profile.wine.sha256,
        true,
    )?;
    validate_file_at(
        runtime,
        &Path::new("bin").join(profile.loader_name),
        profile.loader.size,
        profile.loader.sha256,
        true,
    )
}

fn validate_receipt_at(
    directory: &Directory,
    name: &str,
    expected: &str,
) -> Result<(), AdapterFailure> {
    let mut file = directory
        .open_file(Path::new(name))
        .map_err(|_| AdapterFailure::here())?;
    if file.metadata().map_err(|_| AdapterFailure::here())?.len() > 256 {
        return Err(AdapterFailure::here());
    }
    let mut value = String::new();
    file.read_to_string(&mut value)
        .map_err(|_| AdapterFailure::here())?;
    (value == expected)
        .then_some(())
        .ok_or_else(|| AdapterFailure::here())
}

fn validate_file_at(
    directory: &Directory,
    relative: &Path,
    expected_size: u64,
    expected_sha256: &str,
    executable: bool,
) -> Result<(), AdapterFailure> {
    let mut file = directory
        .open_file(relative)
        .map_err(|_| AdapterFailure::here())?;
    let metadata = file.metadata().map_err(|_| AdapterFailure::here())?;
    if metadata.len() != expected_size || (executable && metadata.permissions().mode() & 0o111 == 0)
    {
        return Err(AdapterFailure::here());
    }
    let (sha256, _) = hash_open_artifact(&mut file).map_err(|_| AdapterFailure::here())?;
    (sha256 == expected_sha256)
        .then_some(())
        .ok_or_else(|| AdapterFailure::here())
}

/// A prefix Wine created: its registry hives exist.
fn initialized_prefix(prefix: &Directory) -> bool {
    ["system.reg", "user.reg", "userdef.reg"]
        .iter()
        .all(|hive| prefix.open_file(Path::new(hive)).is_ok())
}

/// A prefix Wine's first boot never finished, safe to make again: none of
/// its registry hives exists at all (a hive that exists but can't be
/// opened, such as one a backup hard-linked, is not this), and it holds no
/// Diablo II and no saved games. Deliberately narrow: anything else is
/// refused, so a player's installation is never removed by setup.
fn abandoned_prefix(prefix: &Directory) -> bool {
    let absent = |relative: &str| {
        matches!(
            prefix.open_beneath(Path::new(relative), OFlags::PATH | OFlags::NOFOLLOW),
            Err(error) if error.kind() == io::ErrorKind::NotFound
        )
    };
    let no_saves = || match prefix.open_directory(Path::new("drive_c/users")) {
        Err(error) => error.kind() == io::ErrorKind::NotFound,
        Ok(users) => users.names().is_ok_and(|names| {
            names.iter().all(|user| {
                match users.open_directory(&Path::new(user).join("Saved Games")) {
                    Err(error) => error.kind() == io::ErrorKind::NotFound,
                    Ok(saved) => saved.names().is_ok_and(|names| names.is_empty()),
                }
            })
        }),
    };
    ["system.reg", "user.reg", "userdef.reg"]
        .iter()
        .all(|hive| absent(hive))
        && absent("drive_c/Program Files (x86)/Diablo II")
        && absent("drive_c/Program Files/Diablo II")
        && no_saves()
}

/// A custom Gecko receipt satisfies the prefix only while the Gecko source
/// is custom (`custom_gecko`), mirroring the Auditor.
fn gecko_installed(prefix: &Directory, custom_gecko: bool) -> bool {
    (custom_gecko && custom_gecko_prefix(prefix))
        || (validate_receipt_at(prefix, GECKO_RECEIPT, GECKO_SHA256).is_ok()
            && validate_file_at(
                prefix,
                Path::new("drive_c/windows/syswow64/gecko/2.47.4/wine_gecko/xul.dll"),
                71_766_016,
                GECKO_XUL_SHA256,
                false,
            )
            .is_ok())
}

/// A prefix component installed after Gecko, with a pinned receipt for its
/// built-in installer or a custom-unverified one, mirroring the Auditor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrefixDependency {
    WineMono,
    DotnetDesktop,
}

impl PrefixDependency {
    const fn builtin_receipt(self) -> &'static str {
        match self {
            Self::WineMono => MONO_RECEIPT,
            Self::DotnetDesktop => DOTNET_DESKTOP_RECEIPT,
        }
    }

    const fn custom_receipt(self) -> &'static str {
        match self {
            Self::WineMono => CUSTOM_MONO_RECEIPT,
            Self::DotnetDesktop => CUSTOM_DOTNET_DESKTOP_RECEIPT,
        }
    }

    const fn builtin_sha256(self) -> &'static str {
        match self {
            Self::WineMono => crate::audit::WINE_MONO_MSI_SHA256,
            Self::DotnetDesktop => crate::audit::DOTNET_DESKTOP_INSTALLER_SHA256,
        }
    }

    const fn download_artifact(self) -> DownloadArtifact {
        match self {
            Self::WineMono => DownloadArtifact::WineMono,
            Self::DotnetDesktop => DownloadArtifact::DotnetDesktop,
        }
    }

    /// The prefix-relative tree the installer must leave: the exact pinned
    /// version for the built-in source, any version for a custom one.
    const fn installed_tree(self, custom: bool) -> &'static str {
        match (self, custom) {
            (Self::WineMono, _) => "drive_c/windows/mono/mono-2.0",
            (Self::DotnetDesktop, false) => {
                "drive_c/Program Files (x86)/dotnet/shared/Microsoft.WindowsDesktop.App/6.0.36"
            }
            (Self::DotnetDesktop, true) => {
                "drive_c/Program Files (x86)/dotnet/shared/Microsoft.WindowsDesktop.App"
            }
        }
    }

    fn installed(self, prefix: &Directory, custom: bool) -> bool {
        let receipt = if custom {
            matches!(
                prefix.open_file(Path::new(self.builtin_receipt())),
                Err(error) if error.kind() == io::ErrorKind::NotFound
            ) && read_custom_receipt_at(prefix, self.custom_receipt(), self.download_artifact())
                .is_ok()
        } else {
            validate_receipt_at(prefix, self.builtin_receipt(), self.builtin_sha256()).is_ok()
        };
        receipt
            && prefix
                .open_directory(Path::new(self.installed_tree(custom)))
                .is_ok()
    }
}

/// After an installer run, require its tree, then write the receipt for
/// the source used last and remove the other kind of receipt.
fn record_dependency_provenance(
    paths: &ManagedPaths,
    dependency: PrefixDependency,
    artifact: &VerifiedArtifact,
) -> Result<(), AdapterFailure> {
    let prefix = paths
        .directory()
        .open_directory(Path::new("prefix"))
        .map_err(|_| AdapterFailure::here())?;
    let custom = artifact.custom_source();
    prefix
        .open_directory(Path::new(dependency.installed_tree(custom.is_some())))
        .map_err(|_| AdapterFailure::here())?;
    match custom {
        Some(url) => {
            write_custom_receipt_at(
                &prefix,
                dependency.custom_receipt(),
                &CustomSourceReceipt::new(
                    dependency.download_artifact(),
                    url,
                    artifact.content_sha256(),
                ),
            )?;
            remove_receipt_at(&prefix, dependency.builtin_receipt())
        }
        None => {
            if artifact.content_sha256() != dependency.builtin_sha256() {
                return Err(AdapterFailure::here());
            }
            // Wine Mono's tree has no version in its path, so a Mono from a
            // custom source would satisfy it even if the pinned MSI did not
            // replace it: the registered version must be the pinned one.
            if dependency == PrefixDependency::WineMono && !pinned_mono_registered(&prefix)? {
                return Err(AdapterFailure::here());
            }
            write_receipt_last_at(
                &prefix,
                dependency.builtin_receipt(),
                dependency.builtin_sha256(),
            )?;
            remove_receipt_at(&prefix, dependency.custom_receipt())
        }
    }
}

/// The pinned Wine Mono version as its MSI registers it.
const PINNED_MONO_VERSION: &str = "11.3.0";
const MAX_SYSTEM_REGISTRY_BYTES: u64 = 128 * 1024 * 1024;

/// Whether `system.reg` registers Wine Mono Runtime only at the pinned
/// version: every uninstall entry named "Wine Mono Runtime" must carry the
/// pinned `DisplayVersion`, and there must be at least one.
fn pinned_mono_registered(prefix: &Directory) -> Result<bool, AdapterFailure> {
    let file = prefix
        .open_file(Path::new("system.reg"))
        .map_err(|_| AdapterFailure::here())?;
    let mut bytes = Vec::new();
    file.take(MAX_SYSTEM_REGISTRY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AdapterFailure::here())?;
    if bytes.len() as u64 > MAX_SYSTEM_REGISTRY_BYTES {
        return Err(AdapterFailure::here());
    }
    Ok(mono_versions_pinned(&String::from_utf8_lossy(&bytes)))
}

fn mono_versions_pinned(registry: &str) -> bool {
    let mut found = false;
    let mut section_is_mono = false;
    let mut section_version: Option<&str> = None;
    let mut finish = |is_mono: bool, version: Option<&str>| -> bool {
        if is_mono {
            found = true;
            version == Some(PINNED_MONO_VERSION)
        } else {
            true
        }
    };
    for line in registry.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with('[') {
            if !finish(section_is_mono, section_version) {
                return false;
            }
            section_is_mono = false;
            section_version = None;
        } else if line == "\"DisplayName\"=\"Wine Mono Runtime\"" {
            section_is_mono = true;
        } else if let Some(version) = line
            .strip_prefix("\"DisplayVersion\"=\"")
            .and_then(|rest| rest.strip_suffix('"'))
        {
            section_version = Some(version);
        }
    }
    finish(section_is_mono, section_version) && found
}

#[cfg(test)]
fn validate_receipt(path: PathBuf, expected: &str) -> Result<(), AdapterFailure> {
    let metadata = fs::symlink_metadata(&path).map_err(|_| AdapterFailure::here())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 256 {
        return Err(AdapterFailure::here());
    }
    let value = fs::read_to_string(path).map_err(|_| AdapterFailure::here())?;
    (value == expected)
        .then_some(())
        .ok_or_else(|| AdapterFailure::here())
}

#[cfg(test)]
fn write_receipt_last(directory: &Path, name: &str, value: &str) -> Result<(), AdapterFailure> {
    validate_owned_directory(directory)?;
    let final_path = directory.join(name);
    if final_path.exists() {
        return validate_receipt(final_path, value);
    }
    Directory::open(directory)
        .and_then(|directory| directory.atomic_write(OsStr::new(name), value.as_bytes()))
        .map_err(|_| AdapterFailure::here())
}

fn write_receipt_last_at(
    directory: &Directory,
    name: &str,
    value: &str,
) -> Result<(), AdapterFailure> {
    match validate_receipt_at(directory, name, value) {
        Ok(()) => Ok(()),
        Err(_) => directory
            .atomic_write(OsStr::new(name), value.as_bytes())
            .map_err(|_| AdapterFailure::here()),
    }
}

/// `ClientSynchronizer` backed directly by the verified updater.
pub(crate) struct UpdaterClientSynchronizer {
    source: Mutex<Box<dyn Pd2Source>>,
    sequence: AtomicU64,
}

impl UpdaterClientSynchronizer {
    /// Synchronize from `base`: the built-in PD2 origin, or a custom base
    /// whose manifest MD5s still gate every promoted object.
    pub(crate) fn with_client_base(
        base: crate::manifest::Pd2ClientBase,
    ) -> Result<Self, AdapterFailure> {
        Ok(Self {
            source: Mutex::new(Box::new(
                SystemPd2Source::with_client_base(base).map_err(|_| AdapterFailure::here())?,
            )),
            sequence: AtomicU64::new(1),
        })
    }
}

impl ClientSynchronizer for UpdaterClientSynchronizer {
    fn synchronize(
        &self,
        paths: &ManagedPaths,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        self.apply(paths, control, false)
    }

    fn accept_installed(
        &self,
        paths: &ManagedPaths,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        self.apply(paths, control, true)
    }

    fn record_copied(
        &self,
        paths: &ManagedPaths,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        let project_d2 = paths.game().join("ProjectD2");
        let relative = Path::new("ProjectD2");
        let game = paths.open_game().map_err(|_| AdapterFailure::here())?;
        let project = game
            .open_directory(relative)
            .map_err(|_| AdapterFailure::here())?;
        validate_inventory_at(project.try_clone().map_err(|_| AdapterFailure::here())?)?;
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let transaction_id = format!("import-{}-{sequence}", std::process::id());
        let mut copied = crate::update::CopiedClientPd2Source::new(&project);
        VerifiedUpdater::with_layout(
            UpdateLayout::anchored(project_d2, game, relative),
            &mut copied,
        )
        .apply_controlled(
            &transaction_id,
            GameActivity::Idle,
            ProtectedInventoryEvidence::Complete,
            control,
        )
        .map_err(|_| AdapterFailure::here())?;
        Ok(())
    }

    fn prefetch(
        &self,
        paths: &ManagedPaths,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        let cache = paths
            .directory()
            .ensure_directory(Path::new(crate::install::CLIENT_PREFETCH))
            .map_err(|_| AdapterFailure::here())?;
        // A client already installed (an existing install) is copied into
        // the cache where its files match, instead of downloaded again.
        let installed = paths
            .open_game()
            .and_then(|game| game.open_directory(Path::new("ProjectD2")))
            .ok();
        let mut source = self.source.lock().map_err(|_| AdapterFailure::here())?;
        crate::update::prefetch_client(source.as_mut(), &cache, installed.as_ref(), control)
            .map_err(|_| AdapterFailure::here())
    }
}

impl UpdaterClientSynchronizer {
    /// The client of the current manifest from the verified updater; with
    /// `as_installed`, the client as it is installed, recorded as the
    /// accepted one without a change to its files
    /// ([`crate::update::InstalledClientPd2Source`]).
    fn apply(
        &self,
        paths: &ManagedPaths,
        control: &OperationControl,
        as_installed: bool,
    ) -> Result<(), AdapterFailure> {
        let project_d2 = paths.game().join("ProjectD2");
        let relative = Path::new("ProjectD2");
        let game = paths.open_game().map_err(|_| AdapterFailure::here())?;
        let project = game
            .open_directory(relative)
            .map_err(|_| AdapterFailure::here())?;
        validate_inventory_at(project.try_clone().map_err(|_| AdapterFailure::here())?)?;
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let transaction_id = format!("install-{}-{sequence}", std::process::id());
        let mut source = self.source.lock().map_err(|_| AdapterFailure::here())?;
        // Client files the Download step verified are served from the
        // cache; only files that changed since are fetched. As installed, the
        // cache only gives the manifest when the server cannot.
        let cache = paths
            .directory()
            .open_directory(Path::new(crate::install::CLIENT_PREFETCH))
            .ok();
        let mut cached = crate::update::PrefetchedPd2Source::new(source.as_mut(), cache);
        let mut kept;
        let source: &mut dyn Pd2Source = if as_installed {
            kept = crate::update::InstalledClientPd2Source::new(&mut cached, &project);
            &mut kept
        } else {
            &mut cached
        };
        VerifiedUpdater::with_layout(UpdateLayout::anchored(project_d2, game, relative), source)
            .apply_controlled(
                &transaction_id,
                GameActivity::Idle,
                ProtectedInventoryEvidence::Complete,
                control,
            )
            .map_err(|_| AdapterFailure::here())?;
        // The cache keeps its copy: Start over and a later Download reuse
        // it instead of fetching the client again.
        Ok(())
    }
}

fn validate_inventory_at(root: Directory) -> Result<(), AdapterFailure> {
    let mut pending = vec![root];
    let mut count = 0_usize;
    while let Some(directory) = pending.pop() {
        for name in directory.names().map_err(|_| AdapterFailure::here())? {
            count = count.checked_add(1).ok_or_else(|| AdapterFailure::here())?;
            if count > MAX_INVENTORY_ENTRIES {
                return Err(AdapterFailure::here());
            }
            match directory.open_directory(Path::new(&name)) {
                Ok(child) => pending.push(child),
                Err(error) if error.kind() == io::ErrorKind::NotADirectory => {
                    let file = directory
                        .open_file(Path::new(&name))
                        .map_err(|_| AdapterFailure::here())?;
                    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
                        return Err(AdapterFailure::here());
                    }
                }
                Err(_) => return Err(AdapterFailure::here()),
            }
        }
    }
    Ok(())
}

fn validate_owned_directory(path: &Path) -> Result<(), AdapterFailure> {
    let metadata = fs::symlink_metadata(path).map_err(|_| AdapterFailure::here())?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(AdapterFailure::here())
    }
}

fn validated_directory_environment(name: &str) -> Result<OsString, AdapterFailure> {
    let value = env::var_os(name).ok_or_else(|| AdapterFailure::here())?;
    let path = Path::new(&value);
    if path.is_absolute() {
        validate_owned_directory(path)?;
        Ok(value)
    } else {
        Err(AdapterFailure::here())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        io::Cursor,
        os::unix::fs::symlink,
        sync::{
            atomic::{AtomicBool, AtomicUsize},
            mpsc::SyncSender,
        },
    };
    use xz2::write::XzEncoder;

    use super::*;
    use crate::host_execution::{ProcessExit, SupervisedProcess};

    struct FakeHttp {
        response: Mutex<Option<ArtifactHttpResponse>>,
    }

    impl ArtifactHttp for FakeHttp {
        fn get(
            &self,
            _spec: &ArtifactSpec,
            _range_start: Option<u64>,
            _control: &OperationControl,
        ) -> Result<ArtifactHttpResponse, AdapterFailure> {
            self.response
                .lock()
                .map_err(|_| AdapterFailure::here())?
                .take()
                .ok_or_else(|| AdapterFailure::here())
        }
    }

    struct SequenceHttp {
        responses: Mutex<VecDeque<ArtifactHttpResponse>>,
        ranges: Arc<Mutex<Vec<Option<u64>>>>,
    }

    impl ArtifactHttp for SequenceHttp {
        fn get(
            &self,
            _spec: &ArtifactSpec,
            range_start: Option<u64>,
            _control: &OperationControl,
        ) -> Result<ArtifactHttpResponse, AdapterFailure> {
            self.ranges
                .lock()
                .map_err(|_| AdapterFailure::here())?
                .push(range_start);
            self.responses
                .lock()
                .map_err(|_| AdapterFailure::here())?
                .pop_front()
                .ok_or_else(|| AdapterFailure::here())
        }
    }

    struct StalledHttp {
        entered: Mutex<Option<SyncSender<()>>>,
    }

    impl ArtifactHttp for StalledHttp {
        fn get(
            &self,
            _spec: &ArtifactSpec,
            _range_start: Option<u64>,
            control: &OperationControl,
        ) -> Result<ArtifactHttpResponse, AdapterFailure> {
            if let Some(entered) = self
                .entered
                .lock()
                .map_err(|_| AdapterFailure::here())?
                .take()
            {
                let _ = entered.send(());
            }
            while !control.is_cancelled() {
                thread::sleep(Duration::from_millis(1));
            }
            Err(AdapterFailure::here())
        }
    }

    fn fixed_fixture_spec(bytes: &'static [u8]) -> ArtifactSpec {
        let digest = Box::leak(hex(&Sha256::digest(bytes)).into_boxed_str());
        ArtifactSpec {
            kind: ArtifactKind::Gecko,
            request_url: Cow::Borrowed("https://fixtures.invalid/gecko"),
            redirects: RedirectPolicy::None,
            size: SizePolicy::Exact(bytes.len() as u64),
            integrity: IntegrityPolicy::Sha256(digest),
            expected_filename: None,
            fallback_urls: &[],
        }
    }

    fn wrapper_fixture_spec() -> ArtifactSpec {
        ArtifactSpec {
            kind: ArtifactKind::BaseGameWrapper,
            request_url: Cow::Borrowed("https://fixtures.invalid/d2-wrapper"),
            redirects: RedirectPolicy::None,
            size: SizePolicy::Bounded { min: 64, max: 256 },
            integrity: IntegrityPolicy::PeExecutable,
            expected_filename: Some("Diablo-II-Setup.exe"),
            fallback_urls: &[],
        }
    }

    fn verified_test_artifact(kind: ArtifactKind, path: PathBuf) -> VerifiedArtifact {
        let bytes = minimal_pe();
        fs::write(&path, &bytes).unwrap();
        let digest = hex(&Sha256::digest(&bytes));
        VerifiedArtifact {
            kind,
            path,
            origin: ArtifactOrigin::ApprovedNetwork,
            observed_sha256: digest.clone(),
            retained_receipt_sha256: volatile_wrapper(kind).then_some(digest),
            requested_url: "https://fixture.invalid/artifact".to_owned(),
            final_url: "https://fixture.invalid/artifact".to_owned(),
            custom_source: None,
        }
    }

    fn minimal_pe() -> Vec<u8> {
        let mut bytes = vec![0_u8; 128];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&64_u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes
    }

    fn release_sized_pe() -> Vec<u8> {
        let mut bytes = minimal_pe();
        bytes.resize(1_000_000, 0);
        bytes
    }

    fn write_installer_media(directory: &Path) {
        let bytes = release_sized_pe();
        for filename in [
            "Downloader_Diablo2_enUS.exe",
            "Downloader_Diablo2_Lord_of_Destruction_enUS.exe",
        ] {
            fs::write(directory.join(filename), &bytes).unwrap();
        }
    }

    #[test]
    fn discard_downloads_removes_only_the_launchers_own_downloads() {
        let root = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(root.path()).unwrap();
        let cache = root.path().join("cache/artifacts");
        fs::create_dir_all(&cache).unwrap();
        let receipt = |origin: ArtifactOrigin| {
            serde_json::to_vec(&CacheReceipt {
                schema: CACHE_RECEIPT_SCHEMA,
                origin,
                requested_url: String::new(),
                final_url: String::new(),
                attachment: String::new(),
                observed_size: 1,
                observed_sha256: String::new(),
            })
            .unwrap()
        };
        // Downloaded from Blizzard: goes. Chosen from the player's folder:
        // stays.
        fs::write(cache.join("d2-wrapper.verified"), b"downloaded").unwrap();
        fs::write(
            cache.join("d2-wrapper.verified.provenance.json"),
            receipt(ArtifactOrigin::ApprovedNetwork),
        )
        .unwrap();
        fs::write(cache.join("lod-wrapper.verified"), b"from a folder").unwrap();
        fs::write(
            cache.join("lod-wrapper.verified.provenance.json"),
            receipt(ArtifactOrigin::UserSelected),
        )
        .unwrap();
        for ours in [
            "pd2-bootstrap.verified",
            "custom-pd2-installer.download",
            "custom-pd2-installer.download.provenance.json",
            "artifact-02.partial",
            "artifact-05.partial",
            "artifact-05.partial.source",
            "d2-payload.direct",
        ] {
            fs::write(cache.join(ours), b"ours").unwrap();
        }
        for folder in ["d2-payload", "d2-payload.partial", "lod-payload"] {
            fs::create_dir_all(cache.join(folder)).unwrap();
            fs::write(cache.join(folder).join("Installer.exe"), b"x").unwrap();
        }
        // Imported, and the Wine runtime: stay.
        fs::write(cache.join("wine-gecko-2.47.4-x86.verified"), b"kept").unwrap();
        fs::create_dir_all(root.path().join("cache/client/objects")).unwrap();
        fs::write(root.path().join("cache/client/objects/a"), b"pd2").unwrap();
        fs::write(root.path().join("cache/downloads.json"), b"{}").unwrap();

        discard_downloads(&paths).unwrap();
        for gone in [
            "d2-wrapper.verified",
            "d2-wrapper.verified.provenance.json",
            "pd2-bootstrap.verified",
            "custom-pd2-installer.download",
            "custom-pd2-installer.download.provenance.json",
            "artifact-02.partial",
            "artifact-05.partial",
            "artifact-05.partial.source",
            "d2-payload",
            "d2-payload.partial",
            "d2-payload.direct",
        ] {
            assert!(!cache.join(gone).exists(), "{gone}");
        }
        for kept in [
            "lod-wrapper.verified",
            "lod-wrapper.verified.provenance.json",
            "lod-payload/Installer.exe",
            "wine-gecko-2.47.4-x86.verified",
        ] {
            assert!(cache.join(kept).exists(), "{kept}");
        }
        assert!(!root.path().join("cache/client").exists());
        assert!(!root.path().join("cache/downloads.json").exists());
        // Nothing left: a second discard does nothing.
        discard_downloads(&paths).unwrap();
    }

    #[test]
    fn a_full_check_is_trusted_in_the_same_process_only() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("Installer Tome.mpq");
        fs::write(&path, b"tome").unwrap();
        let metadata = fs::metadata(&path).unwrap();
        let checks = SessionChecks::new();
        assert!(!checks.trusts(&metadata, "sha"));
        checks.record(&metadata, "sha");
        // The same process, the same file: trusted.
        assert!(checks.trusts(&metadata, "sha"));
        assert!(!checks.trusts(&metadata, "other"));
        // A new process (a new record): checked again.
        assert!(!SessionChecks::new().trusts(&metadata, "sha"));
        // A write changes the file: checked again.
        std::thread::sleep(Duration::from_millis(20));
        fs::write(&path, b"TOME").unwrap();
        assert!(!checks.trusts(&fs::metadata(&path).unwrap(), "sha"));

        // The payload check uses it: a Tome this process checked is not
        // read again (here its recorded SHA-256 is not its own, to show it).
        let directory = Directory::open(folder.path()).unwrap();
        let tome = fs::metadata(&path).unwrap();
        assert_eq!(
            check_complete_payload_file(
                &directory,
                "",
                "Installer Tome.mpq",
                4,
                "recorded",
                false,
                &|| false
            )
            .unwrap_err()
            .check,
            "sha256"
        );
        SESSION_CHECKS.record(&tome, "recorded");
        assert_eq!(
            check_complete_payload_file(
                &directory,
                "",
                "Installer Tome.mpq",
                4,
                "recorded",
                false,
                &|| false
            ),
            Ok(())
        );
        // The program is always hashed again.
        assert!(
            check_complete_payload_file(
                &directory,
                "",
                "Installer Tome.mpq",
                4,
                "recorded",
                true,
                &|| false
            )
            .is_err()
        );
    }

    #[test]
    fn installer_media_requires_both_exact_official_file_names_and_valid_pe_files() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("Downloader_Diablo2_enUS.exe"),
            release_sized_pe(),
        )
        .unwrap();
        assert!(InstallerMedia::validate(directory.path().to_path_buf()).is_err());

        fs::write(
            directory
                .path()
                .join("Downloader_Diablo2_Lord_of_Destruction_enUS.exe"),
            vec![0_u8; 1_000_000],
        )
        .unwrap();
        assert!(InstallerMedia::validate(directory.path().to_path_buf()).is_err());

        write_installer_media(directory.path());
        assert_eq!(
            InstallerMedia::validate(directory.path().to_path_buf())
                .unwrap()
                .directory(),
            directory.path()
        );
    }

    #[test]
    fn complete_payload_validation_requires_exact_exe_tome_identity_and_inventory() {
        let directory = tempfile::tempdir().unwrap();
        let installer = minimal_pe();
        let tome = b"fixture installer tome";
        let directx = b"fixture DirectX";
        fs::write(directory.path().join(PAYLOAD_INSTALLER), &installer).unwrap();
        fs::write(directory.path().join(PAYLOAD_TOME), tome).unwrap();
        fs::create_dir(directory.path().join(DIRECTX_DIRECTORY)).unwrap();
        fs::write(
            directory.path().join(DIRECTX_DIRECTORY).join("fixture.cab"),
            directx,
        )
        .unwrap();
        let directx_files = Box::leak(
            vec![PayloadFileIdentity::new(
                "fixture.cab",
                directx.len() as u64,
                Box::leak(hex(&Sha256::digest(directx)).into_boxed_str()),
            )]
            .into_boxed_slice(),
        );
        let identity = CompletePayloadIdentity {
            source_directory: "fixture-source",
            cache_directory: "fixture-cache",
            installer_size: installer.len() as u64,
            installer_sha256: Box::leak(hex(&Sha256::digest(&installer)).into_boxed_str()),
            tome_size: tome.len() as u64,
            tome_sha256: Box::leak(hex(&Sha256::digest(tome)).into_boxed_str()),
            directx_files,
        };
        let opened = Directory::open(directory.path()).unwrap();
        validate_complete_payload_directory(&opened, identity, false).unwrap();

        fs::create_dir(directory.path().join("Installer.app")).unwrap();
        validate_complete_payload_directory(&opened, identity, true).unwrap();
        assert!(validate_complete_payload_directory(&opened, identity, false).is_err());
        fs::remove_dir(directory.path().join("Installer.app")).unwrap();

        fs::write(directory.path().join(PAYLOAD_TOME), b"tampered").unwrap();
        assert!(validate_complete_payload_directory(&opened, identity, false).is_err());
        fs::write(directory.path().join(PAYLOAD_TOME), tome).unwrap();
        fs::write(directory.path().join("unexpected"), b"preserve").unwrap();
        assert!(validate_complete_payload_directory(&opened, identity, false).is_err());
        fs::remove_file(directory.path().join("unexpected")).unwrap();

        // Each failure names its file and check: an unfinished download
        // (missing, partial) apart from finished files that differ.
        let check = || check_complete_payload_directory(&opened, identity, false).unwrap_err();
        fs::write(directory.path().join(PAYLOAD_TOME), b"wrong size").unwrap();
        let problem = check();
        assert_eq!(
            (problem.file.as_str(), problem.check, problem.size),
            (PAYLOAD_TOME, "size", Some(10))
        );
        assert!(!problem.incomplete());
        fs::write(
            directory.path().join(PAYLOAD_TOME),
            b"fixture installer tomb",
        )
        .unwrap();
        assert_eq!(check(), PayloadProblem::new(PAYLOAD_TOME, "sha256"));
        fs::remove_file(directory.path().join(PAYLOAD_TOME)).unwrap();
        fs::write(directory.path().join("Installer Tome.mpq.part"), b"half").unwrap();
        let problem = check();
        assert_eq!(
            problem,
            PayloadProblem::new("Installer Tome.mpq.part", "partial")
        );
        assert!(problem.incomplete());
        fs::remove_file(directory.path().join("Installer Tome.mpq.part")).unwrap();
        assert_eq!(check(), PayloadProblem::new(PAYLOAD_TOME, "missing"));
        fs::write(directory.path().join(PAYLOAD_TOME), tome).unwrap();
        fs::write(
            directory.path().join(DIRECTX_DIRECTORY).join("fixture.cab"),
            b"fixture DirectY",
        )
        .unwrap();
        assert_eq!(
            check(),
            PayloadProblem::new(format!("{DIRECTX_DIRECTORY}/fixture.cab"), "sha256")
        );
    }

    #[test]
    fn complete_payload_profile_uses_only_the_products_key1_and_fixed_owner() {
        let arguments = complete_payload_arguments("SECRET-LOD-KEY-CANARY");
        assert_eq!(
            arguments,
            [
                OsString::from("--user=Player"),
                OsString::from("--key1=SECRET-LOD-KEY-CANARY"),
                OsString::from("--destination=C:\\Program Files (x86)\\Diablo II"),
                OsString::from("--silent=true"),
                OsString::from("--noui=true"),
                OsString::from("--skippostlaunch=true"),
                OsString::from("--nopatchnotes=true"),
            ]
        );
        assert!(
            arguments
                .iter()
                .all(|argument| !argument.to_string_lossy().contains("--key2"))
        );
    }

    #[test]
    fn vendor_ui_helper_has_the_frozen_sealed_pe_identity() {
        // Frozen deliberately: the helper waits a minute for the license
        // dialog after the installer's own window closes (five seconds once
        // it was answered), and exits 21 when no reviewed
        // window appeared within ten minutes, so an installer that ended
        // without a window fails its stage instead of running into the
        // two-hour stage bound. Its generic `Installer` error dialog exits
        // 22 when the dialog's text says the product key was rejected ("The
        // information provided was not correct.", or a text naming a key)
        // and 20 otherwise; the text itself never leaves the helper. A main
        // menu left waiting (the license or destination dialog declined) is
        // clicked on "Install" up to three times before the license was
        // answered, and exits 23 once it keeps waiting (a minute after the
        // last click, or ten minutes after the license was answered).
        assert_eq!(VENDOR_UI_HELPER_SIZE, 4_096);
        assert_eq!(
            VENDOR_UI_HELPER_SHA256,
            "ea405393bfe53f0c90a6ac264c02b53991515fb4ac2158d85886189e550998af"
        );
        let mut helper = UiHelper::Vendor.sealed_file().unwrap();
        let mut bytes = Vec::new();
        helper.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len(), VENDOR_UI_HELPER_SIZE);
        assert_eq!(&bytes[..2], b"MZ");
        assert_eq!(hex(&Sha256::digest(bytes)), VENDOR_UI_HELPER_SHA256);
    }

    #[test]
    fn a_cluttered_downloads_folder_imports_only_the_two_official_wrappers() {
        // A folder like ~/Downloads: the two wrappers among unrelated files,
        // other programs, folders, and a link. Only the two exact names are
        // opened, validated, and copied; nothing else is read or kept.
        let selected = tempfile::tempdir().unwrap();
        write_installer_media(selected.path());
        fs::write(selected.path().join("setup.exe"), release_sized_pe()).unwrap();
        fs::write(selected.path().join("notes.txt"), b"unrelated").unwrap();
        fs::write(
            selected.path().join("Downloader_Diablo2_enUS (1).exe"),
            release_sized_pe(),
        )
        .unwrap();
        fs::create_dir(selected.path().join("D2-1.14b-Installer-enUS")).unwrap();
        std::os::unix::fs::symlink("/etc", selected.path().join("link")).unwrap();
        let media = InstallerMedia::validate(selected.path().to_path_buf()).unwrap();
        assert_eq!(media.layout(), InstallerMediaLayout::LegacyWrappers);

        let cache = tempfile::tempdir().unwrap();
        let catalog = ArtifactCatalog::pinned();
        for kind in [
            ArtifactKind::BaseGameWrapper,
            ArtifactKind::ExpansionWrapper,
        ] {
            ProductionArtifactSource::with_installer_media(media.clone())
                .acquire(
                    catalog.spec(kind),
                    cache.path(),
                    &OperationControl::default(),
                )
                .unwrap();
        }
        let mut imported = fs::read_dir(cache.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| !name.starts_with('.') && !name.ends_with(".provenance.json"))
            .collect::<Vec<_>>();
        imported.sort();
        assert_eq!(imported, ["d2-wrapper.verified", "lod-wrapper.verified"]);

        // Without both official files the folder is refused, however much
        // else it holds.
        fs::remove_file(
            selected
                .path()
                .join("Downloader_Diablo2_Lord_of_Destruction_enUS.exe"),
        )
        .unwrap();
        assert!(InstallerMedia::validate(selected.path().to_path_buf()).is_err());
    }

    #[test]
    fn selected_installer_is_imported_and_reused_from_launcher_owned_cache() {
        let selected = tempfile::tempdir().unwrap();
        write_installer_media(selected.path());
        let media = InstallerMedia::validate(selected.path().to_path_buf()).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let catalog = ArtifactCatalog::pinned();
        let spec = catalog.spec(ArtifactKind::BaseGameWrapper);

        let acquired = ProductionArtifactSource::with_installer_media(media.clone())
            .acquire(spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(acquired.origin, ArtifactOrigin::UserSelected);
        assert_eq!(acquired.requested_url, "");
        assert_eq!(acquired.final_url, "");
        assert_eq!(fs::read(&acquired.path).unwrap(), release_sized_pe());
        assert_ne!(acquired.path.parent().unwrap(), selected.path());

        let verified = verify_artifact(spec, cache.path(), acquired).unwrap();
        assert_eq!(verified.origin(), ArtifactOrigin::UserSelected);

        // The same folder reuses the imported object without a download.
        let reused = ProductionArtifactSource {
            http: Box::new(FakeHttp {
                response: Mutex::new(None),
            }),
            retry_policy: RetryPolicy::immediate_for_tests(),
            installer_media: Some(media),
            cache_only: false,
            wrapper_refetch: false,
        }
        .acquire(spec, cache.path(), &OperationControl::default())
        .unwrap();
        assert_eq!(reused.origin, ArtifactOrigin::UserSelected);
        assert_eq!(reused.requested_url, "");
        assert_eq!(reused.final_url, "");

        let receipt: CacheReceipt =
            serde_json::from_slice(&fs::read(cache.path().join(cache_receipt_name(spec))).unwrap())
                .unwrap();
        assert_eq!(receipt.schema, CACHE_RECEIPT_SCHEMA);
        assert_eq!(receipt.origin, ArtifactOrigin::UserSelected);
    }

    #[test]
    fn a_cached_wrapper_is_reused_only_for_the_installer_choice_that_made_it() {
        let catalog = ArtifactCatalog::pinned();
        let spec = catalog.spec(ArtifactKind::BaseGameWrapper);
        let filename = "Downloader_Diablo2_enUS.exe";
        let cache = tempfile::tempdir().unwrap();
        let with_media = |media: Option<InstallerMedia>, http: FakeHttp| ProductionArtifactSource {
            http: Box::new(http),
            retry_policy: RetryPolicy::immediate_for_tests(),
            installer_media: media,
            cache_only: false,
            wrapper_refetch: false,
        };
        let offline = || FakeHttp {
            response: Mutex::new(None),
        };
        let variant = |marker: u8| {
            let mut bytes = release_sized_pe();
            bytes[500] = marker;
            bytes
        };

        // Folder A is imported.
        let folder_a = tempfile::tempdir().unwrap();
        write_installer_media(folder_a.path());
        fs::write(folder_a.path().join(filename), variant(0xa)).unwrap();
        let media_a = InstallerMedia::validate(folder_a.path().to_path_buf()).unwrap();
        with_media(Some(media_a.clone()), offline())
            .acquire(spec, cache.path(), &OperationControl::default())
            .unwrap();
        // The same folder reuses the cache without another copy.
        let reused = with_media(Some(media_a), offline())
            .acquire(spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(fs::read(&reused.path).unwrap(), variant(0xa));

        // Folder B holds other bytes: the stale entry is replaced by B's.
        let folder_b = tempfile::tempdir().unwrap();
        write_installer_media(folder_b.path());
        fs::write(folder_b.path().join(filename), variant(0xb)).unwrap();
        let media_b = InstallerMedia::validate(folder_b.path().to_path_buf()).unwrap();
        let switched = with_media(Some(media_b.clone()), offline())
            .acquire(spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(switched.origin, ArtifactOrigin::UserSelected);
        assert_eq!(fs::read(&switched.path).unwrap(), variant(0xb));

        // Choosing the Blizzard download never runs a folder's wrapper.
        let downloaded = with_media(
            None,
            served(spec.request_url(), &variant(0xc), Some(filename)),
        )
        .acquire(spec, cache.path(), &OperationControl::default())
        .unwrap();
        assert_eq!(downloaded.origin, ArtifactOrigin::ApprovedNetwork);
        assert_eq!(fs::read(&downloaded.path).unwrap(), variant(0xc));
        let reused = with_media(None, offline())
            .acquire(spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(reused.origin, ArtifactOrigin::ApprovedNetwork);

        // And a folder chosen after a download is imported again.
        let back = with_media(Some(media_b), offline())
            .acquire(spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(back.origin, ArtifactOrigin::UserSelected);
        assert_eq!(fs::read(&back.path).unwrap(), variant(0xb));
    }

    #[test]
    fn an_expired_or_discarded_blizzard_wrapper_is_downloaded_again() {
        let catalog = ArtifactCatalog::pinned();
        let spec = catalog.spec(ArtifactKind::BaseGameWrapper);
        let filename = "Downloader_Diablo2_enUS.exe";
        let cache = tempfile::tempdir().unwrap();
        let cache_directory = Directory::open(cache.path()).unwrap();
        let source = |http: FakeHttp| ProductionArtifactSource {
            http: Box::new(http),
            retry_policy: RetryPolicy::immediate_for_tests(),
            installer_media: None,
            cache_only: false,
            wrapper_refetch: false,
        };
        let variant = |marker: u8| {
            let mut bytes = release_sized_pe();
            bytes[500] = marker;
            bytes
        };
        let fetch = |marker: u8| {
            source(served(spec.request_url(), &variant(marker), Some(filename)))
                .acquire(spec, cache.path(), &OperationControl::default())
                .unwrap()
        };
        let offline = || {
            source(FakeHttp {
                response: Mutex::new(None),
            })
            .acquire(spec, cache.path(), &OperationControl::default())
        };

        let first = fetch(0x1);
        assert_eq!(fs::read(&offline().unwrap().path).unwrap(), variant(0x1));

        // Discarded after its download failed: the next acquisition fetches.
        discard_cached_wrapper(spec, &cache_directory).unwrap();
        assert!(offline().is_err(), "nothing cached is reused");
        let refetched = fetch(0x2);
        assert_eq!(refetched.origin, ArtifactOrigin::ApprovedNetwork);
        assert_eq!(fs::read(&refetched.path).unwrap(), variant(0x2));

        // Older than its download authorization: fetched again, not run.
        let week_old = SystemTime::now() - Duration::from_secs(7 * 24 * 60 * 60);
        File::options()
            .write(true)
            .open(&first.path)
            .unwrap()
            .set_modified(week_old)
            .unwrap();
        assert!(offline().is_err(), "an expired wrapper is not reused");
        assert_eq!(fs::read(&fetch(0x3).path).unwrap(), variant(0x3));
        assert_eq!(fs::read(&offline().unwrap().path).unwrap(), variant(0x3));

        // Only a built-in wrapper can be discarded.
        let custom = ArtifactSpec {
            integrity: IntegrityPolicy::Unverified,
            ..spec.clone()
        };
        assert!(discard_cached_wrapper(&custom, &cache_directory).is_err());
        assert!(
            discard_cached_wrapper(
                catalog.spec(ArtifactKind::ClientBootstrap),
                &cache_directory
            )
            .is_err()
        );
    }

    #[test]
    fn seeding_copies_only_pinned_downloads_that_still_verify() {
        let good = b"verified gecko fixture";
        let gecko = fixed_fixture_spec(good);
        let bootstrap = ArtifactSpec {
            kind: ArtifactKind::ClientBootstrap,
            request_url: Cow::Borrowed("https://fixtures.invalid/bootstrap"),
            ..fixed_fixture_spec(b"verified bootstrap fixture")
        };
        let mut specs = ArtifactCatalog::pinned().specs;
        for spec in &mut specs {
            if spec.kind == ArtifactKind::Gecko {
                *spec = gecko.clone();
            } else if spec.kind == ArtifactKind::ClientBootstrap {
                *spec = bootstrap.clone();
            }
        }
        let catalog = ArtifactCatalog::new(specs);
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        let old_cache = old.path().join("cache/artifacts");
        fs::create_dir_all(&old_cache).unwrap();
        fs::write(old_cache.join(cache_name(&gecko)), good).unwrap();
        // A damaged copy of the bootstrap never reaches the new cache.
        fs::write(
            old_cache.join(cache_name(&bootstrap)),
            b"damaged bootstrap fixtur",
        )
        .unwrap();
        fs::write(old_cache.join("d2-wrapper.verified"), minimal_pe()).unwrap();

        let seeded = seed_verified_cache(
            &catalog,
            &Directory::open(old.path()).unwrap(),
            &Directory::open(new.path()).unwrap(),
            &OperationControl::default(),
        )
        .unwrap();
        assert_eq!(seeded, [ArtifactKind::Gecko]);
        let new_cache = new.path().join("cache/artifacts");
        assert_eq!(fs::read(new_cache.join(cache_name(&gecko))).unwrap(), good);
        assert!(!new_cache.join(cache_name(&bootstrap)).exists());
        assert!(!new_cache.join("d2-wrapper.verified").exists());
        // The old root is untouched.
        assert_eq!(fs::read(old_cache.join(cache_name(&gecko))).unwrap(), good);
        // The seeded entry is reused without a download.
        let reused = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(None),
        })
        .acquire(&gecko, &new_cache, &OperationControl::default())
        .unwrap();
        assert_eq!(reused.path, new_cache.join(cache_name(&gecko)));
    }

    #[test]
    fn acquisition_rejects_origin_overflow_and_tampered_cache() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"verified fixture";
        let spec = fixed_fixture_spec(bytes);
        let source = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(Some(ArtifactHttpResponse {
                final_url: "https://mirror.invalid/gecko".to_owned(),
                status: 200,
                declared_length: Some(bytes.len() as u64),
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(Cursor::new(bytes.to_vec())),
            })),
        });
        assert!(
            source
                .acquire(&spec, cache.path(), &OperationControl::default())
                .is_err()
        );

        let source = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(Some(ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 200,
                declared_length: Some(bytes.len() as u64 + 1),
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(Cursor::new(bytes.to_vec())),
            })),
        });
        assert!(
            source
                .acquire(&spec, cache.path(), &OperationControl::default())
                .is_err()
        );

        let final_path = cache.path().join(cache_name(&spec));
        fs::write(&final_path, b"tampered fixture").unwrap();
        let source = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(None),
        });
        assert!(
            source
                .acquire(&spec, cache.path(), &OperationControl::default())
                .is_err()
        );
    }

    #[test]
    fn acquisition_promotes_only_a_complete_verified_partial_and_reuses_it() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"verified fixture";
        let spec = fixed_fixture_spec(bytes);
        let source = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(Some(ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 200,
                declared_length: Some(bytes.len() as u64),
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(Cursor::new(bytes.to_vec())),
            })),
        });
        let acquired = source
            .acquire(&spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(fs::read(&acquired.path).unwrap(), bytes);
        assert_eq!(public_cache_entries(cache.path()).len(), 1);

        let cached = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(None),
        })
        .acquire(&spec, cache.path(), &OperationControl::default())
        .unwrap();
        assert_eq!(cached.path, acquired.path);
    }

    #[test]
    fn the_install_steps_use_only_verified_downloads_already_in_the_cache() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"verified fixture";
        let spec = fixed_fixture_spec(bytes);
        let response = || {
            Mutex::new(Some(ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 200,
                declared_length: Some(bytes.len() as u64),
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(Cursor::new(bytes.to_vec())),
            }))
        };
        // Nothing in the cache: a cache-only source downloads nothing, even
        // with the network available.
        let cache_only = ProductionArtifactSource::with_http(FakeHttp {
            response: response(),
        })
        .cache_only();
        assert!(
            cache_only
                .acquire(&spec, cache.path(), &OperationControl::default())
                .is_err()
        );
        assert!(public_cache_entries(cache.path()).is_empty());
        // What the Download step verified is served from the cache.
        let downloaded = ProductionArtifactSource::with_http(FakeHttp {
            response: response(),
        })
        .acquire(&spec, cache.path(), &OperationControl::default())
        .unwrap();
        let served = cache_only
            .acquire(&spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(served.path, downloaded.path);
        // A downloader wrapper is never fetched by an install step.
        let wrapper = wrapper_fixture_spec();
        assert!(
            ProductionArtifactSource::with_http(FakeHttp {
                response: Mutex::new(None),
            })
            .cache_only()
            .acquire(&wrapper, cache.path(), &OperationControl::default())
            .is_err()
        );
    }

    #[test]
    fn on_android_an_install_step_reuses_or_refetches_blizzards_wrapper() {
        let spec = ArtifactCatalog::pinned()
            .spec(ArtifactKind::BaseGameWrapper)
            .clone();
        let filename = "Downloader_Diablo2_enUS.exe";
        let cache = tempfile::tempdir().unwrap();
        let variant = |marker: u8| {
            let mut bytes = release_sized_pe();
            bytes[500] = marker;
            bytes
        };
        let install_step = |http: FakeHttp| {
            ProductionArtifactSource::with_http(http)
                .cache_only()
                .with_wrapper_refetch()
        };
        let offline = || FakeHttp {
            response: Mutex::new(None),
        };
        // The wrapper Download verified is served without the network.
        let downloaded = ProductionArtifactSource::with_http(served(
            spec.request_url(),
            &variant(0x1),
            Some(filename),
        ))
        .acquire(&spec, cache.path(), &OperationControl::default())
        .unwrap();
        let reused = install_step(offline())
            .acquire(&spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(reused.path, downloaded.path);
        assert_eq!(fs::read(&reused.path).unwrap(), variant(0x1));
        // The desktop's cache-only source never serves a wrapper.
        assert!(
            ProductionArtifactSource::with_http(offline())
                .cache_only()
                .acquire(&spec, cache.path(), &OperationControl::default())
                .is_err()
        );
        // Expired: downloaded again once.
        let week_old = SystemTime::now() - Duration::from_secs(7 * 24 * 60 * 60);
        File::options()
            .write(true)
            .open(&downloaded.path)
            .unwrap()
            .set_modified(week_old)
            .unwrap();
        let refetched = install_step(served(spec.request_url(), &variant(0x2), Some(filename)))
            .acquire(&spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(refetched.origin, ArtifactOrigin::ApprovedNetwork);
        assert_eq!(fs::read(&refetched.path).unwrap(), variant(0x2));
        // Anything else stays cache-only.
        let bytes = b"verified fixture";
        assert!(
            install_step(offline())
                .acquire(
                    &fixed_fixture_spec(bytes),
                    cache.path(),
                    &OperationControl::default()
                )
                .is_err()
        );
    }

    #[test]
    fn a_successful_acquisition_leaves_no_empty_partial_slot() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"verified fixture";
        let spec = fixed_fixture_spec(bytes);
        let slot = cache.path().join(artifact_partial_name(spec.kind));
        ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(Some(ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 200,
                declared_length: Some(bytes.len() as u64),
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(Cursor::new(bytes.to_vec())),
            })),
        })
        .acquire(&spec, cache.path(), &OperationControl::default())
        .unwrap();
        assert!(!slot.exists(), "the slot is removed after success");
        assert!(!cache.path().join(partial_binding_name(spec.kind)).exists());

        // Empty slots older releases left are swept by the next
        // acquisition; a slot with bytes is a resumable partial and stays.
        let other = cache
            .path()
            .join(artifact_partial_name(ArtifactKind::ClientBootstrap));
        fs::write(&other, b"").unwrap();
        fs::write(&slot, b"").unwrap();
        let reuse = || {
            ProductionArtifactSource::with_http(FakeHttp {
                response: Mutex::new(None),
            })
            .acquire(&spec, cache.path(), &OperationControl::default())
            .unwrap()
        };
        reuse();
        assert!(!slot.exists());
        assert!(!other.exists());
        fs::write(&slot, b"verif").unwrap();
        reuse();
        assert_eq!(fs::read(&slot).unwrap(), b"verif");
    }

    #[test]
    fn an_interrupted_acquisition_publishes_nothing_and_keeps_its_bound_partial() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"verified fixture";
        let spec = fixed_fixture_spec(bytes);
        let source = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(Some(ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 200,
                declared_length: Some(bytes.len() as u64),
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(InterruptedBody {
                    prefix: Cursor::new(bytes[..5].to_vec()),
                }),
            })),
        });

        assert!(
            source
                .acquire(&spec, cache.path(), &OperationControl::default())
                .is_err()
        );
        assert!(!cache.path().join(cache_name(&spec)).exists());
        assert_eq!(kept_partial(cache.path(), &spec), Some(bytes[..5].to_vec()));
    }

    /// The kept partial of `spec`'s slot and its source binding, if any.
    fn kept_partial(cache: &Path, spec: &ArtifactSpec) -> Option<Vec<u8>> {
        let partial = fs::read(cache.join(artifact_partial_name(spec.kind))).ok()?;
        let binding = read_partial_binding(&Directory::open(cache).unwrap(), spec.kind)?;
        assert_eq!(binding.fingerprint, partial_fingerprint(spec));
        (!partial.is_empty()).then_some(partial)
    }

    #[test]
    fn an_empty_wrapper_answer_is_getlegacys_rate_limit_and_is_asked_again() {
        let catalog = ArtifactCatalog::pinned();
        let spec = catalog.spec(ArtifactKind::BaseGameWrapper);
        let filename = "Downloader_Diablo2_enUS.exe";
        let answer = |bytes: Vec<u8>| ArtifactHttpResponse {
            final_url: spec.request_url().to_owned(),
            status: 200,
            declared_length: Some(bytes.len() as u64),
            content_range: None,
            suggested_filename: Some(filename.to_owned()),
            retry_after: None,
            body: Box::new(Cursor::new(bytes)),
        };
        let cache = tempfile::tempdir().unwrap();
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let acquired = ProductionArtifactSource::with_http(SequenceHttp {
            responses: Mutex::new(VecDeque::from([
                answer(Vec::new()),
                answer(b"slow down".to_vec()),
                answer(release_sized_pe()),
            ])),
            ranges: Arc::clone(&ranges),
        })
        .acquire(spec, cache.path(), &OperationControl::default())
        .unwrap();
        assert_eq!(fs::read(acquired.path).unwrap(), release_sized_pe());
        assert_eq!(*ranges.lock().unwrap(), [None, None, None]);

        // Only while the retry budget lasts.
        let cache = tempfile::tempdir().unwrap();
        assert!(
            ProductionArtifactSource::with_http(SequenceHttp {
                responses: Mutex::new((0..8).map(|_| answer(Vec::new())).collect()),
                ranges: Arc::default(),
            })
            .acquire(spec, cache.path(), &OperationControl::default())
            .is_err()
        );
        assert!(!cache.path().join("d2-wrapper.verified").exists());
    }

    #[test]
    fn interrupted_artifact_body_resumes_from_the_retained_partial() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"verified fixture";
        let spec = fixed_fixture_spec(bytes);
        let split = 5_u64;
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let source = ProductionArtifactSource::with_http(SequenceHttp {
            responses: Mutex::new(VecDeque::from([
                ArtifactHttpResponse {
                    final_url: spec.request_url().to_owned(),
                    status: 200,
                    declared_length: Some(bytes.len() as u64),
                    content_range: None,
                    suggested_filename: None,
                    retry_after: None,
                    body: Box::new(InterruptedBody {
                        prefix: Cursor::new(bytes[..split as usize].to_vec()),
                    }),
                },
                ArtifactHttpResponse {
                    final_url: spec.request_url().to_owned(),
                    status: 206,
                    declared_length: Some(bytes.len() as u64 - split),
                    content_range: Some(ContentRange {
                        start: split,
                        end_inclusive: bytes.len() as u64 - 1,
                        complete_length: bytes.len() as u64,
                    }),
                    suggested_filename: None,
                    retry_after: None,
                    body: Box::new(Cursor::new(bytes[split as usize..].to_vec())),
                },
            ])),
            ranges: Arc::clone(&ranges),
        });

        let acquired = source
            .acquire(&spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(fs::read(acquired.path).unwrap(), bytes);
        assert_eq!(*ranges.lock().unwrap(), vec![None, Some(split)]);
    }

    #[test]
    fn attempts_that_deliver_bytes_renew_the_retry_budget() {
        let cache = tempfile::tempdir().unwrap();
        let spec = fixed_fixture_spec(b"a slow but steadily progressing download");
        let bytes = b"a slow but steadily progressing download".to_vec();
        let total = bytes.len() as u64;
        let chunk = 3_u64;
        // Far more interrupted connections than the no-progress budget of 5,
        // each delivering a few new bytes, like a slow server behind a
        // connection that keeps dropping.
        let mut responses = VecDeque::new();
        let mut start = 0_u64;
        while start < total {
            let end = (start + chunk).min(total);
            let body: Box<dyn Read + Send> = if end == total {
                Box::new(Cursor::new(bytes[start as usize..].to_vec()))
            } else {
                Box::new(InterruptedBody {
                    prefix: Cursor::new(bytes[start as usize..end as usize].to_vec()),
                })
            };
            responses.push_back(ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: if start == 0 { 200 } else { 206 },
                declared_length: Some(total - start),
                content_range: (start > 0).then_some(ContentRange {
                    start,
                    end_inclusive: total - 1,
                    complete_length: total,
                }),
                suggested_filename: None,
                retry_after: None,
                body,
            });
            start = end;
        }
        assert!(responses.len() > RetryPolicy::immediate_for_tests().max_attempts());
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let source = ProductionArtifactSource::with_http(SequenceHttp {
            responses: Mutex::new(responses),
            ranges: Arc::clone(&ranges),
        });
        let acquired = source
            .acquire(&spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(fs::read(acquired.path).unwrap(), bytes);
        assert_eq!(ranges.lock().unwrap().len() as u64, total.div_ceil(chunk));

        // Interruptions that deliver nothing still exhaust the small budget.
        let cache = tempfile::tempdir().unwrap();
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let stalled = (0..8)
            .map(|_| ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 200,
                declared_length: Some(total),
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(InterruptedBody {
                    prefix: Cursor::new(Vec::new()),
                }) as Box<dyn Read + Send>,
            })
            .collect();
        let source = ProductionArtifactSource::with_http(SequenceHttp {
            responses: Mutex::new(stalled),
            ranges: Arc::clone(&ranges),
        });
        assert!(
            source
                .acquire(&spec, cache.path(), &OperationControl::default())
                .is_err()
        );
        assert_eq!(ranges.lock().unwrap().len(), 5);
    }

    #[test]
    fn retryable_artifact_statuses_stop_at_the_small_attempt_budget() {
        let cache = tempfile::tempdir().unwrap();
        let spec = fixed_fixture_spec(b"verified fixture");
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let responses = (0..6)
            .map(|_| ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 429,
                declared_length: None,
                content_range: None,
                suggested_filename: None,
                retry_after: Some(Duration::from_secs(600)),
                body: Box::new(Cursor::new(Vec::new())) as Box<dyn Read + Send>,
            })
            .collect();
        let source = ProductionArtifactSource::with_http(SequenceHttp {
            responses: Mutex::new(responses),
            ranges: Arc::clone(&ranges),
        });

        assert!(
            source
                .acquire(&spec, cache.path(), &OperationControl::default())
                .is_err()
        );
        assert_eq!(ranges.lock().unwrap().len(), 5);
        assert!(public_cache_entries(cache.path()).is_empty());
    }

    #[test]
    fn acquired_artifact_promotes_only_the_exact_response_descriptor() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"verified fixture";
        let spec = fixed_fixture_spec(bytes);
        let source = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(Some(ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 200,
                declared_length: Some(bytes.len() as u64),
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(SubstitutePartialOnRead {
                    body: Cursor::new(bytes.to_vec()),
                    cache: cache.path().to_path_buf(),
                    substituted: false,
                }),
            })),
        });

        assert!(
            source
                .acquire(&spec, cache.path(), &OperationControl::default())
                .is_err()
        );
        assert!(!cache.path().join(cache_name(&spec)).exists());
        assert_eq!(
            fs::read(cache.path().join("retained-response.partial")).unwrap(),
            bytes
        );
        let replacement = cache
            .path()
            .read_dir()
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| {
                let name = entry.file_name();
                name == OsStr::new(artifact_partial_name(spec.kind))
            })
            .expect("attacker replacement retained");
        assert_eq!(fs::read(replacement.path()).unwrap(), b"attacker");
    }

    #[test]
    fn invalid_cache_cleanup_stays_with_retained_cache_after_ancestor_swap() {
        let fixture = tempfile::tempdir().unwrap();
        let cache = fixture.path().join("cache");
        let outside = fixture.path().join("outside");
        fs::create_dir(&cache).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(cache.join("wrapper"), b"invalid").unwrap();
        let directory = Directory::open(&cache).unwrap();
        let opened = directory.open_file(Path::new("wrapper")).unwrap();
        fs::rename(&cache, fixture.path().join("cache-away")).unwrap();
        symlink(&outside, &cache).unwrap();

        remove_regular_cache_entry(&directory, OsStr::new("wrapper"), &opened).unwrap();

        assert_eq!(
            fs::metadata(fixture.path().join("cache-away/wrapper"))
                .unwrap()
                .len(),
            0,
            "invalid cache entry is emptied in place"
        );
        assert!(fs::read_dir(outside).unwrap().next().is_none());
    }

    #[test]
    fn invalid_cache_cleanup_rejects_name_substitution_without_deleting_bytes() {
        let cache = tempfile::tempdir().unwrap();
        fs::write(cache.path().join("wrapper"), b"invalid").unwrap();
        let directory = Directory::open(cache.path()).unwrap();
        let opened = directory.open_file(Path::new("wrapper")).unwrap();
        fs::rename(
            cache.path().join("wrapper"),
            cache.path().join("invalid-away"),
        )
        .unwrap();
        fs::write(cache.path().join("wrapper"), b"attacker").unwrap();

        assert!(remove_regular_cache_entry(&directory, OsStr::new("wrapper"), &opened).is_err());
        assert_eq!(fs::read(cache.path().join("wrapper")).unwrap(), b"attacker");
        assert_eq!(
            fs::read(cache.path().join("invalid-away")).unwrap(),
            b"invalid"
        );
    }

    #[test]
    fn stalled_artifact_request_observes_safe_stop_without_waiting_for_transport() {
        let cache = tempfile::tempdir().unwrap();
        let cache_path = cache.path().to_path_buf();
        let spec = fixed_fixture_spec(b"verified fixture");
        let control = OperationControl::default();
        let worker_control = control.clone();
        let (entered_sender, entered_receiver) = std::sync::mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            ProductionArtifactSource::with_http(StalledHttp {
                entered: Mutex::new(Some(entered_sender)),
            })
            .acquire(&spec, &cache_path, &worker_control)
        });

        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("request entered");
        control.request_cancel();
        assert!(worker.join().unwrap().is_err());
        assert_eq!(
            control.cancellation_safety(),
            crate::operation::CancellationSafety::ProvenSafe
        );
        assert!(public_cache_entries(cache.path()).is_empty());
    }

    #[test]
    fn cancellation_during_acquisition_keeps_the_partial_but_never_publishes_it() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"verified fixture";
        let spec = fixed_fixture_spec(bytes);
        let control = OperationControl::default();
        let source = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(Some(ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 200,
                declared_length: Some(bytes.len() as u64),
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(CancelAfterFirstRead {
                    body: Cursor::new(bytes.to_vec()),
                    control: control.clone(),
                    requested: false,
                }),
            })),
        });

        assert!(source.acquire(&spec, cache.path(), &control).is_err());
        assert!(control.is_cancelled());
        assert_eq!(
            control.cancellation_safety(),
            crate::operation::CancellationSafety::ProvenSafe
        );
        assert!(!cache.path().join(cache_name(&spec)).exists());
        assert!(kept_partial(cache.path(), &spec).is_some());
    }

    #[test]
    fn a_stopped_pinned_download_resumes_with_range_and_is_verified_whole() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"verified fixture";
        let spec = fixed_fixture_spec(bytes);
        let split = 5;
        let control = OperationControl::default();
        let stopped = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(Some(ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 200,
                declared_length: Some(bytes.len() as u64),
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(CancelAfterFirstRead {
                    body: Cursor::new(bytes[..split].to_vec()),
                    control: control.clone(),
                    requested: false,
                }),
            })),
        });
        assert!(stopped.acquire(&spec, cache.path(), &control).is_err());
        assert_eq!(
            kept_partial(cache.path(), &spec),
            Some(bytes[..split].to_vec())
        );

        let ranges = Arc::new(Mutex::new(Vec::new()));
        let resumed = ProductionArtifactSource::with_http(SequenceHttp {
            responses: Mutex::new(VecDeque::from([ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 206,
                declared_length: Some((bytes.len() - split) as u64),
                content_range: Some(ContentRange {
                    start: split as u64,
                    end_inclusive: bytes.len() as u64 - 1,
                    complete_length: bytes.len() as u64,
                }),
                suggested_filename: None,
                retry_after: None,
                body: Box::new(Cursor::new(bytes[split..].to_vec())),
            }])),
            ranges: Arc::clone(&ranges),
        });
        let acquired = resumed
            .acquire(&spec, cache.path(), &OperationControl::default())
            .expect("resumed download");
        assert_eq!(*ranges.lock().unwrap(), vec![Some(split as u64)]);
        assert_eq!(fs::read(&acquired.path).unwrap(), bytes);
        assert_eq!(
            public_cache_entries(cache.path()).len(),
            1,
            "partial and binding are gone"
        );
    }

    #[test]
    fn a_resumed_partial_that_fails_verification_is_downloaded_again_once() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"verified fixture";
        let spec = fixed_fixture_spec(bytes);
        let split = 5;
        let control = OperationControl::default();
        let _ = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(Some(ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 200,
                declared_length: Some(bytes.len() as u64),
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(CancelAfterFirstRead {
                    body: Cursor::new(bytes[..split].to_vec()),
                    control: control.clone(),
                    requested: false,
                }),
            })),
        })
        .acquire(&spec, cache.path(), &control);
        // The kept bytes were damaged on disk.
        fs::write(
            cache.path().join(artifact_partial_name(spec.kind)),
            b"XXXXX",
        )
        .unwrap();

        let ranges = Arc::new(Mutex::new(Vec::new()));
        let source = ProductionArtifactSource::with_http(SequenceHttp {
            responses: Mutex::new(VecDeque::from([
                ArtifactHttpResponse {
                    final_url: spec.request_url().to_owned(),
                    status: 206,
                    declared_length: Some((bytes.len() - split) as u64),
                    content_range: Some(ContentRange {
                        start: split as u64,
                        end_inclusive: bytes.len() as u64 - 1,
                        complete_length: bytes.len() as u64,
                    }),
                    suggested_filename: None,
                    retry_after: None,
                    body: Box::new(Cursor::new(bytes[split..].to_vec())),
                },
                ArtifactHttpResponse {
                    final_url: spec.request_url().to_owned(),
                    status: 200,
                    declared_length: Some(bytes.len() as u64),
                    content_range: None,
                    suggested_filename: None,
                    retry_after: None,
                    body: Box::new(Cursor::new(bytes.to_vec())),
                },
            ])),
            ranges: Arc::clone(&ranges),
        });
        let acquired = source
            .acquire(&spec, cache.path(), &OperationControl::default())
            .expect("downloaded again after the resumed bytes failed");
        assert_eq!(*ranges.lock().unwrap(), vec![Some(split as u64), None]);
        assert_eq!(fs::read(&acquired.path).unwrap(), bytes);
    }

    #[test]
    fn a_partial_from_another_source_is_never_continued() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"verified fixture";
        let spec = fixed_fixture_spec(bytes);
        let control = OperationControl::default();
        let _ = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(Some(ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 200,
                declared_length: Some(bytes.len() as u64),
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(CancelAfterFirstRead {
                    body: Cursor::new(bytes[..5].to_vec()),
                    control: control.clone(),
                    requested: false,
                }),
            })),
        })
        .acquire(&spec, cache.path(), &control);
        assert!(kept_partial(cache.path(), &spec).is_some());

        let other = ArtifactSpec {
            request_url: Cow::Borrowed("https://mirror.invalid/gecko"),
            ..spec.clone()
        };
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let acquired = ProductionArtifactSource::with_http(SequenceHttp {
            responses: Mutex::new(VecDeque::from([ArtifactHttpResponse {
                final_url: other.request_url().to_owned(),
                status: 200,
                declared_length: Some(bytes.len() as u64),
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(Cursor::new(bytes.to_vec())),
            }])),
            ranges: Arc::clone(&ranges),
        })
        .acquire(&other, cache.path(), &OperationControl::default())
        .unwrap();
        assert_eq!(*ranges.lock().unwrap(), vec![None]);
        assert_eq!(fs::read(acquired.path).unwrap(), bytes);
    }

    /// A one-shot local HTTP server with `Range` support: the first request
    /// is answered with half the body, then the connection closes once the
    /// test says so; later requests are served from the requested offset.
    type RangeServer = (
        String,
        std::sync::mpsc::Sender<()>,
        Arc<Mutex<Vec<Option<String>>>>,
    );

    fn serve_with_range(body: &'static [u8]) -> RangeServer {
        use std::io::{BufRead, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/custom.bin",
            listener.local_addr().unwrap().port()
        );
        let (release, released) = std::sync::mpsc::channel::<()>();
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&ranges);
        thread::spawn(move || {
            for (index, stream) in listener.incoming().enumerate() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut range = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: ") {
                        range = Some(value.trim().to_owned());
                    }
                }
                seen.lock().unwrap().push(range.clone());
                let start = range
                    .as_deref()
                    .and_then(|value| value.strip_prefix("bytes="))
                    .and_then(|value| value.strip_suffix('-'))
                    .and_then(|value| value.parse::<usize>().ok());
                if index == 0 {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&body[..body.len() / 2]);
                    let _ = stream.flush();
                    let _ = released.recv();
                } else if let Some(start) = start {
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                        body.len() - start,
                        start,
                        body.len() - 1,
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&body[start..]);
                } else {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body);
                }
            }
        });
        (url, release, ranges)
    }

    #[test]
    fn a_custom_source_resumes_through_a_server_that_supports_range() {
        const BODY: &[u8] = b"custom source bytes that arrive in two halves";
        let (url, release, ranges) = serve_with_range(BODY);
        let spec = ArtifactSpec::custom(ArtifactKind::ClientBootstrap, url, 4096);
        let cache = tempfile::tempdir().unwrap();
        let control = OperationControl::default();
        let partial = cache.path().join(artifact_partial_name(spec.kind));
        let stopper = {
            let control = control.clone();
            let partial = partial.clone();
            thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                while fs::metadata(&partial).map_or(0, |metadata| metadata.len())
                    < (BODY.len() / 2) as u64
                {
                    assert!(Instant::now() < deadline, "first half never arrived");
                    thread::sleep(Duration::from_millis(5));
                }
                control.request_cancel();
                let _ = release.send(());
            })
        };
        let source = ProductionArtifactSource::with_http(SystemArtifactHttp);
        assert!(source.acquire(&spec, cache.path(), &control).is_err());
        stopper.join().unwrap();
        assert_eq!(
            kept_partial(cache.path(), &spec),
            Some(BODY[..BODY.len() / 2].to_vec())
        );

        let acquired = source
            .acquire(&spec, cache.path(), &OperationControl::default())
            .expect("resumed from the kept half");
        assert_eq!(fs::read(acquired.path).unwrap(), BODY);
        assert_eq!(
            *ranges.lock().unwrap(),
            vec![None, Some(format!("bytes={}-", BODY.len() / 2))]
        );
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

    fn fixture_download(spec: &ArtifactSpec, bytes: &'static [u8]) -> ProductionArtifactSource {
        ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(Some(ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 200,
                declared_length: Some(bytes.len() as u64),
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(Cursor::new(bytes.to_vec())),
            })),
        })
    }

    /// A download cache copied from another computer (here another folder:
    /// every file has a new identity) still downloads, instead of failing
    /// before the first byte as a "network" error.
    #[test]
    fn a_copied_download_cache_still_downloads() {
        let bytes = b"verified fixture";
        let spec = fixed_fixture_spec(bytes);
        let original = tempfile::tempdir().unwrap();
        fixture_download(&spec, bytes)
            .acquire(&spec, original.path(), &OperationControl::default())
            .expect("first download");
        let copy = tempfile::tempdir().unwrap();
        copy_tree(original.path(), copy.path());
        // Kept downloads are still reused from the copy...
        let reused = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(None),
        })
        .acquire(&spec, copy.path(), &OperationControl::default())
        .expect("copied download reused");
        assert_eq!(fs::read(reused.path).unwrap(), bytes);
        // ...and a missing one downloads into the copied cache.
        fs::remove_file(copy.path().join(cache_name(&spec))).unwrap();
        let fetched = fixture_download(&spec, bytes)
            .acquire(&spec, copy.path(), &OperationControl::default())
            .expect("download into a copied cache");
        assert_eq!(fs::read(fetched.path).unwrap(), bytes);
    }

    #[test]
    fn repeated_cancelled_artifacts_reuse_one_bounded_catalog_partial_slot() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"verified fixture";
        let spec = fixed_fixture_spec(bytes);
        for _ in 0..128 {
            let control = OperationControl::default();
            let source = ProductionArtifactSource::with_http(FakeHttp {
                response: Mutex::new(Some(ArtifactHttpResponse {
                    final_url: spec.request_url().to_owned(),
                    status: 200,
                    declared_length: Some(bytes.len() as u64),
                    content_range: None,
                    suggested_filename: None,
                    retry_after: None,
                    body: Box::new(CancelAfterFirstRead {
                        body: Cursor::new(bytes.to_vec()),
                        control: control.clone(),
                        requested: false,
                    }),
                })),
            });
            assert!(source.acquire(&spec, cache.path(), &control).is_err());
            let mut entries = fs::read_dir(cache.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name() != ".pd2launcher-retained")
                .map(|entry| entry.file_name())
                .collect::<Vec<_>>();
            entries.sort();
            // One partial slot and its source binding, whatever the count.
            assert_eq!(
                entries,
                [
                    std::ffi::OsString::from(artifact_partial_name(spec.kind)),
                    std::ffi::OsString::from(partial_binding_name(spec.kind)),
                ]
            );
        }
    }

    #[test]
    fn artifact_cache_unknowns_and_wrong_fixed_types_fail_closed() {
        for wrong_type in [false, true] {
            let cache = tempfile::tempdir().unwrap();
            let injected = if wrong_type {
                cache.path().join("artifact-00.partial")
            } else {
                cache.path().join("foreign.download")
            };
            if wrong_type {
                fs::create_dir(&injected).unwrap();
            } else {
                fs::write(&injected, b"foreign").unwrap();
            }
            let spec = fixed_fixture_spec(b"verified fixture");
            let source = ProductionArtifactSource::with_http(FakeHttp {
                response: Mutex::new(None),
            });
            assert!(
                source
                    .acquire(&spec, cache.path(), &OperationControl::default())
                    .is_err()
            );
            assert!(injected.exists());
            assert!(!cache.path().join(cache_name(&spec)).exists());
        }
    }

    #[test]
    fn cancellation_is_unproven_when_exact_partial_cleanup_is_substituted() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"verified fixture";
        let spec = fixed_fixture_spec(bytes);
        let control = OperationControl::default();
        let source = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(Some(ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 200,
                declared_length: Some(bytes.len() as u64),
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(SubstituteAndCancelPartialOnRead {
                    body: Cursor::new(bytes.to_vec()),
                    cache: cache.path().to_path_buf(),
                    control: control.clone(),
                    substituted: false,
                }),
            })),
        });

        assert!(source.acquire(&spec, cache.path(), &control).is_err());
        assert_eq!(
            control.cancellation_safety(),
            crate::operation::CancellationSafety::Unproven
        );
        assert_eq!(
            fs::read(cache.path().join("retained-response.partial")).unwrap(),
            bytes
        );
        let attacker = cache
            .path()
            .read_dir()
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| {
                let name = entry.file_name();
                name == OsStr::new(artifact_partial_name(spec.kind))
            })
            .expect("attacker replacement retained");
        assert_eq!(fs::read(attacker.path()).unwrap(), b"attacker");
    }

    #[test]
    fn volatile_wrapper_reuse_requires_matching_receipt_and_recomputed_digest() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = minimal_pe();
        let spec = wrapper_fixture_spec();
        let acquire = |body: Vec<u8>| {
            ProductionArtifactSource::with_http(FakeHttp {
                response: Mutex::new(Some(ArtifactHttpResponse {
                    final_url: spec.request_url().to_owned(),
                    status: 200,
                    declared_length: Some(body.len() as u64),
                    content_range: None,
                    suggested_filename: spec.expected_filename.map(str::to_owned),
                    retry_after: None,
                    body: Box::new(Cursor::new(body)),
                })),
            })
        };
        let acquired = acquire(bytes.clone())
            .acquire(&spec, cache.path(), &OperationControl::default())
            .unwrap();
        let receipt = cache.path().join(cache_receipt_name(&spec));
        assert!(receipt.is_file());

        let mut tampered = bytes.clone();
        tampered[100] ^= 0x7f;
        fs::write(&acquired.path, tampered).unwrap();
        let reacquired = acquire(bytes.clone())
            .acquire(&spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(fs::read(&reacquired.path).unwrap(), bytes);

        let mut provenance: CacheReceipt =
            serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
        provenance.final_url = "https://attacker.invalid/wrapper".to_owned();
        fs::write(&receipt, serde_json::to_vec(&provenance).unwrap()).unwrap();
        let reacquired = acquire(minimal_pe())
            .acquire(&spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(fs::read(&reacquired.path).unwrap(), minimal_pe());
    }

    #[test]
    fn volatile_wrapper_cannot_replace_receipt_identity_with_new_valid_pe_bytes() {
        let fixture = tempfile::tempdir().unwrap();
        let spec = wrapper_fixture_spec();
        let mut retained = minimal_pe();
        let retained_sha256 = hex(&Sha256::digest(&retained));
        retained.push(0x41);
        let path = fixture.path().join("d2-wrapper.verified");
        fs::write(&path, retained).unwrap();
        let acquired = AcquiredArtifact::new(
            path,
            spec.request_url(),
            spec.request_url(),
            spec.expected_filename.map(str::to_owned),
        )
        .with_retained_receipt_sha256(retained_sha256);

        assert!(matches!(
            verify_artifact(&spec, fixture.path(), acquired),
            Err(ArtifactRejection::DigestMismatch)
        ));
    }

    #[test]
    fn archive_extraction_rejects_links_and_parent_paths() {
        for archive in [archive_with_symlink(), archive_with_parent_path()] {
            let fixture = tempfile::tempdir().unwrap();
            let source = fixture.path().join("fixture.tar.xz");
            fs::write(&source, archive).unwrap();
            let destination = fixture.path().join("out");
            fs::create_dir(&destination).unwrap();
            let destination = Directory::open(&destination).unwrap();
            assert!(
                extract_archive_safely(
                    File::open(&source).unwrap(),
                    &destination,
                    "runtime",
                    &OperationControl::default(),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn archive_extraction_accepts_only_regular_entries_below_identity() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("fixture.tar.xz");
        fs::write(&source, safe_archive()).unwrap();
        let destination = fixture.path().join("out");
        fs::create_dir(&destination).unwrap();
        extract_archive_safely(
            File::open(&source).unwrap(),
            &Directory::open(&destination).unwrap(),
            "runtime",
            &OperationControl::default(),
        )
        .unwrap();
        assert_eq!(
            fs::read(destination.join("runtime/bin/wine")).unwrap(),
            b"wine"
        );
    }

    #[test]
    fn runtime_archive_accepts_only_flat_links_to_archive_regular_files() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("fixture.tar.xz");
        fs::write(&source, archive_with_safe_symlink()).unwrap();
        let destination = fixture.path().join("out");
        fs::create_dir(&destination).unwrap();
        extract_archive_safely(
            File::open(&source).unwrap(),
            &Directory::open(&destination).unwrap(),
            "runtime",
            &OperationControl::default(),
        )
        .unwrap();
        assert_eq!(
            fs::read_link(destination.join("runtime/lib/libwine.so")).unwrap(),
            PathBuf::from("libwine.so.1")
        );
        assert_eq!(
            fs::read(destination.join("runtime/lib/libwine.so")).unwrap(),
            b"library"
        );
    }

    #[test]
    fn runtime_archive_rejects_dangling_and_cyclic_flat_links() {
        for archive in [
            archive_with_dangling_symlink(),
            archive_with_symlink_cycle(),
        ] {
            let fixture = tempfile::tempdir().unwrap();
            let source = fixture.path().join("fixture.tar.xz");
            fs::write(&source, archive).unwrap();
            let destination = fixture.path().join("out");
            fs::create_dir(&destination).unwrap();
            assert!(
                extract_archive_safely(
                    File::open(&source).unwrap(),
                    &Directory::open(&destination).unwrap(),
                    "runtime",
                    &OperationControl::default(),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn runtime_archive_extraction_observes_original_cancellation_control() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("fixture.tar.xz");
        fs::write(&source, safe_archive()).unwrap();
        let destination = fixture.path().join("out");
        fs::create_dir(&destination).unwrap();
        let control = OperationControl::default();
        control.request_cancel();

        assert!(
            extract_archive_safely(
                File::open(&source).unwrap(),
                &Directory::open(&destination).unwrap(),
                "runtime",
                &control,
            )
            .is_err()
        );
        let runtime = Directory::open(&destination)
            .unwrap()
            .open_directory(Path::new("runtime"))
            .unwrap();
        assert!(runtime.names().unwrap().is_empty());
        assert!(runtime.open_file(Path::new(RUNTIME_RECEIPT)).is_err());
    }

    #[test]
    fn runtime_archive_extraction_uses_the_exact_retained_open_object() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("fixture.tar.xz");
        fs::write(&source, safe_archive()).unwrap();
        let retained = File::open(&source).unwrap();
        fs::rename(&source, fixture.path().join("verified-away.tar.xz")).unwrap();
        fs::write(
            &source,
            build_archive(|builder| {
                append_directory(builder, "runtime/");
                append_directory(builder, "runtime/bin/");
                append_file(builder, "runtime/bin/wine", b"attacker");
            }),
        )
        .unwrap();
        let destination = fixture.path().join("out");
        fs::create_dir(&destination).unwrap();

        extract_archive_safely(
            retained,
            &Directory::open(&destination).unwrap(),
            "runtime",
            &OperationControl::default(),
        )
        .unwrap();
        assert_eq!(
            fs::read(destination.join("runtime/bin/wine")).unwrap(),
            b"wine"
        );
    }

    #[test]
    fn incomplete_runtime_materialization_resumes_after_a_simulated_restart() {
        let fixture = tempfile::tempdir().unwrap();
        let runner = Directory::open(fixture.path()).unwrap();
        write_receipt_last_at(
            &runner,
            RUNTIME_MATERIALIZATION_STATE,
            managed_runtime().archive_sha512,
        )
        .unwrap();
        let runtime = runner.ensure_directory(Path::new("runtime")).unwrap();
        let archive = fixture.path().join("runtime.tar.xz");
        fs::write(&archive, safe_archive()).unwrap();

        assert!(
            extract_runtime_archive_safely_inner(
                File::open(&archive).unwrap(),
                &runtime,
                ArchiveRoot::Exact("runtime"),
                &OperationControl::default(),
                |_| Err(AdapterFailure::here()),
            )
            .is_err()
        );
        assert!(runtime.open_file(Path::new(RUNTIME_RECEIPT)).is_err());

        drop(runtime);
        drop(runner);
        let restarted_runner = Directory::open(fixture.path()).unwrap();
        validate_receipt_at(
            &restarted_runner,
            RUNTIME_MATERIALIZATION_STATE,
            managed_runtime().archive_sha512,
        )
        .unwrap();
        let restarted_runtime = restarted_runner
            .open_directory(Path::new("runtime"))
            .unwrap();
        extract_runtime_archive_safely(
            File::open(&archive).unwrap(),
            &restarted_runtime,
            ArchiveRoot::Exact("runtime"),
            &OperationControl::default(),
        )
        .unwrap();
        write_receipt_last_at(
            &restarted_runtime,
            RUNTIME_RECEIPT,
            managed_runtime().archive_sha512,
        )
        .unwrap();

        assert_eq!(
            fs::read(fixture.path().join("runtime/bin/wine")).unwrap(),
            b"wine"
        );
        validate_receipt_at(
            &restarted_runtime,
            RUNTIME_RECEIPT,
            managed_runtime().archive_sha512,
        )
        .unwrap();
    }

    #[test]
    fn runtime_extraction_stays_below_the_retained_destination_after_ancestor_swap() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("fixture.tar.xz");
        fs::write(&source, safe_archive()).unwrap();
        let destination = fixture.path().join("out");
        let retained = fixture.path().join("out-retained");
        let outside = fixture.path().join("outside");
        fs::create_dir(&destination).unwrap();
        fs::create_dir(&outside).unwrap();
        let destination_directory = Directory::open(&destination).unwrap();
        fs::rename(&destination, &retained).unwrap();
        std::os::unix::fs::symlink(&outside, &destination).unwrap();

        extract_archive_safely(
            File::open(source).unwrap(),
            &destination_directory,
            "runtime",
            &OperationControl::default(),
        )
        .unwrap();

        assert_eq!(
            fs::read(retained.join("runtime/bin/wine")).unwrap(),
            b"wine"
        );
        assert!(fs::read_dir(outside).unwrap().next().is_none());
    }

    #[test]
    fn receipt_is_atomic_and_never_written_before_validated_content() {
        let fixture = tempfile::tempdir().unwrap();
        assert!(write_receipt_last(&fixture.path().join("missing"), "receipt", "digest").is_err());
        assert!(!fixture.path().join("missing/receipt").exists());
        write_receipt_last(fixture.path(), "receipt", "digest").unwrap();
        assert_eq!(
            fs::read_to_string(fixture.path().join("receipt")).unwrap(),
            "digest"
        );
        assert_eq!(public_cache_entries(fixture.path()).len(), 1);
    }

    #[test]
    fn transport_policy_has_no_cleartext_builtin_source() {
        let catalog = ArtifactCatalog::pinned();
        for spec in &catalog.specs {
            assert!(spec.request_url().starts_with("https://"), "{spec:?}");
            assert!(approved_request_transport(spec));
            // The same identity over plain HTTP is never approved.
            let mut cleartext = spec.clone();
            cleartext.request_url = spec.request_url().replacen("https://", "http://", 1).into();
            assert!(!approved_request_transport(&cleartext), "{spec:?}");
        }
        // Only a user-supplied custom source may use plain HTTP.
        assert!(approved_request_transport(&ArtifactSpec::custom(
            ArtifactKind::ClientBootstrap,
            "http://mirror.example/pd2.exe".to_owned(),
            4096,
        )));

        assert!(!approved_redirect(
            catalog.spec(ArtifactKind::Gecko),
            catalog.spec(ArtifactKind::Gecko).request_url(),
            0,
        ));
        assert!(approved_redirect(
            catalog.spec(ArtifactKind::ManagedRuntime),
            "https://release-assets.githubusercontent.com/asset",
            1,
        ));
        assert!(!approved_redirect(
            catalog.spec(ArtifactKind::ManagedRuntime),
            "https://release-assets.githubusercontent.com/asset",
            2,
        ));
        // Launchpad's file link: one redirect, to the librarian's copy.
        let files = catalog
            .spec(ArtifactKind::FexPackage)
            .sources()
            .nth(1)
            .unwrap();
        let librarian = format!(
            "https://launchpadlibrarian.net/877920512/{}",
            files.request_url().rsplit_once('/').unwrap().1
        );
        assert!(approved_redirect(&files, &librarian, 1));
        assert!(!approved_redirect(&files, &librarian, 2));
        assert!(!approved_redirect(
            &files,
            "https://launchpadlibrarian.net/877920512/other.deb",
            1
        ));
        // The mirror: one redirect, to GitHub's asset host.
        let mirror = catalog
            .spec(ArtifactKind::FexPackage)
            .sources()
            .next()
            .unwrap();
        assert!(approved_redirect(
            &mirror,
            "https://release-assets.githubusercontent.com/asset",
            1
        ));
        // The pool: none.
        let pool = catalog
            .spec(ArtifactKind::FexPackage)
            .sources()
            .nth(2)
            .unwrap();
        assert!(!approved_redirect(&pool, &librarian, 1));
    }

    fn run_helper_invocation(host: Arc<LifetimeHost>) -> Result<(), AdapterFailure> {
        run_helper_invocation_with(host, &OperationControl::default())
    }

    /// A Blizzard downloader wrapper with its sealed helper: the wrapper
    /// hands off and exits while the helper still has work to do.
    fn run_helper_invocation_with(
        host: Arc<LifetimeHost>,
        control: &OperationControl,
    ) -> Result<(), AdapterFailure> {
        let (fixture, paths) = managed_paths_with_runtime();
        let wrapper = verified_test_artifact(
            ArtifactKind::ExpansionWrapper,
            fixture
                .path()
                .join("Downloader_Diablo2_Lord_of_Destruction_enUS.exe"),
        );
        let lease = fixture_lease(fixture.path());
        ProductionInstallDriver::new(Arc::new(host)).run_wine(WineInvocation {
            paths: &paths,
            runtime: InstalledRuntime::BuiltIn(managed_runtime()),
            working_directory: paths.root(),
            program: WineProgram::Verified(&wrapper),
            verified_argument: None,
            arguments: Vec::new(),
            profile: WineProfile::Install,
            timeout: INSTALL_TIMEOUT,
            mutation_lease: &lease,
            control,
            ui_helper: Some(UiHelper::Downloader),
            stall: None,
            accepted_exit_codes: &[],
        })
    }

    #[test]
    fn a_stop_while_the_child_and_its_helper_run_is_proven_safe() {
        let host = LifetimeHost::new(HelperScript::NeverExits, false);
        let control = OperationControl::default();
        control.request_cancel();
        assert!(run_helper_invocation_with(Arc::clone(&host), &control).is_err());
        let events = host.events();
        assert!(events.contains(&"terminate child".to_owned()), "{events:?}");
        assert!(
            events.contains(&"terminate helper".to_owned()),
            "{events:?}"
        );
        assert!(
            events.contains(&"spawn wineserver -k".to_owned()),
            "{events:?}"
        );
        assert_eq!(
            control.cancellation_safety(),
            crate::operation::CancellationSafety::ProvenSafe
        );
    }

    #[test]
    fn a_stop_during_the_post_child_wait_with_a_live_helper_is_proven_safe() {
        let host = LifetimeHost::new(HelperScript::NeverExits, false);
        let control = OperationControl::default();
        *host.cancel_on_wait.lock().unwrap() = Some(control.clone());
        assert!(run_helper_invocation_with(Arc::clone(&host), &control).is_err());
        let events = host.events();
        let child_exit = events
            .iter()
            .position(|event| event == "exit child ok")
            .unwrap();
        let helper_stop = events
            .iter()
            .position(|event| event == "terminate helper")
            .unwrap();
        assert!(child_exit < helper_stop, "{events:?}");
        assert!(
            events.contains(&"spawn wineserver -k".to_owned()),
            "{events:?}"
        );
        assert_eq!(
            control.cancellation_safety(),
            crate::operation::CancellationSafety::ProvenSafe
        );
    }

    #[test]
    fn ui_helper_outlives_its_child_until_wineserver_proves_the_prefix_quiet() {
        let host = LifetimeHost::new(HelperScript::ExitsOk, false);
        run_helper_invocation(Arc::clone(&host)).unwrap();
        // The wrapper exits at once; the helper is not stopped with it but
        // keeps running through `wineserver -w` until it has answered its
        // dialog, and the prefix goes quiet only after that.
        assert_eq!(
            host.events(),
            [
                "spawn helper",
                "spawn child",
                "exit child ok",
                "spawn wineserver -w",
                "exit helper ok",
                "exit wineserver -w ok",
            ]
        );
    }

    #[test]
    fn a_failed_ui_helper_fails_the_stage_and_quiesces_the_prefix() {
        let host = LifetimeHost::new(HelperScript::ExitsFailed, false);
        assert!(run_helper_invocation(Arc::clone(&host)).is_err());
        let events = host.events();
        let failed = events
            .iter()
            .position(|e| e == "exit helper failed")
            .unwrap();
        // The waiting `wineserver -w` is stopped, then the prefix is killed
        // and waited for.
        assert_eq!(
            events[..failed],
            [
                "spawn helper",
                "spawn child",
                "exit child ok",
                "spawn wineserver -w"
            ]
        );
        assert_eq!(
            events[failed + 1..],
            [
                "terminate wineserver -w",
                "spawn wineserver -k",
                "exit wineserver -k ok",
                "spawn wineserver -w",
                "exit wineserver -w ok",
            ]
        );
    }

    #[test]
    fn the_blizzard_download_folder_is_reported_as_byte_progress() {
        let fixture = tempfile::tempdir().unwrap();
        let folder = fixture.path().join("downloads");
        fs::create_dir_all(folder.join("D2-1.14b-Installer-enUS/DirectX")).unwrap();
        fs::write(
            folder.join("D2-1.14b-Installer-enUS/Installer.exe"),
            [0; 300],
        )
        .unwrap();
        fs::write(
            folder.join("D2-1.14b-Installer-enUS/DirectX/dsetup.dll"),
            [0; 200],
        )
        .unwrap();
        // A link is never followed.
        std::os::unix::fs::symlink("/etc", folder.join("link")).unwrap();
        assert_eq!(folder_size(&folder, 4), 500);

        let (publisher, receiver) = crate::operation::progress_channel();
        let control = OperationControl::with_progress(publisher);
        let stall = DownloadStall::default();
        let watcher = DownloadFolderWatcher::start(
            folder,
            1_000,
            WorkPhase::BaseGameInstall,
            control,
            GrowthClock::new(
                DOWNLOAD_STALL_LIMIT,
                DOWNLOAD_RESUME_LIMIT,
                DOWNLOAD_EMPTY_LIMIT,
                Instant::now(),
            ),
            stall.clone(),
            None,
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let progress = loop {
            if let Some(progress) = receiver.take_latest() {
                break progress;
            }
            assert!(Instant::now() < deadline, "no progress reported");
            thread::sleep(Duration::from_millis(20));
        };
        drop(watcher);
        assert!(!stall.is_stalled());
        assert_eq!(progress.phase(), WorkPhase::BaseGameInstall);
        let bytes = progress.file_bytes().unwrap();
        assert_eq!((bytes.completed(), bytes.total()), (500, Some(1_000)));
    }

    #[test]
    fn the_vendor_helper_exit_status_says_which_error_dialog_ended_the_stage() {
        for (script, expected) in [
            (
                HelperScript::ExitsWith(22),
                Some(VENDOR_HELPER_KEY_REJECTED),
            ),
            (
                HelperScript::ExitsWith(20),
                Some(VENDOR_HELPER_ERROR_DIALOG),
            ),
            (
                HelperScript::ExitsWith(23),
                Some(VENDOR_HELPER_INSTALLER_MENU),
            ),
            (HelperScript::ExitsFailed, Some(1)),
            (HelperScript::ExitsOk, Some(0)),
        ] {
            let host = LifetimeHost::new(script, false);
            let (fixture, paths) = managed_paths_with_runtime();
            let installer = verified_test_artifact(
                ArtifactKind::BaseGameWrapper,
                fixture.path().join("Installer.exe"),
            );
            let lease = fixture_lease(fixture.path());
            let helper_exit = HelperExit::default();
            let result = ProductionInstallDriver::new(Arc::new(host)).run_wine_observing_helper(
                WineInvocation {
                    paths: &paths,
                    runtime: InstalledRuntime::BuiltIn(managed_runtime()),
                    working_directory: paths.root(),
                    program: WineProgram::Verified(&installer),
                    verified_argument: None,
                    arguments: Vec::new(),
                    profile: WineProfile::Install,
                    timeout: INSTALL_TIMEOUT,
                    mutation_lease: &lease,
                    control: &OperationControl::default(),
                    ui_helper: Some(UiHelper::Vendor),
                    stall: None,
                    accepted_exit_codes: &[],
                },
                &helper_exit,
            );
            assert_eq!(result.is_ok(), expected == Some(0));
            assert_eq!(helper_exit.code(), expected);
        }
        // A helper stopped by the Launcher reports no status of its own.
        let host = LifetimeHost::new(HelperScript::NeverExits, false);
        let control = OperationControl::default();
        control.request_cancel();
        let (fixture, paths) = managed_paths_with_runtime();
        let installer = verified_test_artifact(
            ArtifactKind::BaseGameWrapper,
            fixture.path().join("Installer.exe"),
        );
        let lease = fixture_lease(fixture.path());
        let helper_exit = HelperExit::default();
        assert!(
            ProductionInstallDriver::new(Arc::new(host))
                .run_wine_observing_helper(
                    WineInvocation {
                        paths: &paths,
                        runtime: InstalledRuntime::BuiltIn(managed_runtime()),
                        working_directory: paths.root(),
                        program: WineProgram::Verified(&installer),
                        verified_argument: None,
                        arguments: Vec::new(),
                        profile: WineProfile::Install,
                        timeout: INSTALL_TIMEOUT,
                        mutation_lease: &lease,
                        control: &control,
                        ui_helper: Some(UiHelper::Vendor),
                        stall: None,
                        accepted_exit_codes: &[],
                    },
                    &helper_exit,
                )
                .is_err()
        );
        assert_eq!(helper_exit.code(), None);
    }

    #[test]
    fn a_quiet_prefix_with_a_live_helper_is_contradictory_evidence() {
        let host = LifetimeHost::new(HelperScript::NeverExits, true);
        assert!(run_helper_invocation(Arc::clone(&host)).is_err());
        let events = host.events();
        assert!(events.contains(&"exit wineserver -w ok".to_owned()));
        assert_eq!(events.last().map(String::as_str), Some("terminate helper"));
    }

    /// The official launcher closes with a failure status; that is the
    /// user's choice, not evidence.
    struct FailedChildHost {
        requests: Mutex<Vec<RequestSnapshot>>,
    }

    impl HostExecution for FailedChildHost {
        fn spawn(&self, request: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
            // `wineserver` and the setting's `reg` run succeed; every other
            // child exits with a failure.
            let succeeds = request.executable() == Path::new("/proc/self/fd/232")
                || request.arguments().iter().any(|argument| argument == "reg");
            self.requests.lock().unwrap().push(RequestSnapshot {
                executable: request.executable().to_owned(),
                guest: request.is_guest(),
                arguments: request.arguments().to_vec(),
                working_directory: request.working_directory().map(Path::to_owned),
                clears_environment: request.clears_environment(),
                environment: request.environment().to_vec(),
                output_limit: request.output_limit(),
                inherited_descriptors: request.inherited_file_descriptors().collect(),
                rendered_debug: format!("{request:?}"),
            });
            if succeeds {
                Ok(Box::new(CompleteProcess))
            } else {
                Ok(Box::new(FailedProcess))
            }
        }
    }

    #[test]
    fn the_official_launcher_runs_from_project_d2_with_the_game_profile_until_quiet() {
        let (fixture, paths) = managed_paths_with_runtime();
        let project = paths.game().join("ProjectD2");
        fs::create_dir_all(&project).unwrap();
        let lease = fixture_lease(fixture.path());
        let driver = |host: Arc<dyn HostExecution>| ProductionInstallDriver::new(host);

        // Missing, or not a Windows executable: nothing starts.
        let host = Arc::new(RecordingHost::default());
        assert!(
            driver(host.clone())
                .run_official_launcher(fixture.path(), &lease, &OperationControl::default())
                .is_err()
        );
        fs::write(project.join("PD2Launcher.exe"), b"#!/bin/sh").unwrap();
        assert!(
            driver(host.clone())
                .run_official_launcher(fixture.path(), &lease, &OperationControl::default())
                .is_err()
        );
        assert!(host.requests.lock().unwrap().is_empty());

        fs::write(project.join("PD2Launcher.exe"), minimal_pe()).unwrap();
        let host = Arc::new(FailedChildHost {
            requests: Mutex::new(Vec::new()),
        });
        driver(host.clone())
            .run_official_launcher(fixture.path(), &lease, &OperationControl::default())
            .unwrap();
        let requests = host.requests.lock().unwrap();
        // The software rendering setting (on by default) is synced first:
        // `reg add`, then its own `wineserver -w`.
        assert_eq!(requests.len(), 4);
        assert!(
            requests[0]
                .arguments
                .ends_with(&wpf_software_rendering_arguments(true))
        );
        assert_wineserver_wait_profile(&requests[1], &paths);
        let launcher = &requests[2];
        assert_guest_preloader(launcher);
        assert_eq!(
            launcher.arguments,
            [
                runtime_loader_arguments(),
                vec![OsString::from("PD2Launcher.exe")],
            ]
            .concat()
        );
        assert_eq!(
            launcher.working_directory.as_deref(),
            Some(project.as_path())
        );
        // It inherits the exclusive installation lease, like every mutator.
        assert_eq!(launcher.inherited_descriptors, [235, 230, 231, 236, 234]);
        assert_eq!(
            environment_value(launcher, "WINEDLLOVERRIDES"),
            Some(OsStr::new(crate::process::GAME_DLL_OVERRIDES))
        );
        assert_eq!(environment_value(launcher, "WINEARCH"), None);
        // The FEX workarounds apply only to the translated (ARM64) row.
        let translated = TRANSLATED_DOTNET.then_some(OsStr::new("0"));
        assert_eq!(
            environment_value(launcher, "DOTNET_EnableWriteXorExecute"),
            translated
        );
        assert_eq!(
            environment_value(launcher, "DOTNET_TieredCompilation"),
            translated
        );
        // FEX's strict settings, as for the installers and the game (ARM64).
        for (name, value) in FEX_STRICT_ENVIRONMENT {
            assert_eq!(
                environment_value(launcher, name),
                cfg!(target_arch = "aarch64").then_some(OsStr::new(value)),
                "{name}"
            );
        }
        assert_launcher_cache_home(launcher, &paths);
        // Its exit is followed by waiting for everything it started.
        assert_wineserver_wait_profile(&requests[3], &paths);
    }

    #[test]
    fn on_android_a_folder_used_in_place_is_installed_on_the_g_drive() {
        let (fixture, paths) = managed_paths_with_runtime();
        fs::create_dir_all(fixture.path().join("prefix/dosdevices")).unwrap();
        let copy = Path::new("/data/user/0/com.pd2launcher.app/files/import/a1/Diablo II");

        assert_eq!(
            in_place_install_path(&paths, copy, true).unwrap(),
            r"G:\Diablo II"
        );
        // The drive's root is the copy's parent, on the data partition.
        assert_eq!(
            fs::read_link(fixture.path().join("prefix/dosdevices/g:")).unwrap(),
            copy.parent().unwrap()
        );

        // The desktop keeps the host root's drive and links nothing.
        let (desktop, paths) = managed_paths_with_runtime();
        fs::create_dir_all(desktop.path().join("prefix/dosdevices")).unwrap();
        assert_eq!(
            in_place_install_path(&paths, Path::new("/home/deck/Games/Diablo II"), false).unwrap(),
            r"Z:\home\deck\Games\Diablo II"
        );
        assert!(fs::symlink_metadata(desktop.path().join("prefix/dosdevices/g:")).is_err());
    }

    #[test]
    fn the_official_launcher_runs_from_a_game_folder_used_in_place() {
        let (fixture, _) = managed_paths_with_runtime();
        let game = tempfile::tempdir().unwrap();
        let project = game.path().join("ProjectD2");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("PD2Launcher.exe"), minimal_pe()).unwrap();
        fs::create_dir_all(fixture.path().join("prefix")).unwrap();
        crate::game_folder::write_pointer(
            &Directory::open(&fixture.path().join("prefix")).unwrap(),
            &crate::game_folder::ExternalGameFolder::at(game.path()),
        )
        .unwrap();
        let lease = fixture_lease(fixture.path());
        let host = Arc::new(FailedChildHost {
            requests: Mutex::new(Vec::new()),
        });
        ProductionInstallDriver::new(host.clone())
            .run_official_launcher(fixture.path(), &lease, &OperationControl::default())
            .unwrap();
        // Outside the managed root, its ProjectD2 is still where it starts.
        let requests = host.requests.lock().unwrap();
        assert_eq!(
            requests[2].working_directory.as_deref(),
            Some(project.as_path())
        );
    }

    #[test]
    fn another_launcher_program_runs_from_its_own_folder_with_its_start_options() {
        let (fixture, paths) = managed_paths_with_runtime();
        let other = paths.game().join("ZYLPD2");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("Launcher.exe"), minimal_pe()).unwrap();
        let lease = fixture_lease(fixture.path());
        let host = Arc::new(FailedChildHost {
            requests: Mutex::new(Vec::new()),
        });
        ProductionInstallDriver::new(host.clone())
            .with_official_launcher("ZYLPD2/Launcher.exe", " -lang  zh ")
            .run_official_launcher(fixture.path(), &lease, &OperationControl::default())
            .unwrap();
        let requests = host.requests.lock().unwrap();
        let launcher = &requests[2];
        assert_eq!(
            launcher.arguments,
            [
                runtime_loader_arguments(),
                ["Launcher.exe", "-lang", "zh"].map(OsString::from).to_vec(),
            ]
            .concat()
        );
        assert_eq!(launcher.working_directory.as_deref(), Some(other.as_path()));
    }

    #[test]
    fn turning_software_rendering_off_removes_the_value_before_the_official_launcher_opens() {
        let (fixture, paths) = managed_paths_with_runtime();
        let project = paths.game().join("ProjectD2");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("PD2Launcher.exe"), minimal_pe()).unwrap();
        let lease = fixture_lease(fixture.path());
        let host = Arc::new(FailedChildHost {
            requests: Mutex::new(Vec::new()),
        });
        ProductionInstallDriver::new(host.clone())
            .with_wpf_software_rendering(false)
            .run_official_launcher(fixture.path(), &lease, &OperationControl::default())
            .unwrap();
        let requests = host.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(
            requests[0]
                .arguments
                .ends_with(&wpf_software_rendering_arguments(false))
        );
        assert!(
            requests[2]
                .arguments
                .ends_with(&[OsString::from("PD2Launcher.exe")])
        );
    }

    #[test]
    fn the_official_launcher_opens_without_reg_when_user_reg_already_matches_the_setting() {
        let section = "[Software\\\\Microsoft\\\\Avalon.Graphics] 1792627165\n#time=1dd61b8\n";
        let with_value = format!(
            "WINE REGISTRY Version 2\n\n{section}\"DisableHWAcceleration\"=dword:00000001\n"
        );
        let without_value = format!("WINE REGISTRY Version 2\n\n{section}");
        for (enabled, user_reg, reg_runs) in [
            (true, &with_value, false),
            (true, &without_value, true),
            (false, &without_value, false),
            (false, &with_value, true),
        ] {
            let (fixture, paths) = managed_paths_with_runtime();
            let project = paths.game().join("ProjectD2");
            fs::create_dir_all(&project).unwrap();
            fs::write(project.join("PD2Launcher.exe"), minimal_pe()).unwrap();
            fs::create_dir_all(fixture.path().join("prefix")).unwrap();
            fs::write(fixture.path().join("prefix/user.reg"), user_reg).unwrap();
            let lease = fixture_lease(fixture.path());
            let host = Arc::new(FailedChildHost {
                requests: Mutex::new(Vec::new()),
            });
            ProductionInstallDriver::new(host.clone())
                .with_wpf_software_rendering(enabled)
                .run_official_launcher(fixture.path(), &lease, &OperationControl::default())
                .unwrap();
            let requests = host.requests.lock().unwrap();
            let case = format!("software rendering {enabled}, user.reg {user_reg:?}");
            // `reg` and its own `wineserver -w` run only when the value
            // must change; then the launcher and its `wineserver -w`.
            let reg = requests
                .iter()
                .filter(|request| {
                    request
                        .arguments
                        .ends_with(&wpf_software_rendering_arguments(enabled))
                })
                .count();
            assert_eq!(reg, usize::from(reg_runs), "{case}");
            assert_eq!(requests.len(), if reg_runs { 4 } else { 2 }, "{case}");
            assert!(
                requests[requests.len() - 2]
                    .arguments
                    .ends_with(&[OsString::from("PD2Launcher.exe")]),
                "{case}"
            );
            assert_wineserver_wait_profile(&requests[requests.len() - 1], &paths);
        }
    }

    #[test]
    fn wpf_software_rendering_is_seen_only_in_the_avalon_graphics_section() {
        let section = "[Software\\\\Microsoft\\\\Avalon.Graphics] 1792627165\n#time=1dd61b8\n";
        let matches = |user_reg: &str| {
            (
                wpf_software_rendering_matches(user_reg, true),
                wpf_software_rendering_matches(user_reg, false),
            )
        };
        // Set: matches the setting on; no value: matches it off.
        assert_eq!(
            matches(&format!(
                "{section}\"DisableHWAcceleration\"=dword:00000001\n"
            )),
            (true, false)
        );
        assert_eq!(matches(section), (false, true));
        assert_eq!(matches(""), (false, true));
        // Names in any case; a line end of Windows' kind.
        assert_eq!(
            matches(
                "[software\\\\microsoft\\\\avalon.graphics] 1\r\n\"disablehwacceleration\"=dword:00000001\r\n"
            ),
            (true, false)
        );
        // Spaces at the start of a line do not count.
        assert_eq!(
            matches(&format!(
                "{section}  \"DisableHWAcceleration\"=dword:00000001\n"
            )),
            (true, false)
        );
        // Another value of that name matches neither: `reg` sets it.
        assert_eq!(
            matches(&format!(
                "{section}\"DisableHWAcceleration\"=dword:00000000\n"
            )),
            (false, false)
        );
        assert_eq!(
            matches(&format!(
                "{section}\"DisableHWAcceleration\"=dword:00000001\n\"disablehwacceleration\"=\"1\"\n"
            )),
            (false, false)
        );
        // Another value name that starts the same is not it.
        assert_eq!(
            matches(&format!(
                "{section}\"DisableHWAccelerationX\"=dword:00000001\n"
            )),
            (false, true)
        );
        // The same value under another key, or after the section, is not it.
        assert_eq!(
            matches(
                "[Software\\\\Microsoft\\\\Avalon.Graphics\\\\Other] 1\n\"DisableHWAcceleration\"=dword:00000001\n"
            ),
            (false, true)
        );
        assert_eq!(
            matches(&format!(
                "{section}\n[Software\\\\Wine] 1\n\"DisableHWAcceleration\"=dword:00000001\n"
            )),
            (false, true)
        );
    }

    #[test]
    fn tiered_compilation_is_off_under_fex_and_has_no_call_counting_on_android() {
        assert_eq!(
            dotnet_tiered_compilation(false),
            [("DOTNET_TieredCompilation", "0")]
        );
        assert_eq!(
            dotnet_tiered_compilation(true),
            [
                ("DOTNET_TieredCompilation", "1"),
                ("DOTNET_TC_CallCounting", "0")
            ]
        );
        // Each kind of host, whatever the host the test runs on.
        for (translated, android) in [(true, true), (true, false), (false, true), (false, false)] {
            let (fixture, _) = managed_paths_with_runtime();
            app_runtime_fixture(fixture.path());
            initialized_prefix_fixture(fixture.path());
            // The game folder of the host's layout (on Android outside the prefix).
            let project = ManagedPaths::prepare_on(fixture.path(), android)
                .unwrap()
                .game()
                .join("ProjectD2");
            fs::create_dir_all(&project).unwrap();
            fs::write(project.join(OFFICIAL_LAUNCHER), minimal_pe()).unwrap();
            let lease = fixture_lease(fixture.path());
            let host = Arc::new(RecordingHost::default());
            ProductionInstallDriver::new(host.clone())
                .on_android_host(android)
                .with_translated_dotnet(translated)
                .run_official_launcher(fixture.path(), &lease, &OperationControl::default())
                .unwrap();
            let requests = host.requests.lock().unwrap();
            let launcher = requests
                .iter()
                .find(|request| {
                    request
                        .arguments
                        .ends_with(&[OsString::from(OFFICIAL_LAUNCHER)])
                })
                .expect("the official launcher ran");
            // Only where its .NET runs translated (ARM64).
            let case = format!("translated {translated}, android {android}");
            let expected = match (translated, android) {
                (false, _) => (None, None),
                (true, true) => (Some("1"), Some("0")),
                (true, false) => (Some("0"), None),
            };
            let value = |name| environment_value(launcher, name).and_then(OsStr::to_str);
            assert_eq!(
                (
                    value("DOTNET_TieredCompilation"),
                    value("DOTNET_TC_CallCounting")
                ),
                expected,
                "{case}"
            );
            assert_eq!(
                value("DOTNET_EnableWriteXorExecute"),
                translated.then_some("0"),
                "{case}"
            );
            // No other Wine run gets them.
            for request in requests
                .iter()
                .filter(|request| !std::ptr::eq(*request, launcher))
            {
                assert_eq!(
                    environment_value(request, "DOTNET_TC_CallCounting"),
                    None,
                    "{case}: {:?}",
                    request.arguments
                );
            }
        }
    }

    #[test]
    fn wpf_software_rendering_is_added_or_deleted_for_the_prefix_user() {
        assert_eq!(
            wpf_software_rendering_arguments(true),
            [
                "add",
                r"HKCU\Software\Microsoft\Avalon.Graphics",
                "/v",
                "DisableHWAcceleration",
                "/t",
                "REG_DWORD",
                "/d",
                "1",
                "/f"
            ]
            .map(OsString::from)
        );
        assert_eq!(
            wpf_software_rendering_arguments(false),
            [
                "delete",
                r"HKCU\Software\Microsoft\Avalon.Graphics",
                "/v",
                "DisableHWAcceleration",
                "/f"
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn wow64_prefix_selects_windows_7_through_builtin_reg() {
        assert_eq!(
            crate::runtime::WINE_WOW64_11_11.windows_version,
            Some("win7")
        );
        assert_eq!(crate::runtime::WINE_GE_8_26.windows_version, None);
        assert_eq!(
            windows_version_arguments("win7"),
            [
                "add",
                r"HKCU\Software\Wine",
                "/v",
                "Version",
                "/d",
                "win7",
                "/f"
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn gecko_uses_builtin_msiexec_with_a_separate_verified_msi_descriptor() {
        let (fixture, paths) = managed_paths_with_runtime();
        let gecko = verified_test_artifact(
            ArtifactKind::Gecko,
            fixture.path().join("wine-gecko-x86.msi"),
        );
        let host = Arc::new(RecordingHost::default());
        let lease = fixture_lease(fixture.path());

        ProductionInstallDriver::new(host.clone())
            .run_wine(WineInvocation {
                paths: &paths,
                runtime: InstalledRuntime::BuiltIn(managed_runtime()),
                working_directory: paths.root(),
                program: WineProgram::Builtin(OsString::from("msiexec")),
                verified_argument: Some(&gecko),
                arguments: vec![
                    OsString::from("/i"),
                    OsString::from("/proc/self/fd/233"),
                    OsString::from("/qn"),
                ],
                profile: WineProfile::Install,
                timeout: RUNTIME_TIMEOUT,
                mutation_lease: &lease,
                control: &OperationControl::default(),
                ui_helper: None,
                stall: None,
                accepted_exit_codes: &[],
            })
            .unwrap();

        let requests = host.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_guest_preloader(&requests[0]);
        assert_eq!(
            requests[0].arguments,
            [
                runtime_loader_arguments(),
                vec![
                    OsString::from("msiexec"),
                    OsString::from("/i"),
                    OsString::from("/proc/self/fd/233"),
                    OsString::from("/qn"),
                ],
            ]
            .concat()
        );
        assert_eq!(
            requests[0].inherited_descriptors,
            [235, 230, 231, 236, 234, 233]
        );
        assert_eq!(requests[0].working_directory.as_deref(), Some(paths.root()));
        assert_wineserver_wait_profile(&requests[1], &paths);
    }

    #[test]
    fn verified_program_descriptor_survives_path_substitution_and_rejects_links() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("wrapper.exe");
        let artifact = verified_test_artifact(ArtifactKind::BaseGameWrapper, path.clone());
        let mut verified = open_verified_program(&artifact).unwrap();
        fs::rename(&path, fixture.path().join("verified-away.exe")).unwrap();
        fs::write(&path, b"attacker replacement").unwrap();
        let mut executed = Vec::new();
        verified.read_to_end(&mut executed).unwrap();
        assert_eq!(executed, minimal_pe());

        let linked_path = fixture.path().join("linked.exe");
        fs::hard_link(fixture.path().join("verified-away.exe"), &linked_path).unwrap();
        let linked = VerifiedArtifact {
            path: linked_path,
            ..artifact
        };
        assert!(open_verified_program(&linked).is_err());

        let symlink_path = fixture.path().join("symlink.exe");
        symlink(fixture.path().join("verified-away.exe"), &symlink_path).unwrap();
        let symlinked = VerifiedArtifact {
            path: symlink_path,
            ..linked
        };
        assert!(open_verified_program(&symlinked).is_err());
    }

    #[test]
    fn a_managed_program_is_executed_only_while_its_name_is_the_verified_object() {
        let fixture = tempfile::tempdir().unwrap();
        let cache = fixture.path().join("cache/artifacts");
        fs::create_dir_all(&cache).unwrap();
        let path = cache.join("lod-wrapper.verified");
        let artifact = verified_test_artifact(ArtifactKind::ExpansionWrapper, path.clone());
        let root = Directory::open(fixture.path()).unwrap();
        let relative = Path::new("cache/artifacts/lod-wrapper.verified");
        let verified = open_verified_program_beneath(&root, relative, &artifact).unwrap();
        assert!(require_named_program(&root, relative, &verified).is_ok());

        // Swapped for other bytes after verification: refused.
        fs::rename(&path, cache.join("verified-away")).unwrap();
        fs::write(&path, minimal_pe()).unwrap();
        assert!(require_named_program(&root, relative, &verified).is_err());
        // A second link to the verified object: refused.
        fs::remove_file(&path).unwrap();
        fs::hard_link(cache.join("verified-away"), &path).unwrap();
        assert!(require_named_program(&root, relative, &verified).is_err());
        // A symbolic link anywhere on the way: refused, at open and at check.
        fs::remove_file(&path).unwrap();
        fs::rename(cache.join("verified-away"), &path).unwrap();
        assert!(require_named_program(&root, relative, &verified).is_ok());
        fs::rename(
            fixture.path().join("cache"),
            fixture.path().join("real-cache"),
        )
        .unwrap();
        symlink(
            fixture.path().join("real-cache"),
            fixture.path().join("cache"),
        )
        .unwrap();
        assert!(require_named_program(&root, relative, &verified).is_err());
        assert!(open_verified_program_beneath(&root, relative, &artifact).is_err());
    }

    /// A helper that exits a moment after the prefix went quiet.
    struct LateExit {
        polls: usize,
    }

    impl SupervisedProcess for LateExit {
        fn try_wait(&mut self) -> io::Result<Option<ProcessExit>> {
            if self.polls == 0 {
                Ok(Some(ProcessExit::new(true, Some(0))))
            } else {
                self.polls -= 1;
                Ok(None)
            }
        }

        fn terminate(&mut self) -> io::Result<()> {
            self.polls = 0;
            Ok(())
        }
    }

    #[test]
    fn a_helper_reaped_shortly_after_the_prefix_went_quiet_is_accepted() {
        let mut late: Option<Box<dyn SupervisedProcess>> = Some(Box::new(LateExit { polls: 5 }));
        assert!(finish_helper_within(&mut late, Duration::from_secs(2)).is_ok());
        assert!(late.is_none());
        let mut stuck: Option<Box<dyn SupervisedProcess>> =
            Some(Box::new(LateExit { polls: usize::MAX }));
        assert!(finish_helper_within(&mut stuck, Duration::from_millis(50)).is_err());
        assert!(
            stuck.is_none(),
            "a helper still running after the grace is stopped"
        );
    }

    #[test]
    fn cancelled_vendor_child_is_reaped_and_prefix_is_quiesced() {
        let (fixture, paths) = managed_paths_with_runtime();
        fs::create_dir_all(paths.game()).unwrap();
        // A child without a UI helper, so the spawn order is child, then
        // the wineserver processes.
        let bootstrap = verified_test_artifact(
            ArtifactKind::ClientBootstrap,
            fixture.path().join("cancelled-bootstrap.exe"),
        );
        let state = Arc::new(CancelProcessState::default());
        let host = Arc::new(CancelHost {
            state: Arc::clone(&state),
            spawns: AtomicUsize::new(0),
        });
        let control = OperationControl::default();
        control.request_cancel();
        let lease = fixture_lease(fixture.path());

        assert!(
            ProductionInstallDriver::new(host)
                .install_client_bootstrap(&paths, &bootstrap, &lease, &control)
                .is_err()
        );
        assert!(state.terminated.load(Ordering::Acquire));
        assert!(state.waited.load(Ordering::Acquire));
        assert_eq!(state.quiesce_spawns.load(Ordering::Acquire), 2);
        assert_eq!(
            control.cancellation_safety(),
            crate::operation::CancellationSafety::ProvenSafe
        );
    }

    #[test]
    fn cancellation_during_normal_wineserver_wait_is_reaped_then_quiesced() {
        let (fixture, paths) = managed_paths_with_runtime();
        fs::create_dir_all(paths.game()).unwrap();
        // A child without a UI helper, so the spawn order is child, then
        // the wineserver processes.
        let bootstrap = verified_test_artifact(
            ArtifactKind::ClientBootstrap,
            fixture.path().join("post-child-cancel.exe"),
        );
        let state = Arc::new(CancelProcessState::default());
        let control = OperationControl::default();
        let host = Arc::new(CancelPostChildHost {
            state: Arc::clone(&state),
            control: control.clone(),
            spawns: AtomicUsize::new(0),
        });
        let lease = fixture_lease(fixture.path());

        assert!(
            ProductionInstallDriver::new(host)
                .install_client_bootstrap(&paths, &bootstrap, &lease, &control)
                .is_err()
        );
        assert!(state.terminated.load(Ordering::Acquire));
        assert!(state.waited.load(Ordering::Acquire));
        assert_eq!(state.quiesce_spawns.load(Ordering::Acquire), 2);
        assert_eq!(
            control.cancellation_safety(),
            crate::operation::CancellationSafety::ProvenSafe
        );
    }

    #[test]
    fn a_stop_is_proven_safe_when_no_wineserver_was_left_to_kill() {
        let (fixture, paths) = managed_paths_with_runtime();
        fs::create_dir_all(paths.game()).unwrap();
        let bootstrap = verified_test_artifact(
            ArtifactKind::ClientBootstrap,
            fixture.path().join("cancelled-bootstrap.exe"),
        );
        // The stopped child was the server's last client: `wineserver -k`
        // exits 1 (no server), and `wineserver -w` proves the prefix quiet.
        struct NoServerHost {
            state: Arc<CancelProcessState>,
            spawns: AtomicUsize,
        }
        impl HostExecution for NoServerHost {
            fn spawn(&self, _: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
                match self.spawns.fetch_add(1, Ordering::AcqRel) {
                    0 => Ok(Box::new(CancelProcess {
                        state: Arc::clone(&self.state),
                    })),
                    1 => Ok(Box::new(FailedProcess)),
                    _ => Ok(Box::new(CompleteProcess)),
                }
            }
        }
        let state = Arc::new(CancelProcessState::default());
        let host = Arc::new(NoServerHost {
            state: Arc::clone(&state),
            spawns: AtomicUsize::new(0),
        });
        let control = OperationControl::default();
        control.request_cancel();
        let lease = fixture_lease(fixture.path());
        assert!(
            ProductionInstallDriver::new(host.clone())
                .install_client_bootstrap(&paths, &bootstrap, &lease, &control)
                .is_err()
        );
        assert_eq!(host.spawns.load(Ordering::Acquire), 3, "child, -k, -w");
        assert_eq!(
            control.cancellation_safety(),
            crate::operation::CancellationSafety::ProvenSafe
        );
    }

    #[test]
    fn failed_wine_quiescence_is_unproven_and_descendant_retains_mutation_lease() {
        let (fixture, paths) = managed_paths_with_runtime();
        fs::create_dir_all(paths.game()).unwrap();
        // A child without a UI helper, so the spawn order is child, then
        // the wineserver processes.
        let bootstrap = verified_test_artifact(
            ArtifactKind::ClientBootstrap,
            fixture.path().join("cancelled-bootstrap.exe"),
        );
        let process_state = Arc::new(CancelProcessState::default());
        let descendant = Arc::new(Mutex::new(None));
        let host = Arc::new(CleanupFailureHost {
            process_state,
            descendant: Arc::clone(&descendant),
            spawns: AtomicUsize::new(0),
        });
        let control = OperationControl::default();
        control.request_cancel();
        let lease_path = fixture.path().join("mutation.lock");
        let lease = fixture_lease(fixture.path());
        File::try_lock(&lease).unwrap();

        assert!(
            ProductionInstallDriver::new(host)
                .install_client_bootstrap(&paths, &bootstrap, &lease, &control)
                .is_err()
        );
        assert_eq!(
            control.cancellation_safety(),
            crate::operation::CancellationSafety::Unproven
        );
        drop(lease);
        let competitor = OpenOptions::new()
            .read(true)
            .write(true)
            .open(lease_path)
            .unwrap();
        assert!(matches!(
            File::try_lock(&competitor),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        descendant.lock().unwrap().take();
        File::try_lock(&competitor).unwrap();
    }

    #[test]
    fn a_vendor_wrapper_only_downloads_and_never_receives_the_key() {
        let (fixture, paths) = managed_paths_with_runtime();
        let artifact = verified_test_artifact(
            ArtifactKind::BaseGameWrapper,
            fixture.path().join("wrapper.exe"),
        );
        let product_key = ProductKey::new("SECRET-PRODUCT-KEY-CANARY".to_owned()).unwrap();
        let request = VendorInstallRequest {
            product: VendorProduct::BaseGame,
            artifact: &artifact,
            paths: &paths,
            product_key: &product_key,
        };
        let host = Arc::new(RecordingHost::default());
        let lease = fixture_lease(fixture.path());

        // The recording host downloads nothing, so no verified payload
        // exists and nothing is installed.
        let failure = ProductionInstallDriver::new(host.clone())
            .install_vendor(request, &lease, &OperationControl::default())
            .unwrap_err();
        // An unfinished download; no installer ran, so the stage may fetch a
        // fresh wrapper once.
        assert_eq!(failure, VendorFailure::PayloadIncomplete);
        let requests = host.requests.lock().unwrap();
        assert_eq!(requests.len(), 3, "helper, wrapper, wineserver -w");
        assert_guest_preloader(&requests[0]);
        let helper_arguments = &requests[0].arguments;
        assert_eq!(
            &helper_arguments[..helper_arguments.len() - 1],
            runtime_loader_arguments().as_slice()
        );
        assert!(
            Path::new(helper_arguments.last().unwrap())
                .starts_with(format!("/proc/{}/fd", std::process::id()))
        );
        assert_eq!(requests[0].working_directory.as_deref(), Some(paths.root()));
        assert_eq!(
            requests[0].inherited_descriptors,
            [235, 230, 231, 236, 234, 237]
        );
        assert_guest_preloader(&requests[1]);
        assert_eq!(
            requests[1].arguments,
            [
                runtime_loader_arguments(),
                vec![
                    OsString::from("/proc/self/fd/235/wrapper.exe"),
                    OsString::from(r"--saveas=C:\pd2launcher-downloads"),
                    OsString::from("--autolaunch=0"),
                ],
            ]
            .concat()
        );
        assert_eq!(requests[1].working_directory.as_deref(), Some(paths.root()));
        assert_eq!(
            requests[1].inherited_descriptors,
            [235, 230, 231, 236, 234, 233]
        );
        assert_wineserver_wait_profile(&requests[2], &paths);
        for request in requests.iter() {
            assert!(!request.rendered_debug.contains("SECRET-PRODUCT-KEY-CANARY"));
            assert!(
                request
                    .arguments
                    .iter()
                    .all(|argument| !argument.to_string_lossy().contains("SECRET"))
            );
        }
        // The download folder is Launcher-owned and inside the prefix.
        assert!(paths.root().join(WRAPPER_DOWNLOADS).is_dir());
        assert!(!directory_contains(
            fixture.path(),
            b"SECRET-PRODUCT-KEY-CANARY"
        ));
    }

    /// The helper and Blizzard's downloader run until they are stopped, as at
    /// its "Lost Internet Connection!"; every later process completes.
    #[derive(Default)]
    struct StalledDownloadHost {
        spawns: AtomicUsize,
        terminated: Arc<AtomicUsize>,
    }

    impl HostExecution for StalledDownloadHost {
        fn spawn(&self, _: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
            if self.spawns.fetch_add(1, Ordering::AcqRel) < 2 {
                Ok(Box::new(WaitingProcess {
                    terminated: Arc::clone(&self.terminated),
                }))
            } else {
                Ok(Box::new(CompleteProcess))
            }
        }
    }

    struct WaitingProcess {
        terminated: Arc<AtomicUsize>,
    }

    impl SupervisedProcess for WaitingProcess {
        fn try_wait(&mut self) -> io::Result<Option<ProcessExit>> {
            Ok(None)
        }

        fn terminate(&mut self) -> io::Result<()> {
            self.terminated.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }

        fn wait(&mut self) -> io::Result<ProcessExit> {
            Ok(ProcessExit::new(false, None))
        }
    }

    #[test]
    fn a_download_that_stops_changing_ends_the_downloader_as_stalled() {
        let (fixture, paths) = managed_paths_with_runtime();
        let artifact = verified_test_artifact(
            ArtifactKind::BaseGameWrapper,
            fixture.path().join("wrapper.exe"),
        );
        let host = Arc::new(StalledDownloadHost::default());
        let lease = fixture_lease(fixture.path());

        // A limit of zero: the first look once the downloader runs finds
        // the folder unchanged. Bounded, so a regression fails instead of
        // waiting for the install timeout.
        let (sender, receiver) = std::sync::mpsc::channel();
        let driver_host = host.clone();
        thread::spawn(move || {
            let failure = ProductionInstallDriver::new(driver_host)
                .with_download_stall_limit(Duration::ZERO)
                .download_vendor_payload(&paths, &artifact, &lease, &OperationControl::default())
                .unwrap_err();
            let _ = sender.send(failure);
            drop(fixture);
        });
        let failure = receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("the stalled downloader was not stopped");

        // A lost connection, not an expired wrapper: no fresh one is fetched.
        assert_eq!(failure, VendorFailure::DownloadStalled);
        // The downloader and its helper were both stopped.
        assert_eq!(host.terminated.load(Ordering::Acquire), 2);
    }

    #[test]
    fn a_downloader_that_receives_data_is_not_stopped_however_slowly_it_writes() {
        let (fixture, paths) = managed_paths_with_runtime();
        let artifact = verified_test_artifact(
            ArtifactKind::BaseGameWrapper,
            fixture.path().join("wrapper.exe"),
        );
        let counter = fixture.path().join("received-bytes");
        fs::write(&counter, "0\n").unwrap();
        let host = Arc::new(StalledDownloadHost::default());
        let lease = fixture_lease(fixture.path());
        // Data comes in for six seconds; nothing is written to the folder.
        // The count is written whole through a rename, as the app writes it
        // (ReceivedBytes.kt): a plain write empties the file first, and a
        // watch that reads it then counts no data.
        let feeder = {
            let counter = counter.clone();
            thread::spawn(move || {
                let started = Instant::now();
                let mut bytes = 0u64;
                let temporary = counter.with_extension("tmp");
                while started.elapsed() < Duration::from_secs(6) {
                    bytes += 128 * 1024;
                    fs::write(&temporary, format!("{bytes}\n")).unwrap();
                    fs::rename(&temporary, &counter).unwrap();
                    thread::sleep(Duration::from_millis(300));
                }
                Instant::now()
            })
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let driver_host = host.clone();
        thread::spawn(move || {
            // Twice the watch's interval, so one late look does not stop it.
            let failure = ProductionInstallDriver::new(driver_host)
                .with_download_stall_limit(Duration::from_secs(2))
                .with_received_bytes(counter)
                .download_vendor_payload(&paths, &artifact, &lease, &OperationControl::default())
                .unwrap_err();
            let _ = sender.send((failure, Instant::now()));
        });
        let (failure, stopped) = receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("the downloader was not stopped once the data stopped");
        let fed_until = feeder.join().unwrap();
        drop(fixture);
        assert_eq!(failure, VendorFailure::DownloadStalled);
        // Stopped only once no data came for the limit after the feed ended:
        // with the folder alone it would have stopped two seconds after arming.
        assert!(
            stopped >= fed_until + Duration::from_millis(500),
            "stopped {:?} before the feed ended",
            fed_until.saturating_duration_since(stopped)
        );
    }

    /// The downloader's helper exits 23 at once (Blizzard's downloader
    /// showed a message box); the downloader itself would run on.
    #[derive(Default)]
    struct DownloaderErrorHost {
        spawns: AtomicUsize,
        terminated: Arc<AtomicUsize>,
    }

    impl HostExecution for DownloaderErrorHost {
        fn spawn(&self, _: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
            match self.spawns.fetch_add(1, Ordering::AcqRel) {
                0 => Ok(Box::new(ExitedProcess(ProcessExit::new(false, Some(23))))),
                1 => Ok(Box::new(WaitingProcess {
                    terminated: Arc::clone(&self.terminated),
                })),
                _ => Ok(Box::new(CompleteProcess)),
            }
        }
    }

    struct ExitedProcess(ProcessExit);

    impl SupervisedProcess for ExitedProcess {
        fn try_wait(&mut self) -> io::Result<Option<ProcessExit>> {
            Ok(Some(self.0))
        }

        fn terminate(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_helper_error_in_the_same_poll_wins_over_a_stall_that_has_passed() {
        let (fixture, paths) = managed_paths_with_runtime();
        let host = Arc::new(DownloaderErrorHost::default());
        // The downloader is the second spawn: skip the helper's slot.
        host.spawns.store(1, Ordering::Release);
        let stall = DownloadStall::default();
        stall.mark_stalled();
        let mut helper: Option<Box<dyn SupervisedProcess>> =
            Some(Box::new(ExitedProcess(ProcessExit::new(false, Some(23)))));
        let result = ProductionInstallDriver::new(host.clone()).spawn_and_wait_accepting(
            "wine BaseGameWrapper",
            ProcessRequest::guest("/bin/false"),
            Instant::now() + Duration::from_secs(30),
            Some(&stall),
            &OperationControl::default(),
            &mut helper,
            false,
            &[],
        );
        assert!(result.is_err());
        // The helper's error ended it, not the stall.
        assert!(!stall.ended_it());
        assert_eq!(host.terminated.load(Ordering::Acquire), 1, "the downloader");
        drop((fixture, paths));
    }

    #[test]
    fn a_finished_file_that_differs_is_recorded_and_removed_so_the_retry_downloads_it() {
        let (fixture, paths) = managed_paths_with_runtime();
        let artifact = verified_test_artifact(
            ArtifactKind::BaseGameWrapper,
            fixture.path().join("wrapper.exe"),
        );
        let identity = complete_payload_identity(ArtifactKind::BaseGameWrapper).unwrap();
        let payload = paths
            .root()
            .join(WRAPPER_DOWNLOADS)
            .join(identity.source_directory);
        fs::create_dir_all(payload.join(DIRECTX_DIRECTORY)).unwrap();
        fs::write(payload.join(PAYLOAD_INSTALLER), b"not the installer").unwrap();
        fs::write(payload.join(PAYLOAD_TOME), b"tome").unwrap();
        let host = Arc::new(RecordingHost::default());
        let lease = fixture_lease(fixture.path());
        let failure = ProductionInstallDriver::new(host)
            .download_vendor_payload(&paths, &artifact, &lease, &OperationControl::default())
            .unwrap_err();
        assert_eq!(failure, VendorFailure::PayloadRejected);
        // The wrong-sized installer is gone; the rest is kept for the retry.
        assert!(!payload.join(PAYLOAD_INSTALLER).exists());
        assert!(payload.join(PAYLOAD_TOME).exists());
    }

    #[test]
    fn an_error_box_the_helper_saw_is_reported_as_the_downloaders_error() {
        let (fixture, paths) = managed_paths_with_runtime();
        let artifact = verified_test_artifact(
            ArtifactKind::BaseGameWrapper,
            fixture.path().join("wrapper.exe"),
        );
        let host = Arc::new(DownloaderErrorHost::default());
        let lease = fixture_lease(fixture.path());
        let failure = ProductionInstallDriver::new(host.clone())
            .with_download_stall_limit(Duration::ZERO)
            .download_vendor_payload(&paths, &artifact, &lease, &OperationControl::default())
            .unwrap_err();
        assert_eq!(failure, VendorFailure::DownloaderErrorDialog);
        assert_eq!(host.terminated.load(Ordering::Acquire), 1, "the downloader");
    }

    #[test]
    fn a_download_folder_stalls_a_minute_after_its_last_change_and_later_before_its_first() {
        let start = Instant::now();
        let minute = Duration::from_secs(60);
        let resume = Duration::from_secs(5 * 60);
        let empty = Duration::from_secs(11 * 60);
        let folder = |files, length| FolderState {
            files,
            length,
            blocks: length / 512,
        };
        let clock = || GrowthClock::new(minute, resume, empty, start);
        // Nothing yet: the downloader has not started, so the longest limit.
        let mut never = clock();
        assert!(!never.observe(folder(0, 0), None, start));
        assert!(!never.observe(folder(0, 0), None, start + minute * 5));
        assert!(never.observe(folder(0, 0), None, start + empty));

        // Parts kept from an earlier run: time to start and check them first.
        let mut resumed = clock();
        assert!(!resumed.observe(folder(12, 4096), None, start));
        assert!(!resumed.observe(folder(12, 4096), None, start + minute * 4));
        assert!(resumed.observe(folder(12, 4096), None, start + resume));
        // Once it writes, a minute without a change is enough.
        let mut writing = clock();
        assert!(!writing.observe(folder(12, 4096), None, start));
        assert!(!writing.observe(folder(12, 8192), None, start + minute * 3));
        assert!(writing.observe(folder(12, 8192), None, start + minute * 4));

        // A fresh download: its (empty) parts appear, then nothing comes.
        let mut clock = clock();
        assert!(!clock.observe(folder(0, 0), None, start));
        assert!(!clock.observe(folder(12, 0), None, start + Duration::from_secs(5)));
        assert!(!clock.observe(folder(12, 0), None, start + Duration::from_secs(64)));
        assert!(clock.observe(folder(12, 0), None, start + Duration::from_secs(65)));
        assert!(!clock.observe(folder(12, 8192), None, start + Duration::from_secs(66)));
        assert!(!clock.observe(folder(12, 1024), None, start + Duration::from_secs(90)));
        assert!(clock.observe(folder(12, 1024), None, start + Duration::from_secs(150)));
    }

    #[test]
    fn data_coming_in_counts_as_activity_while_the_folder_does_not_change() {
        let start = Instant::now();
        let minute = Duration::from_secs(60);
        let resume = Duration::from_secs(5 * 60);
        let empty = Duration::from_secs(11 * 60);
        let parts = FolderState {
            files: 12,
            length: 4096,
            blocks: 8,
        };
        let writing = FolderState {
            length: 8192,
            ..parts
        };
        let at = |seconds| start + Duration::from_secs(seconds);
        let clock = || GrowthClock::new(minute, resume, empty, start);

        // Writing, then a slow connection: 80 KB in each 50 s, nothing written.
        let mut slow = clock();
        assert!(!slow.observe(parts, Some(1_000_000), at(0)));
        assert!(!slow.observe(writing, Some(1_000_000), at(10)));
        assert!(!slow.observe(writing, Some(1_081_920), at(60)));
        assert!(!slow.observe(writing, Some(1_163_840), at(110)));
        // A lost connection's retries: a few KB, then nothing.
        assert!(!slow.observe(writing, Some(1_170_000), at(140)));
        assert!(slow.observe(writing, Some(1_172_000), at(170)));
        assert_eq!(slow.received_since_mark(Some(1_172_000)), Some(8_160));

        // Data before the first write only moves the wait later: kept parts
        // still get the resume limit from the last data.
        let mut resumed = clock();
        assert!(!resumed.observe(parts, Some(0), at(0)));
        assert!(!resumed.observe(parts, Some(100_000), at(20)));
        assert!(!resumed.observe(parts, Some(100_000), at(20 + 299)));
        assert!(resumed.observe(parts, Some(100_000), at(20 + 300)));

        // A trickle below the threshold adds up to it.
        let mut trickle = clock();
        assert!(!trickle.observe(parts, Some(0), at(0)));
        assert!(!trickle.observe(writing, Some(0), at(1)));
        assert!(!trickle.observe(writing, Some(40_000), at(40)));
        assert!(!trickle.observe(writing, Some(70_000), at(60)));
        assert!(!trickle.observe(writing, Some(70_000), at(119)));
        assert!(trickle.observe(writing, Some(70_000), at(120)));

        // A count that appears later starts the mark; one missing for a read
        // keeps it; one that goes down (a restarted host) counts once.
        let mut late = clock();
        assert!(!late.observe(writing, None, at(0)));
        assert!(!late.observe(writing, Some(500_000), at(30)));
        assert_eq!(late.received_since_mark(Some(500_000)), Some(0));
        assert!(!late.observe(writing, None, at(40)));
        assert_eq!(late.received_since_mark(Some(510_000)), Some(10_000));
        assert!(!late.observe(writing, Some(1_000), at(50)));
        assert_eq!(late.received_since_mark(Some(1_000)), Some(0));

        // No count from the host (the desktop): only the folder counts.
        let mut folder_only = clock();
        assert!(!folder_only.observe(parts, None, at(0)));
        assert!(!folder_only.observe(parts, None, at(240)));
        assert!(folder_only.observe(parts, None, at(300)));
    }

    #[test]
    fn the_hosts_count_is_one_number_in_its_file() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("received-bytes");
        assert_eq!(received_bytes(&file), None);
        fs::write(&file, "123456\n").unwrap();
        assert_eq!(received_bytes(&file), Some(123_456));
        fs::write(&file, "half").unwrap();
        assert_eq!(received_bytes(&file), None);
    }

    #[test]
    fn a_downloaded_payload_runs_only_after_it_verifies_and_is_kept_otherwise() {
        let (fixture, paths) = managed_paths_with_runtime();
        let artifact = verified_test_artifact(
            ArtifactKind::ExpansionWrapper,
            fixture.path().join("wrapper.exe"),
        );
        // A download that stopped early: the installer and a partial Tome.
        let payload = paths
            .root()
            .join(WRAPPER_DOWNLOADS)
            .join(EXPANSION_PAYLOAD_SOURCE);
        fs::create_dir_all(payload.join(DIRECTX_DIRECTORY)).unwrap();
        fs::write(payload.join(PAYLOAD_INSTALLER), minimal_pe()).unwrap();
        fs::write(payload.join("Installer Tome.mpq.part"), b"partial").unwrap();
        let product_key = ProductKey::new("SECRET-PRODUCT-KEY-CANARY".to_owned()).unwrap();
        let request = VendorInstallRequest {
            product: VendorProduct::Expansion,
            artifact: &artifact,
            paths: &paths,
            product_key: &product_key,
        };
        let host = Arc::new(RecordingHost::default());
        let lease = fixture_lease(fixture.path());
        let failure = ProductionInstallDriver::new(host.clone())
            .install_vendor(request, &lease, &OperationControl::default())
            .unwrap_err();
        // The Tome is still a .part: an unfinished download. No installer
        // ran, so the stage may fetch a fresh wrapper once.
        assert_eq!(failure, VendorFailure::PayloadIncomplete);
        let requests = host.requests.lock().unwrap();
        assert_eq!(requests.len(), 3, "only the wrapper ran");
        assert!(requests.iter().all(|request| {
            request
                .arguments
                .iter()
                .all(|argument| !argument.to_string_lossy().contains(PAYLOAD_INSTALLER))
        }));
        // Kept, so the downloader resumes it next time.
        assert!(payload.join("Installer Tome.mpq.part").is_file());

        // Once installed, the folder is removed.
        remove_wrapper_downloads(&paths).unwrap();
        assert!(!paths.root().join(WRAPPER_DOWNLOADS).exists());
        assert!(paths.root().join("prefix/drive_c").is_dir());
        remove_wrapper_downloads(&paths).unwrap();
    }

    #[test]
    fn gl_probe_helper_has_the_frozen_sealed_pe_identity() {
        // Frozen deliberately: rebuilding tools/gl-probe-helper.c must be a
        // reviewed change to both this test and the pinned constants.
        assert_eq!(GL_PROBE_HELPER_SIZE, 3_072);
        assert_eq!(
            GL_PROBE_HELPER_SHA256,
            "1b9c37bcf850b171f356c9e87995415c9e256a606627bc49fce2c58c38733bf6"
        );
        let mut helper = gl_probe_helper().unwrap();
        let mut bytes = Vec::new();
        helper.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len(), GL_PROBE_HELPER_SIZE);
        assert_eq!(&bytes[..2], b"MZ");
        assert_eq!(hex(&Sha256::digest(&bytes)), GL_PROBE_HELPER_SHA256);
        // It imports only what creates a hidden window and an OpenGL
        // context, and writes to standard output.
        let text = String::from_utf8_lossy(&bytes);
        for import in [
            "OPENGL32.dll",
            "glGetString",
            "wglCreateContext",
            "ChoosePixelFormat",
            "CreateWindowExW",
            "WriteFile",
        ] {
            assert!(text.contains(import), "{import}");
        }
        for absent in ["ShowWindow", "CreateProcess", "CreateFile", "Reg"] {
            assert!(!text.contains(absent), "{absent}");
        }
    }

    #[test]
    fn xinput_bridge_has_the_frozen_sealed_pe_identity_and_replaces_wines_own() {
        // Frozen deliberately: rebuilding tools/xinput-bridge.c must be a
        // reviewed change to both this test and the pinned constants.
        assert_eq!(XINPUT_BRIDGE_SIZE, 4_096);
        assert_eq!(
            XINPUT_BRIDGE_SHA256,
            "b78963150d55de280266418095e79ebeb4d754571f49076c77f8924b34ee70c7"
        );
        let fixture = tempfile::tempdir().unwrap();
        let installed = fixture.path().join(XINPUT_BRIDGE_PATH);
        fs::create_dir_all(installed.parent().unwrap()).unwrap();
        fs::write(&installed, b"MZ Wine's own xinput1_4.dll").unwrap();
        let root = Directory::open(fixture.path()).unwrap();
        install_xinput_bridge(&root).unwrap();
        let bytes = fs::read(&installed).unwrap();
        assert_eq!(bytes.len(), XINPUT_BRIDGE_SIZE);
        assert_eq!(&bytes[..2], b"MZ");
        assert_eq!(hex(&Sha256::digest(&bytes)), XINPUT_BRIDGE_SHA256);
        // It exports XInput, reads only the gamepad file the app names, and
        // starts nothing.
        let text = String::from_utf8_lossy(&bytes);
        for name in [
            "XInputGetState",
            "XInputGetCapabilities",
            "PD2_GAMEPAD_FILE",
            "MapViewOfFile",
        ] {
            assert!(text.contains(name), "{name}");
        }
        for absent in ["CreateProcess", "WriteFile", "LoadLibrary", "Reg"] {
            assert!(!text.contains(absent), "{absent}");
        }
        // An unchanged bridge is left as it is.
        let inode = fs::metadata(&installed).unwrap().ino();
        install_xinput_bridge(&root).unwrap();
        assert_eq!(fs::metadata(&installed).unwrap().ino(), inode);
    }

    #[test]
    fn downloader_ui_helper_has_the_frozen_sealed_pe_identity() {
        // Frozen deliberately: rebuilding tools/downloader-ui-helper.c must
        // be a reviewed change to both this test and the pinned constants.
        assert_eq!(DOWNLOADER_UI_HELPER_SIZE, 4_096);
        assert_eq!(
            DOWNLOADER_UI_HELPER_SHA256,
            "85416beb66fe49804766e5a2d007f2e822cb160ee8122e2de0a7d4fa524eb46e"
        );
        let mut helper = UiHelper::Downloader.sealed_file().unwrap();
        let mut bytes = Vec::new();
        helper.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len(), DOWNLOADER_UI_HELPER_SIZE);
        assert_eq!(&bytes[..2], b"MZ");
        assert_eq!(hex(&Sha256::digest(&bytes)), DOWNLOADER_UI_HELPER_SHA256);
        // It names only the downloader's own dialogs and control.
        let text = String::from_utf16_lossy(
            &bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect::<Vec<_>>(),
        );
        for reviewed in [
            "#32770",
            " - Blizzard Downloader",
            "100% - Blizzard Downloader",
            "Browse for Folder",
            "Browse For Folder",
        ] {
            assert!(text.contains(reviewed), "{reviewed}");
        }
    }

    #[test]
    fn vendor_child_output_cannot_enter_adapter_failures_or_launcher_storage() {
        let (fixture, paths) = managed_paths_with_runtime();
        let artifact = verified_test_artifact(
            ArtifactKind::BaseGameWrapper,
            fixture.path().join("wrapper.exe"),
        );
        let product_key = ProductKey::new("VALID-PRODUCT-KEY-FIXTURE".to_owned()).unwrap();
        let request = VendorInstallRequest {
            product: VendorProduct::BaseGame,
            artifact: &artifact,
            paths: &paths,
            product_key: &product_key,
        };

        let lease = fixture_lease(fixture.path());
        let error = ProductionInstallDriver::new(Arc::new(OutputFailureHost))
            .install_vendor(request, &lease, &OperationControl::default())
            .unwrap_err();

        assert!(!format!("{error:?}").contains("SECRET-CHILD-OUTPUT-CANARY"));
        assert!(!directory_contains(
            fixture.path(),
            b"SECRET-CHILD-OUTPUT-CANARY"
        ));
    }

    #[test]
    fn bootstrap_profile_is_silent_no_restart_no_launch_and_exact_destination() {
        let (fixture, paths) = managed_paths_with_runtime();
        let bootstrap = verified_test_artifact(
            ArtifactKind::ClientBootstrap,
            fixture.path().join("ProjectD2Installer.exe"),
        );
        let host = Arc::new(RecordingHost::default());
        let lease = fixture_lease(fixture.path());

        ProductionInstallDriver::new(host.clone())
            .install_client_bootstrap(&paths, &bootstrap, &lease, &OperationControl::default())
            .unwrap();

        let requests = host.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_guest_preloader(&requests[0]);
        assert_eq!(
            requests[0].arguments,
            [
                runtime_loader_arguments(),
                vec![
                    OsString::from("/proc/self/fd/233"),
                    OsString::from("/SP-"),
                    OsString::from("/VERYSILENT"),
                    OsString::from("/SUPPRESSMSGBOXES"),
                    OsString::from("/NORESTART"),
                    OsString::from(r"/DIR=C:\Program Files (x86)\Diablo II"),
                ],
            ]
            .concat()
        );
        assert_eq!(requests[0].working_directory.as_deref(), Some(paths.root()));
        assert_eq!(
            requests[0].inherited_descriptors,
            [235, 230, 231, 236, 234, 233]
        );
        assert_wineserver_wait_profile(&requests[1], &paths);
    }

    struct InterruptedBody {
        prefix: Cursor<Vec<u8>>,
    }

    struct SubstitutePartialOnRead {
        body: Cursor<Vec<u8>>,
        cache: PathBuf,
        substituted: bool,
    }

    struct SubstituteAndCancelPartialOnRead {
        body: Cursor<Vec<u8>>,
        cache: PathBuf,
        control: OperationControl,
        substituted: bool,
    }

    impl Read for SubstitutePartialOnRead {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.substituted {
                self.substituted = true;
                let partial = self
                    .cache
                    .read_dir()?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .find(|path| {
                        path.file_name()
                            .is_some_and(|name| name.to_string_lossy().ends_with(".partial"))
                    })
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "partial missing"))?;
                fs::rename(&partial, self.cache.join("retained-response.partial"))?;
                fs::write(&partial, b"attacker")?;
            }
            self.body.read(buffer)
        }
    }

    impl Read for SubstituteAndCancelPartialOnRead {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.substituted {
                self.substituted = true;
                let partial = self
                    .cache
                    .read_dir()?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .find(|path| {
                        path.file_name()
                            .is_some_and(|name| name.to_string_lossy().ends_with(".partial"))
                    })
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "partial missing"))?;
                fs::rename(&partial, self.cache.join("retained-response.partial"))?;
                fs::write(&partial, b"attacker")?;
                self.control.request_cancel();
            }
            self.body.read(buffer)
        }
    }

    impl Read for InterruptedBody {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let read = self.prefix.read(buffer)?;
            if read == 0 {
                Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "fixture interruption",
                ))
            } else {
                Ok(read)
            }
        }
    }

    struct CancelAfterFirstRead {
        body: Cursor<Vec<u8>>,
        control: OperationControl,
        requested: bool,
    }

    impl Read for CancelAfterFirstRead {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let read = self.body.read(buffer)?;
            if read > 0 && !self.requested {
                self.requested = true;
                self.control.request_cancel();
            }
            Ok(read)
        }
    }

    #[derive(Debug, Eq, PartialEq)]
    struct RequestSnapshot {
        executable: PathBuf,
        guest: bool,
        arguments: Vec<OsString>,
        working_directory: Option<PathBuf>,
        clears_environment: bool,
        environment: Vec<(OsString, OsString)>,
        output_limit: Option<usize>,
        inherited_descriptors: Vec<i32>,
        rendered_debug: String,
    }

    #[derive(Default)]
    struct RecordingHost {
        requests: Mutex<Vec<RequestSnapshot>>,
    }

    impl HostExecution for RecordingHost {
        fn spawn(&self, request: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
            let snapshot = RequestSnapshot {
                executable: request.executable().to_owned(),
                guest: request.is_guest(),
                arguments: request.arguments().to_vec(),
                working_directory: request.working_directory().map(Path::to_owned),
                clears_environment: request.clears_environment(),
                environment: request.environment().to_vec(),
                output_limit: request.output_limit(),
                inherited_descriptors: request.inherited_file_descriptors().collect(),
                rendered_debug: format!("{request:?}"),
            };
            self.requests.lock().unwrap().push(snapshot);
            Ok(Box::new(CompleteProcess))
        }
    }

    struct CompleteProcess;

    impl SupervisedProcess for CompleteProcess {
        fn try_wait(&mut self) -> io::Result<Option<ProcessExit>> {
            Ok(Some(ProcessExit::new(true, Some(0))))
        }

        fn terminate(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct CancelProcessState {
        terminated: AtomicBool,
        waited: AtomicBool,
        quiesce_spawns: AtomicUsize,
    }

    struct CancelHost {
        state: Arc<CancelProcessState>,
        spawns: AtomicUsize,
    }

    struct CancelPostChildHost {
        state: Arc<CancelProcessState>,
        control: OperationControl,
        spawns: AtomicUsize,
    }

    impl HostExecution for CancelPostChildHost {
        fn spawn(&self, _: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
            match self.spawns.fetch_add(1, Ordering::AcqRel) {
                0 => Ok(Box::new(CompleteProcess)),
                1 => {
                    self.control.request_cancel();
                    Ok(Box::new(CancelProcess {
                        state: Arc::clone(&self.state),
                    }))
                }
                _ => {
                    self.state.quiesce_spawns.fetch_add(1, Ordering::AcqRel);
                    Ok(Box::new(CompleteProcess))
                }
            }
        }
    }

    impl HostExecution for CancelHost {
        fn spawn(&self, _: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
            let spawn = self.spawns.fetch_add(1, Ordering::AcqRel);
            if spawn == 0 {
                Ok(Box::new(CancelProcess {
                    state: Arc::clone(&self.state),
                }))
            } else {
                self.state.quiesce_spawns.fetch_add(1, Ordering::AcqRel);
                Ok(Box::new(CompleteProcess))
            }
        }
    }

    struct CancelProcess {
        state: Arc<CancelProcessState>,
    }

    impl SupervisedProcess for CancelProcess {
        fn try_wait(&mut self) -> io::Result<Option<ProcessExit>> {
            Ok(None)
        }

        fn terminate(&mut self) -> io::Result<()> {
            self.state.terminated.store(true, Ordering::Release);
            Ok(())
        }

        fn wait(&mut self) -> io::Result<ProcessExit> {
            self.state.waited.store(true, Ordering::Release);
            Ok(ProcessExit::new(false, None))
        }
    }

    struct CleanupFailureHost {
        process_state: Arc<CancelProcessState>,
        descendant: Arc<Mutex<Option<File>>>,
        spawns: AtomicUsize,
    }

    impl HostExecution for CleanupFailureHost {
        fn spawn(&self, request: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
            match self.spawns.fetch_add(1, Ordering::AcqRel) {
                0 => {
                    *self.descendant.lock().unwrap() =
                        Some(request.clone_inherited_file(CHILD_MUTATION_LEASE_FD)?);
                    Ok(Box::new(CancelProcess {
                        state: Arc::clone(&self.process_state),
                    }))
                }
                1 => Ok(Box::new(CompleteProcess)),
                _ => Ok(Box::new(FailedProcess)),
            }
        }
    }

    struct FailedProcess;

    impl SupervisedProcess for FailedProcess {
        fn try_wait(&mut self) -> io::Result<Option<ProcessExit>> {
            Ok(Some(ProcessExit::new(false, Some(1))))
        }

        fn terminate(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct OutputFailureHost;

    impl HostExecution for OutputFailureHost {
        fn spawn(&self, _: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
            Ok(Box::new(OutputFailureProcess {
                output: crate::host_execution::ProcessOutput::new(
                    b"SECRET-CHILD-OUTPUT-CANARY".to_vec(),
                    b"SECRET-CHILD-OUTPUT-CANARY".to_vec(),
                    false,
                    false,
                ),
            }))
        }
    }

    struct OutputFailureProcess {
        output: crate::host_execution::ProcessOutput,
    }

    impl SupervisedProcess for OutputFailureProcess {
        fn try_wait(&mut self) -> io::Result<Option<ProcessExit>> {
            Ok(Some(ProcessExit::new(false, Some(1))))
        }

        fn terminate(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn output(&self) -> crate::host_execution::ProcessOutput {
            self.output.clone()
        }
    }

    #[derive(Clone, Copy)]
    enum HelperScript {
        ExitsOk,
        ExitsFailed,
        /// Fails with this exit status.
        ExitsWith(i32),
        NeverExits,
    }

    /// Scripted helper, child, and wineserver processes that record their
    /// spawns, exits, and terminations in order.
    struct LifetimeHost {
        events: Mutex<Vec<String>>,
        helper_exited: AtomicBool,
        helper: HelperScript,
        /// `wineserver -w` reports quiet even while the helper runs.
        quiet_while_helper_runs: bool,
        /// Requested when the first `wineserver -w` starts: a stop during
        /// the post-child wait, while the helper is still alive.
        cancel_on_wait: Mutex<Option<OperationControl>>,
    }

    impl LifetimeHost {
        fn new(helper: HelperScript, quiet_while_helper_runs: bool) -> Arc<Self> {
            Arc::new(Self {
                events: Mutex::new(Vec::new()),
                helper_exited: AtomicBool::new(false),
                helper,
                quiet_while_helper_runs,
                cancel_on_wait: Mutex::new(None),
            })
        }

        fn record(&self, event: String) {
            self.events.lock().unwrap().push(event);
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }
    }

    impl HostExecution for Arc<LifetimeHost> {
        fn spawn(&self, request: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
            let arguments = request.arguments();
            let role = if request.executable() == Path::new("/proc/self/fd/232") {
                if arguments.iter().any(|argument| argument == "-k") {
                    "wineserver -k"
                } else {
                    "wineserver -w"
                }
            } else if arguments.iter().any(|argument| {
                Path::new(argument).starts_with(format!("/proc/{}/fd", std::process::id()))
            }) {
                "helper"
            } else {
                "child"
            };
            self.record(format!("spawn {role}"));
            if role == "wineserver -w"
                && let Some(control) = self.cancel_on_wait.lock().unwrap().take()
            {
                control.request_cancel();
            }
            Ok(Box::new(LifetimeProcess {
                host: Arc::clone(self),
                helper_path: (role == "helper").then(|| PathBuf::from(arguments.last().unwrap())),
                role,
                polls: 0,
                exited: false,
            }))
        }
    }

    struct LifetimeProcess {
        host: Arc<LifetimeHost>,
        role: &'static str,
        helper_path: Option<PathBuf>,
        polls: usize,
        exited: bool,
    }

    impl SupervisedProcess for LifetimeProcess {
        fn try_wait(&mut self) -> io::Result<Option<ProcessExit>> {
            if let Some(path) = &self.helper_path {
                // The spawn request has already been dropped. A separate
                // process must still be able to open the sealed executable.
                let image = File::open(path).expect("helper image retained during supervision");
                let digest = hex(&Sha256::digest(fs::read(path).unwrap()));
                assert!(digest == VENDOR_UI_HELPER_SHA256 || digest == DOWNLOADER_UI_HELPER_SHA256);
                assert!(
                    rustix::fs::fcntl_get_seals(&image)
                        .unwrap()
                        .contains(SealFlags::WRITE)
                );
            }
            self.polls += 1;
            let code = match self.host.helper {
                HelperScript::ExitsWith(code) if self.role == "helper" => code,
                _ => 1,
            };
            let success = match self.role {
                "helper" => match self.host.helper {
                    HelperScript::ExitsOk if self.polls >= 3 => Some(true),
                    HelperScript::ExitsFailed | HelperScript::ExitsWith(_) if self.polls >= 3 => {
                        Some(false)
                    }
                    _ => None,
                },
                "wineserver -w" => (self.host.quiet_while_helper_runs
                    || self.host.helper_exited.load(Ordering::Acquire))
                .then_some(true),
                _ => Some(true),
            };
            if let Some(success) = success
                && !self.exited
            {
                self.exited = true;
                if self.role == "helper" {
                    self.host.helper_exited.store(true, Ordering::Release);
                }
                let outcome = if success { "ok" } else { "failed" };
                self.host.record(format!("exit {} {outcome}", self.role));
            }
            Ok(success
                .map(|success| ProcessExit::new(success, Some(if success { 0 } else { code }))))
        }

        fn terminate(&mut self) -> io::Result<()> {
            self.host.record(format!("terminate {}", self.role));
            if self.role == "helper" {
                // A killed helper is no longer a Wine client.
                self.host.helper_exited.store(true, Ordering::Release);
            }
            Ok(())
        }

        fn wait(&mut self) -> io::Result<ProcessExit> {
            self.exited = true;
            Ok(ProcessExit::new(false, None))
        }
    }

    fn served(final_url: &str, bytes: &[u8], attachment: Option<&str>) -> FakeHttp {
        FakeHttp {
            response: Mutex::new(Some(ArtifactHttpResponse {
                final_url: final_url.to_owned(),
                status: 200,
                declared_length: Some(bytes.len() as u64),
                content_range: None,
                suggested_filename: attachment.map(str::to_owned),
                retry_after: None,
                body: Box::new(Cursor::new(bytes.to_vec())),
            })),
        }
    }

    /// Serves each URL from its own queue of responses (a URL with none left
    /// fails the request), records every URL requested, and may stop the job
    /// at its first request.
    #[derive(Default)]
    struct RoutedHttp {
        routes: Mutex<std::collections::HashMap<String, VecDeque<ArtifactHttpResponse>>>,
        requested: Arc<Mutex<Vec<String>>>,
        stop_at_first_request: bool,
    }

    impl RoutedHttp {
        fn route(self, url: &str, final_url: &str, bytes: &[u8]) -> Self {
            self.routes
                .lock()
                .unwrap()
                .entry(url.to_owned())
                .or_default()
                .push_back(
                    served(final_url, bytes, None)
                        .response
                        .into_inner()
                        .unwrap()
                        .unwrap(),
                );
            self
        }
    }

    impl ArtifactHttp for RoutedHttp {
        fn get(
            &self,
            spec: &ArtifactSpec,
            _range_start: Option<u64>,
            control: &OperationControl,
        ) -> Result<ArtifactHttpResponse, AdapterFailure> {
            self.requested
                .lock()
                .unwrap()
                .push(spec.request_url().to_owned());
            if self.stop_at_first_request {
                control.request_cancel();
                return Err(AdapterFailure::here());
            }
            self.routes
                .lock()
                .unwrap()
                .get_mut(spec.request_url())
                .and_then(VecDeque::pop_front)
                .ok_or_else(|| AdapterFailure::here())
        }
    }

    const MIRROR: &str = "https://github.com/fixture/fixture/releases/download/fex-0/fex.deb";
    const LAUNCHPAD_FILES: &str =
        "https://launchpad.net/~fixture/+archive/ubuntu/fixture/+files/fex_0-1~j_arm64.deb";
    const LAUNCHPAD_POOL: &str = "https://ppa.launchpadcontent.net/fixture/fex_0-1~j_arm64.deb";

    /// A built-in FEX file as the catalog has it: the mirror release, then
    /// Launchpad's file link, then its pool.
    fn mirrored_fixture_spec(bytes: &'static [u8]) -> ArtifactSpec {
        ArtifactSpec {
            kind: ArtifactKind::FexPackage,
            request_url: Cow::Borrowed(MIRROR),
            redirects: RedirectPolicy::GithubReleaseAsset,
            fallback_urls: &[LAUNCHPAD_FILES, LAUNCHPAD_POOL],
            ..fixed_fixture_spec(bytes)
        }
    }

    fn requested_sources(requested: &Mutex<Vec<String>>) -> Vec<String> {
        let mut sources = requested.lock().unwrap().clone();
        sources.dedup();
        sources
    }

    #[test]
    fn a_mirrored_file_comes_from_the_mirror_when_it_has_it() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"the pinned package";
        let spec = mirrored_fixture_spec(bytes);
        let http = RoutedHttp::default()
            .route(
                MIRROR,
                "https://release-assets.githubusercontent.com/github-production-release-asset/1/2?sig=3",
                bytes,
            )
            .route(LAUNCHPAD_FILES, LAUNCHPAD_FILES, bytes);
        let requested = Arc::clone(&http.requested);
        let acquired = ProductionArtifactSource::with_http(http)
            .acquire(&spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(requested_sources(&requested), [MIRROR]);
        assert_eq!(acquired.requested_url, MIRROR);
        let verified = verify_artifact(&spec, cache.path(), acquired).unwrap();
        assert_eq!(fs::read(verified.path()).unwrap(), bytes);
    }

    #[test]
    fn each_source_is_tried_in_order_until_one_gives_the_pinned_bytes() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"the pinned package";
        let spec = mirrored_fixture_spec(bytes);
        // The mirror does not answer; Launchpad's file link sends the request
        // to a host it may not; the pool has the file.
        let http = RoutedHttp::default()
            .route(
                LAUNCHPAD_FILES,
                "https://elsewhere.invalid/fex_0-1~j_arm64.deb",
                bytes,
            )
            .route(LAUNCHPAD_POOL, LAUNCHPAD_POOL, bytes);
        let requested = Arc::clone(&http.requested);
        let acquired = ProductionArtifactSource::with_http(http)
            .acquire(&spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(
            requested_sources(&requested),
            [MIRROR, LAUNCHPAD_FILES, LAUNCHPAD_POOL]
        );
        assert_eq!(acquired.requested_url, LAUNCHPAD_POOL);
        // The workflow's own check accepts a fallback's provenance.
        let verified = verify_artifact(&spec, cache.path(), acquired).unwrap();
        assert_eq!(fs::read(verified.path()).unwrap(), bytes);

        // Launchpad's file link and its librarian's copy are accepted too,
        // and other bytes from the mirror are not.
        let cache = tempfile::tempdir().unwrap();
        let http = RoutedHttp::default()
            .route(
                MIRROR,
                "https://release-assets.githubusercontent.com/1/2",
                b"not the pinned one",
            )
            .route(
                LAUNCHPAD_FILES,
                "https://launchpadlibrarian.net/877920585/fex_0-1~j_arm64.deb",
                bytes,
            );
        let requested = Arc::clone(&http.requested);
        let acquired = ProductionArtifactSource::with_http(http)
            .acquire(&spec, cache.path(), &OperationControl::default())
            .unwrap();
        assert_eq!(requested_sources(&requested), [MIRROR, LAUNCHPAD_FILES]);
        assert_eq!(
            acquired.final_url,
            "https://launchpadlibrarian.net/877920585/fex_0-1~j_arm64.deb"
        );
        verify_artifact(&spec, cache.path(), acquired).unwrap();
    }

    #[test]
    fn a_mirrored_file_fails_only_when_every_source_failed_and_a_stop_tries_no_other() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"the pinned package";
        let spec = mirrored_fixture_spec(bytes);
        let http = RoutedHttp::default().route(
            LAUNCHPAD_FILES,
            "https://launchpadlibrarian.net/1/fex_0-1~j_arm64.deb",
            b"not the pinned one",
        );
        let requested = Arc::clone(&http.requested);
        assert!(
            ProductionArtifactSource::with_http(http)
                .acquire(&spec, cache.path(), &OperationControl::default())
                .is_err()
        );
        assert_eq!(
            requested_sources(&requested),
            [MIRROR, LAUNCHPAD_FILES, LAUNCHPAD_POOL]
        );
        assert!(!cache.path().join(cache_name(&spec)).exists());

        // The player's Stop during the first source ends it there.
        let http = RoutedHttp {
            stop_at_first_request: true,
            ..RoutedHttp::default()
        };
        let requested = Arc::clone(&http.requested);
        let control = OperationControl::default();
        assert!(
            ProductionArtifactSource::with_http(http)
                .acquire(&spec, cache.path(), &control)
                .is_err()
        );
        assert!(control.is_cancelled());
        assert_eq!(*requested.lock().unwrap(), [MIRROR]);
    }

    /// `cargo test -- --ignored --nocapture the_built_in_fex_packages_download`
    /// downloads every CPU level's package from its real sources (standard
    /// error names the host each came from).
    #[test]
    #[ignore = "downloads from the real sources"]
    fn the_built_in_fex_packages_download_from_their_real_sources() {
        use crate::builtin_fex::FexVariant;
        for variant in [
            FexVariant::Armv8_0,
            FexVariant::Armv8_2,
            FexVariant::Armv8_4,
        ] {
            let cache = tempfile::tempdir().unwrap();
            let spec =
                ArtifactCatalog::builtin_fex_spec(ArtifactKind::FexPackage, variant.package());
            let acquired = ProductionArtifactSource::new()
                .acquire(&spec, cache.path(), &OperationControl::default())
                .unwrap();
            eprintln!(
                "{variant:?}: requested {}, served from {}",
                acquired.requested_url, acquired.final_url
            );
            let verified = verify_artifact(&spec, cache.path(), acquired).unwrap();
            assert_eq!(verified.content_sha256(), variant.package_sha256());
        }
    }

    #[test]
    fn custom_source_bypasses_integrity_while_the_builtin_still_rejects_a_bad_hash() {
        let cache = tempfile::tempdir().unwrap();
        let custom_url = "https://mirror.example/any-gecko.msi?token=abc";
        let spec = ArtifactSpec::custom(ArtifactKind::Gecko, custom_url.to_owned(), 1024);
        let bytes = b"arbitrary bytes that match no pin";
        // The mirror redirected; a custom source accepts any http(s) final URL.
        let acquired = ProductionArtifactSource::with_http(served(
            "https://cdn.mirror.example/final.msi",
            bytes,
            None,
        ))
        .acquire(&spec, cache.path(), &OperationControl::default())
        .unwrap();
        assert_eq!(acquired.path, cache.path().join("custom-gecko.download"));
        let verified = verify_artifact(&spec, cache.path(), acquired).unwrap();
        assert_eq!(verified.custom_source(), Some(custom_url));
        assert_eq!(fs::read(verified.path()).unwrap(), bytes);

        // The cache receipt never stores the token-bearing URL.
        let receipt =
            fs::read_to_string(cache.path().join("custom-gecko.download.provenance.json")).unwrap();
        assert!(!receipt.contains("token"), "{receipt}");
        assert!(receipt.contains(&custom_url_fingerprint(custom_url)));
        assert!(receipt.contains("https://cdn.mirror.example/final.msi"));

        // Reused offline only for the exact same custom URL.
        let reused = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(None),
        })
        .acquire(&spec, cache.path(), &OperationControl::default())
        .unwrap();
        assert_eq!(fs::read(&reused.path).unwrap(), bytes);
        let reverified = verify_artifact(&spec, cache.path(), reused).unwrap();
        assert_eq!(reverified.custom_source(), Some(custom_url));
        let other = ArtifactSpec::custom(
            ArtifactKind::Gecko,
            "https://other.example/gecko.msi".to_owned(),
            1024,
        );
        assert!(
            ProductionArtifactSource::with_http(FakeHttp {
                response: Mutex::new(None),
            })
            .acquire(&other, cache.path(), &OperationControl::default())
            .is_err(),
            "a cached custom object must not satisfy a different custom URL"
        );

        // The built-in pinned source keeps full verification and never reuses
        // a custom-source object.
        let builtin = fixed_fixture_spec(b"pinned fixture!!");
        assert!(
            ProductionArtifactSource::with_http(served(
                builtin.request_url(),
                b"tampered fixture",
                None,
            ))
            .acquire(&builtin, cache.path(), &OperationControl::default())
            .is_err()
        );
        assert!(!cache.path().join(cache_name(&builtin)).exists());
        let redirected = fixed_fixture_spec(b"pinned fixture!!");
        assert!(
            ProductionArtifactSource::with_http(served(
                "https://cdn.mirror.example/final.msi",
                b"pinned fixture!!",
                None,
            ))
            .acquire(&redirected, cache.path(), &OperationControl::default())
            .is_err()
        );
    }

    #[test]
    fn custom_source_size_cap_is_enforced_before_promotion() {
        let cache = tempfile::tempdir().unwrap();
        let spec = ArtifactSpec::custom(
            ArtifactKind::ClientBootstrap,
            "https://mirror.example/pd2.exe".to_owned(),
            8,
        );
        assert_eq!(spec.maximum_bytes(), 8);
        assert!(
            ProductionArtifactSource::with_http(served(
                spec.request_url(),
                b"sixteen bytes!!!",
                None,
            ))
            .acquire(&spec, cache.path(), &OperationControl::default())
            .is_err()
        );
        let undeclared = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(Some(ArtifactHttpResponse {
                final_url: spec.request_url().to_owned(),
                status: 200,
                declared_length: None,
                content_range: None,
                suggested_filename: None,
                retry_after: None,
                body: Box::new(Cursor::new(vec![7_u8; 64])),
            })),
        });
        assert!(
            undeclared
                .acquire(&spec, cache.path(), &OperationControl::default())
                .is_err()
        );
        assert!(public_cache_entries(cache.path()).is_empty());
        assert_eq!(
            ArtifactCatalog::with_sources(&crate::sources::DownloadSources::resolve(
                &[(
                    crate::sources::DownloadArtifact::Pd2Installer,
                    "https://mirror.example/pd2.exe".to_owned()
                )]
                .into(),
                &Default::default(),
            ))
            .spec(ArtifactKind::ClientBootstrap)
            .maximum_bytes(),
            crate::sources::CUSTOM_ARTIFACT_MAX_BYTES
        );
    }

    #[test]
    fn custom_sources_allow_plain_http_and_redirects_but_builtins_do_not() {
        let plain = "http://mirror.example/wine.tar.xz".to_owned();
        let custom = ArtifactSpec::custom(ArtifactKind::ManagedRuntime, plain.clone(), 1024);
        assert!(approved_request_transport(&custom));
        assert!(approved_redirect(&custom, "https://anywhere.example/x", 3));
        assert!(approved_redirect(&custom, &plain, 0));
        assert!(!approved_redirect(&custom, "ftp://anywhere.example/x", 0));
        assert!(!approved_redirect(
            &custom,
            "https://anywhere.example/x",
            CUSTOM_REDIRECT_LIMIT
        ));
        let catalog = ArtifactCatalog::pinned();
        let gecko = catalog.spec(ArtifactKind::Gecko);
        assert!(!gecko.is_custom());
        assert!(!approved_redirect(gecko, "https://anywhere.example/x", 0));
        assert!(!gecko.approves_final_url("https://anywhere.example/x"));
        let mut builtin_http = gecko.clone();
        builtin_http.request_url = plain.into();
        assert!(!approved_request_transport(&builtin_http));
    }

    #[test]
    fn blizzard_download_feeds_the_same_wrapper_validation_as_local_media() {
        let catalog = ArtifactCatalog::pinned();
        let spec = catalog.spec(ArtifactKind::BaseGameWrapper);
        let filename = "Downloader_Diablo2_enUS.exe";

        let cache = tempfile::tempdir().unwrap();
        let acquired = ProductionArtifactSource::with_http(served(
            spec.request_url(),
            &release_sized_pe(),
            Some(filename),
        ))
        .acquire(spec, cache.path(), &OperationControl::default())
        .unwrap();
        assert_eq!(acquired.origin, ArtifactOrigin::ApprovedNetwork);
        // Same Launcher-owned cache object and receipt as an imported folder.
        assert_eq!(acquired.path, cache.path().join("d2-wrapper.verified"));
        let verified = verify_artifact(spec, cache.path(), acquired).unwrap();
        assert_eq!(verified.custom_source(), None);
        let receipt: CacheReceipt =
            serde_json::from_slice(&fs::read(cache.path().join(cache_receipt_name(spec))).unwrap())
                .unwrap();
        assert_eq!(receipt.attachment, filename);
        assert_eq!(receipt.origin, ArtifactOrigin::ApprovedNetwork);
        let reused = ProductionArtifactSource::with_http(FakeHttp {
            response: Mutex::new(None),
        })
        .acquire(spec, cache.path(), &OperationControl::default())
        .unwrap();
        assert_eq!(fs::read(reused.path).unwrap(), release_sized_pe());

        // Not a PE, too small, or the wrong attachment: rejected exactly like
        // a folder missing a valid installer.
        let mut not_pe = release_sized_pe();
        not_pe[..2].copy_from_slice(b"ZM");
        for (bytes, attachment) in [
            (not_pe, Some(filename)),
            (minimal_pe(), Some(filename)),
            (release_sized_pe(), Some("Other.exe")),
            (release_sized_pe(), None),
        ] {
            let cache = tempfile::tempdir().unwrap();
            assert!(
                ProductionArtifactSource::with_http(served(spec.request_url(), &bytes, attachment))
                    .acquire(spec, cache.path(), &OperationControl::default())
                    .is_err()
            );
            assert!(!cache.path().join("d2-wrapper.verified").exists());
        }
    }

    fn custom_runtime_archive(top: &str) -> Vec<u8> {
        let mut elf = b"\x7fELF".to_vec();
        elf.extend_from_slice(b" custom wine");
        build_archive(|builder| {
            append_directory(builder, &format!("{top}/"));
            append_directory(builder, &format!("{top}/bin/"));
            append_file(builder, &format!("{top}/bin/wine"), &elf);
            append_file(builder, &format!("{top}/bin/wineserver"), b"server");
            append_file(builder, &format!("{top}/lib/wine/readme"), b"data");
        })
    }

    fn custom_runtime_artifact(paths: &ManagedPaths, bytes: &[u8]) -> VerifiedArtifact {
        let cache = paths.root().join("cache/artifacts");
        fs::create_dir_all(&cache).unwrap();
        let path = cache.join("custom-wine.download");
        fs::write(&path, bytes).unwrap();
        VerifiedArtifact {
            kind: ArtifactKind::ManagedRuntime,
            path,
            origin: ArtifactOrigin::ApprovedNetwork,
            observed_sha256: hex(&Sha256::digest(bytes)),
            retained_receipt_sha256: None,
            requested_url: "https://mirror.example/wine.tar.xz".to_owned(),
            final_url: "https://mirror.example/wine.tar.xz".to_owned(),
            custom_source: Some("https://mirror.example/wine.tar.xz?sig=secret".to_owned()),
        }
    }

    #[test]
    fn custom_wine_archive_materializes_under_its_own_top_level_with_receipt_last() {
        let fixture = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(fixture.path()).unwrap();
        let archive = custom_runtime_archive("wine-9.22-staging-amd64");
        let artifact = custom_runtime_artifact(&paths, &archive);

        ensure_runtime(&paths, &artifact, &OperationControl::default()).unwrap();
        let runtime = fixture.path().join("runner/custom-wine");
        assert!(runtime.join("bin/wine").is_file());
        assert!(runtime.join("lib/wine/readme").is_file());
        assert!(!fixture.path().join("runner/custom-wine.staging").exists());
        assert!(
            !fixture
                .path()
                .join("runner/wine-9.22-staging-amd64")
                .exists()
        );
        let receipt = CustomSourceReceipt::decode(
            &fs::read(runtime.join(CUSTOM_RUNTIME_RECEIPT)).unwrap(),
            DownloadArtifact::Wine,
        )
        .unwrap();
        assert_eq!(receipt.host(), "mirror.example");
        assert_eq!(receipt.sha256(), artifact.content_sha256());
        let text = fs::read_to_string(runtime.join(CUSTOM_RUNTIME_RECEIPT)).unwrap();
        assert!(!text.contains("secret"), "{text}");
        assert_eq!(
            InstalledRuntime::select(paths.directory(), true),
            InstalledRuntime::Custom
        );
        let handles = RuntimeHandles::open(&paths, InstalledRuntime::Custom).unwrap();
        assert_eq!(handles.runtime, InstalledRuntime::Custom);
        assert!(loader_arguments(handles.runtime).is_empty());

        // The built-in Wine source never selects the custom runtime.
        assert_eq!(
            InstalledRuntime::select(paths.directory(), false),
            InstalledRuntime::BuiltIn(managed_runtime())
        );

        // Idempotent: a complete custom runtime is kept as installed.
        ensure_runtime(&paths, &artifact, &OperationControl::default()).unwrap();
        // A different archive never silently replaces the installed runtime.
        let other = custom_runtime_artifact(&paths, &custom_runtime_archive("other"));
        ensure_runtime(&paths, &other, &OperationControl::default()).unwrap();
        assert_eq!(
            CustomSourceReceipt::decode(
                &fs::read(runtime.join(CUSTOM_RUNTIME_RECEIPT)).unwrap(),
                DownloadArtifact::Wine,
            )
            .unwrap()
            .sha256(),
            hex(&Sha256::digest(&archive))
        );
    }

    #[test]
    fn custom_wine_archive_keeps_extraction_safety_rules() {
        for archive in [archive_with_parent_path(), archive_with_symlink(), {
            // Two top-level directories: not a single runtime root.
            build_archive(|builder| {
                append_directory(builder, "first/");
                append_file(builder, "first/bin/wine", b"\x7fELF");
                append_file(builder, "second/escape", b"x");
            })
        }] {
            let fixture = tempfile::tempdir().unwrap();
            let paths = ManagedPaths::prepare(fixture.path()).unwrap();
            let artifact = custom_runtime_artifact(&paths, &archive);
            assert!(ensure_runtime(&paths, &artifact, &OperationControl::default()).is_err());
            assert!(!fixture.path().join("outside.txt").exists());
            assert!(
                !fixture
                    .path()
                    .join("runner/custom-wine")
                    .join(CUSTOM_RUNTIME_RECEIPT)
                    .exists()
            );
            assert_eq!(
                InstalledRuntime::select(paths.directory(), true),
                InstalledRuntime::BuiltIn(managed_runtime())
            );
        }
        // A payload without an executable ELF `bin/wine` is rejected too.
        let fixture = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(fixture.path()).unwrap();
        let artifact = custom_runtime_artifact(&paths, &safe_archive());
        assert!(ensure_runtime(&paths, &artifact, &OperationControl::default()).is_err());
    }

    #[test]
    fn a_tampered_pinned_runtime_is_never_executed_after_custom_materialization() {
        use std::os::unix::fs::MetadataExt;
        let fixture = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(fixture.path()).unwrap();
        // A pinned receipt is present, but the pinned payload was tampered.
        let pinned = paths.runtime().join("bin");
        fs::create_dir_all(&pinned).unwrap();
        fs::write(
            paths.runtime().join(RUNTIME_RECEIPT),
            managed_runtime().archive_sha512,
        )
        .unwrap();
        for name in ["wine", "wineserver", managed_runtime().loader_name] {
            fs::write(pinned.join(name), b"\x7fELF tampered").unwrap();
            fs::set_permissions(pinned.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let artifact = custom_runtime_artifact(&paths, &custom_runtime_archive("wine-custom"));

        let runtime = ensure_runtime(&paths, &artifact, &OperationControl::default()).unwrap();
        assert_eq!(runtime, InstalledRuntime::Custom);
        let handles = RuntimeHandles::open(&paths, runtime).unwrap();
        let pinned_inodes = ["wine", "wineserver", managed_runtime().loader_name]
            .map(|name| fs::metadata(pinned.join(name)).unwrap().ino());
        for file in [&handles.preloader, &handles.wine, &handles.wineserver] {
            assert!(
                !pinned_inodes.contains(&file.metadata().unwrap().ino()),
                "nothing from the tampered pinned runtime may be opened for execution"
            );
        }
        assert_eq!(
            handles.wine.metadata().unwrap().ino(),
            fs::metadata(fixture.path().join("runner/custom-wine/bin/wine"))
                .unwrap()
                .ino()
        );
    }

    fn custom_gecko_prefix_fixture(root: &Path) -> PathBuf {
        let prefix = root.join("prefix");
        fs::create_dir_all(&prefix).unwrap();
        for hive in ["system.reg", "user.reg", "userdef.reg"] {
            fs::write(prefix.join(hive), b"WINE REGISTRY").unwrap();
        }
        fs::write(
            prefix.join(CUSTOM_GECKO_RECEIPT),
            CustomSourceReceipt::new(
                DownloadArtifact::Gecko,
                "https://mirror.example/gecko.msi",
                &"a".repeat(64),
            )
            .encode(),
        )
        .unwrap();
        prefix
    }

    #[test]
    fn a_custom_gecko_prefix_counts_only_while_the_gecko_source_is_custom() {
        let (fixture, paths) = managed_paths_with_runtime();
        let prefix = custom_gecko_prefix_fixture(fixture.path());
        let prefix_directory = Directory::open(&prefix).unwrap();
        assert!(gecko_installed(&prefix_directory, true));
        assert!(!gecko_installed(&prefix_directory, false));

        // Custom Gecko still configured: nothing is downloaded or executed.
        let host = Arc::new(RecordingHost::default());
        let lease = fixture_lease(fixture.path());
        let mut custom = verified_test_artifact(ArtifactKind::Gecko, fixture.path().join("c.msi"));
        custom.custom_source = Some("https://mirror.example/gecko.msi".to_owned());
        ProductionInstallDriver::new(host.clone())
            .provision_runtime(
                &paths,
                None,
                PrefixArtifacts {
                    gecko: Some(&custom),
                    ..PrefixArtifacts::default()
                },
                &lease,
                &OperationControl::default(),
            )
            .unwrap();
        ProductionInstallDriver::new(host.clone())
            .provision_runtime(
                &paths,
                None,
                PrefixArtifacts::default(),
                &lease,
                &OperationControl::default(),
            )
            .unwrap();
        assert!(host.requests.lock().unwrap().is_empty());

        // Built-in Gecko configured: the verified MSI is reinstalled in place
        // through the accepted runtime; the prefix is never recreated.
        let gecko = verified_test_artifact(ArtifactKind::Gecko, fixture.path().join("g.msi"));
        let result = ProductionInstallDriver::new(host.clone()).provision_runtime(
            &paths,
            None,
            PrefixArtifacts {
                gecko: Some(&gecko),
                ..PrefixArtifacts::default()
            },
            &lease,
            &OperationControl::default(),
        );
        // The fixture MSI installs no pinned xul.dll, so the pinned receipt is
        // never written and the custom receipt is kept until it would be.
        assert!(result.is_err());
        let requests = host.requests.lock().unwrap();
        assert!(requests[0].arguments.contains(&OsString::from("msiexec")));
        assert!(
            requests
                .iter()
                .all(|request| !request.arguments.contains(&OsString::from("wineboot")))
        );
        assert!(prefix.join("system.reg").is_file());
        assert!(!prefix.join(GECKO_RECEIPT).exists());
        assert!(prefix.join(CUSTOM_GECKO_RECEIPT).exists());
    }

    #[test]
    fn only_a_prefix_without_hives_game_or_saves_is_made_again() {
        let fixture = tempfile::tempdir().unwrap();
        let prefix = fixture.path().join("prefix");
        fs::create_dir_all(prefix.join("drive_c/users/steamuser/Saved Games")).unwrap();
        let open = || Directory::open(&prefix).unwrap();
        // First wineboot stopped early: nothing to keep.
        assert!(abandoned_prefix(&open()));
        // A hive exists but can't be opened (a backup hard-linked it).
        fs::write(prefix.join("user.reg"), b"WINE REGISTRY").unwrap();
        fs::hard_link(
            prefix.join("user.reg"),
            fixture.path().join("user.reg.backup"),
        )
        .unwrap();
        assert!(!initialized_prefix(&open()));
        assert!(
            !abandoned_prefix(&open()),
            "a hard-linked hive is refused, never removed"
        );
        fs::remove_file(prefix.join("user.reg")).unwrap();
        // Saved characters are kept.
        fs::write(
            prefix.join("drive_c/users/steamuser/Saved Games/Hero.d2s"),
            b"save",
        )
        .unwrap();
        assert!(!abandoned_prefix(&open()));
        fs::remove_file(prefix.join("drive_c/users/steamuser/Saved Games/Hero.d2s")).unwrap();
        // So is the game.
        fs::create_dir_all(prefix.join("drive_c/Program Files (x86)/Diablo II")).unwrap();
        assert!(!abandoned_prefix(&open()));
    }

    /// A prefix an older release set up on Android: Wine's files, the game,
    /// the .NET Desktop Runtime, and Wine Gecko and Wine Mono with their
    /// receipts.
    fn older_android_prefix(root: &Path) -> PathBuf {
        let prefix = root.join("prefix");
        let write = |relative: &str| {
            let path = prefix.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"x").unwrap();
        };
        for receipt in [
            GECKO_RECEIPT,
            MONO_RECEIPT,
            DOTNET_DESKTOP_RECEIPT,
            "system.reg",
            "user.reg",
            "userdef.reg",
        ] {
            write(receipt);
        }
        write("drive_c/windows/mono/mono-2.0/lib/mono/4.5/mscorlib.dll");
        write("drive_c/windows/mono/mono-2.0/support/installinf-x86.exe");
        write("drive_c/windows/syswow64/gecko/2.47.4/wine_gecko/xul.dll");
        fs::create_dir_all(prefix.join("drive_c/windows/syswow64/gecko/plugin")).unwrap();
        fs::create_dir_all(prefix.join("drive_c/windows/system32/gecko/plugin")).unwrap();
        write("drive_c/windows/system32/kernel32.dll");
        write("drive_c/Program Files (x86)/Diablo II/Game.exe");
        write("drive_c/Program Files (x86)/Diablo II/ProjectD2/PD2Launcher.exe");
        write("drive_c/Program Files (x86)/dotnet/dotnet.exe");
        prefix
    }

    #[test]
    fn wine_gecko_and_mono_leave_an_android_prefix_and_nothing_else_does() {
        let fixture = tempfile::tempdir().unwrap();
        let prefix = older_android_prefix(fixture.path());
        // Wine maps a read-only attribute onto a folder's mode.
        fs::set_permissions(
            prefix.join("drive_c/windows/mono/mono-2.0/support"),
            fs::Permissions::from_mode(0o500),
        )
        .unwrap();
        let open = || Directory::open(&prefix).unwrap();
        assert!(remove_wine_gecko_and_mono(&open()).unwrap());
        for gone in [
            GECKO_RECEIPT,
            MONO_RECEIPT,
            "drive_c/windows/mono",
            "drive_c/windows/syswow64/gecko/2.47.4",
        ] {
            assert!(!prefix.join(gone).exists(), "{gone} is still there");
        }
        for kept in [
            DOTNET_DESKTOP_RECEIPT,
            "system.reg",
            "drive_c/windows/syswow64/gecko/plugin",
            "drive_c/windows/system32/gecko/plugin",
            "drive_c/windows/system32/kernel32.dll",
            "drive_c/Program Files (x86)/Diablo II/Game.exe",
            "drive_c/Program Files (x86)/Diablo II/ProjectD2/PD2Launcher.exe",
            "drive_c/Program Files (x86)/dotnet/dotnet.exe",
        ] {
            assert!(prefix.join(kept).exists(), "{kept} was removed");
        }
        // Again: nothing left to remove, and nothing else goes.
        assert!(!remove_wine_gecko_and_mono(&open()).unwrap());
        assert!(prefix.join(DOTNET_DESKTOP_RECEIPT).exists());
    }

    #[test]
    fn a_removal_of_wine_gecko_and_mono_that_stopped_part_way_is_finished_later() {
        let fixture = tempfile::tempdir().unwrap();
        let prefix = older_android_prefix(fixture.path());
        // The receipts went and half of Mono's folder, then the app stopped.
        fs::remove_file(prefix.join(GECKO_RECEIPT)).unwrap();
        fs::remove_file(prefix.join(MONO_RECEIPT)).unwrap();
        fs::remove_dir_all(prefix.join("drive_c/windows/mono/mono-2.0/lib")).unwrap();
        let open = || Directory::open(&prefix).unwrap();
        assert!(remove_wine_gecko_and_mono(&open()).unwrap());
        assert!(!prefix.join("drive_c/windows/mono").exists());
        assert!(
            !prefix
                .join("drive_c/windows/syswow64/gecko/2.47.4")
                .exists()
        );
        assert!(
            prefix
                .join("drive_c/Program Files (x86)/Diablo II/Game.exe")
                .exists()
        );
        assert!(!remove_wine_gecko_and_mono(&open()).unwrap());
    }

    #[test]
    fn a_linked_mono_folder_is_never_followed() {
        use std::os::unix::fs::symlink;
        let fixture = tempfile::tempdir().unwrap();
        let prefix = older_android_prefix(fixture.path());
        let outside = fixture.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep.txt"), b"keep").unwrap();
        fs::remove_dir_all(prefix.join("drive_c/windows/mono")).unwrap();
        symlink(&outside, prefix.join("drive_c/windows/mono")).unwrap();
        let prefix_directory = Directory::open(&prefix).unwrap();
        assert!(remove_wine_gecko_and_mono(&prefix_directory).is_err());
        assert!(outside.join("keep.txt").exists());
    }

    /// A folder outside the prefix with one file in it, for the link tests.
    fn outside_folder(root: &Path) -> PathBuf {
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep.txt"), b"keep").unwrap();
        outside
    }

    #[test]
    fn a_link_inside_the_mono_tree_goes_and_its_target_stays() {
        use std::os::unix::fs::symlink;
        let fixture = tempfile::tempdir().unwrap();
        let prefix = older_android_prefix(fixture.path());
        let outside = outside_folder(fixture.path());
        symlink(
            &outside,
            prefix.join("drive_c/windows/mono/mono-2.0/lib/linked"),
        )
        .unwrap();
        symlink(
            outside.join("keep.txt"),
            prefix.join("drive_c/windows/mono/mono-2.0/linked.txt"),
        )
        .unwrap();
        assert!(remove_wine_gecko_and_mono(&Directory::open(&prefix).unwrap()).unwrap());
        assert!(!prefix.join("drive_c/windows/mono").exists());
        assert_eq!(fs::read(outside.join("keep.txt")).unwrap(), b"keep");
    }

    #[test]
    fn a_linked_gecko_version_folder_is_never_followed() {
        use std::os::unix::fs::symlink;
        let fixture = tempfile::tempdir().unwrap();
        let prefix = older_android_prefix(fixture.path());
        let outside = outside_folder(fixture.path());
        let gecko = prefix.join("drive_c/windows/syswow64/gecko/2.47.4");
        fs::remove_dir_all(&gecko).unwrap();
        symlink(&outside, &gecko).unwrap();
        assert!(remove_wine_gecko_and_mono(&Directory::open(&prefix).unwrap()).is_err());
        assert_eq!(fs::read(outside.join("keep.txt")).unwrap(), b"keep");
    }

    #[test]
    fn a_linked_syswow64_or_drive_c_is_never_followed() {
        use std::os::unix::fs::symlink;
        for linked in ["drive_c/windows/syswow64", "drive_c"] {
            let fixture = tempfile::tempdir().unwrap();
            let prefix = older_android_prefix(fixture.path());
            let outside = fixture.path().join("outside");
            // The linked folder's contents, Gecko's version folder among them,
            // moved outside the prefix.
            fs::rename(prefix.join(linked), &outside).unwrap();
            symlink(&outside, prefix.join(linked)).unwrap();
            assert!(
                remove_wine_gecko_and_mono(&Directory::open(&prefix).unwrap()).is_err(),
                "{linked}"
            );
            let gecko = if linked == "drive_c" {
                outside.join("windows/syswow64/gecko/2.47.4/wine_gecko/xul.dll")
            } else {
                outside.join("gecko/2.47.4/wine_gecko/xul.dll")
            };
            assert!(gecko.exists(), "{linked}");
        }
    }

    #[test]
    fn a_linked_or_hard_linked_receipt_stops_the_removal_and_its_target_stays() {
        use std::os::unix::fs::symlink;
        // A symbolic link as Gecko's receipt.
        let fixture = tempfile::tempdir().unwrap();
        let prefix = older_android_prefix(fixture.path());
        let outside = outside_folder(fixture.path());
        fs::remove_file(prefix.join(GECKO_RECEIPT)).unwrap();
        symlink(outside.join("keep.txt"), prefix.join(GECKO_RECEIPT)).unwrap();
        assert!(remove_wine_gecko_and_mono(&Directory::open(&prefix).unwrap()).is_err());
        assert_eq!(fs::read(outside.join("keep.txt")).unwrap(), b"keep");
        // A hard link as Mono's receipt: not a unique file, so it is left, and
        // the other name keeps its contents.
        let fixture = tempfile::tempdir().unwrap();
        let prefix = older_android_prefix(fixture.path());
        let outside = outside_folder(fixture.path());
        fs::remove_file(prefix.join(MONO_RECEIPT)).unwrap();
        fs::hard_link(outside.join("keep.txt"), prefix.join(MONO_RECEIPT)).unwrap();
        assert!(remove_wine_gecko_and_mono(&Directory::open(&prefix).unwrap()).is_err());
        assert_eq!(fs::read(outside.join("keep.txt")).unwrap(), b"keep");
        assert!(prefix.join(MONO_RECEIPT).exists());
    }

    #[test]
    fn a_system32_gecko_version_folder_goes_too() {
        let fixture = tempfile::tempdir().unwrap();
        let prefix = older_android_prefix(fixture.path());
        let version = prefix.join("drive_c/windows/system32/gecko/2.47.4/wine_gecko");
        fs::create_dir_all(&version).unwrap();
        fs::write(version.join("xul.dll"), b"x").unwrap();
        assert!(remove_wine_gecko_and_mono(&Directory::open(&prefix).unwrap()).unwrap());
        assert!(
            !prefix
                .join("drive_c/windows/system32/gecko/2.47.4")
                .exists()
        );
        assert!(
            prefix
                .join("drive_c/windows/system32/gecko/plugin")
                .exists()
        );
    }

    #[test]
    fn box64_names_the_pd2_installer_by_the_name_it_runs_under() {
        let programs = crate::box64::settings(crate::box64::Run::Installer, true)
            .into_iter()
            .find(|setting| setting.name == "BOX64_DYNAREC_PROGRAMS")
            .map(|setting| setting.value.into_string().unwrap())
            .unwrap();
        let exe = ANDROID_CLIENT_INSTALLER.rsplit('\\').next().unwrap();
        assert_eq!(exe, ANDROID_CLIENT_INSTALLER_NAME);
        // The names, without the empty one after the list's last `;`.
        let names = programs
            .split(';')
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        let tmp = format!("{}.tmp", exe.trim_end_matches(".exe"));
        assert_eq!(names, [exe, tmp.as_str()]);
        assert!(programs.ends_with(';'));
        // It runs from the program copies, which the download watch never counts.
        assert_eq!(
            ANDROID_CLIENT_INSTALLER,
            format!("{WINDOWS_ANDROID_PROGRAM_COPIES}\\{ANDROID_CLIENT_INSTALLER_NAME}")
        );
    }

    #[test]
    fn an_android_ui_helper_runs_as_a_checked_copy_that_is_removed_after() {
        let fixture = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(fixture.path()).unwrap();
        let folder = paths.root().join(ANDROID_PROGRAM_COPIES);
        for helper in [UiHelper::Vendor, UiHelper::Downloader] {
            let sealed = helper.sealed_file().unwrap();
            let copy = folder.join(helper.file_name());
            let (program, guard) = android_helper_copy(&paths, helper, &sealed).unwrap();
            assert_eq!(
                program,
                OsString::from(format!(
                    "{WINDOWS_ANDROID_PROGRAM_COPIES}\\{}",
                    helper.file_name()
                ))
            );
            let mut written = File::open(&copy).unwrap();
            assert_eq!(sha256_open_file(&mut written).unwrap(), helper.sha256());
            // The download watch never sees it.
            assert_eq!(
                folder_state(&paths.root().join(WRAPPER_DOWNLOADS), 8),
                FolderState::default()
            );
            drop(guard);
            assert!(!copy.exists());
            assert!(!folder.exists());
        }
    }

    #[test]
    fn the_app_prefix_is_a_copy_of_its_template_with_links_and_the_dotnet_link() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        // No template: no prefix, and nothing half made.
        assert!(copy_app_prefix_template(root, crate::runtime::PrefixTemplate::Plain).is_err());
        assert!(!root.join("prefix").exists());
        let template = root.join(crate::runtime::APP_PREFIX_TEMPLATE);
        let system32 = template.join("drive_c/windows/system32");
        fs::create_dir_all(&system32).unwrap();
        for hive in ["system.reg", "user.reg", "userdef.reg"] {
            fs::write(template.join(hive), b"WINE REGISTRY").unwrap();
        }
        fs::write(template.join(".update-timestamp"), b"disable\n").unwrap();
        fs::write(system32.join("tool.exe"), b"MZ").unwrap();
        fs::set_permissions(system32.join("tool.exe"), fs::Permissions::from_mode(0o755)).unwrap();
        symlink(
            "../../../../runner/wine-11.0-arm64/lib/wine/aarch64-windows/kernel32.dll",
            system32.join("kernel32.dll"),
        )
        .unwrap();
        // A copy cut short before is replaced.
        fs::create_dir_all(root.join(".prefix.partial/stale")).unwrap();
        copy_app_prefix_template(root, crate::runtime::PrefixTemplate::Plain).unwrap();
        let prefix = root.join("prefix");
        assert!(!root.join(".prefix.partial").exists());
        assert_eq!(
            fs::read(prefix.join("system.reg")).unwrap(),
            b"WINE REGISTRY"
        );
        assert_eq!(
            fs::read(prefix.join(".update-timestamp")).unwrap(),
            b"disable\n"
        );
        let tool = prefix.join("drive_c/windows/system32/tool.exe");
        assert_eq!(
            fs::metadata(&tool).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::read_link(prefix.join("drive_c/windows/system32/kernel32.dll")).unwrap(),
            Path::new("../../../../runner/wine-11.0-arm64/lib/wine/aarch64-windows/kernel32.dll")
        );
        // The template stays for the next prefix.
        assert!(template.join("system.reg").is_file());
        // The .NET link, made once; a real folder at its place is kept.
        link_app_dotnet(root).unwrap();
        let link = root.join(crate::runtime::APP_DOTNET_LINK);
        assert_eq!(
            fs::read_link(&link).unwrap(),
            Path::new(crate::runtime::APP_DOTNET_LINK_TARGET)
        );
        let dotnet = root.join("runner/dotnet-6.0.36-x86/shared");
        fs::create_dir_all(&dotnet).unwrap();
        assert!(
            link.join("shared").is_dir(),
            "the link reaches the app's .NET"
        );
        link_app_dotnet(root).unwrap();
        fs::remove_file(&link).unwrap();
        fs::create_dir(&link).unwrap();
        link_app_dotnet(root).unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().is_dir());
    }

    #[test]
    fn a_dotnet_link_to_another_target_is_made_again_and_a_real_file_or_folder_is_kept() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let link = root.join(crate::runtime::APP_DOTNET_LINK);
        let current = Path::new(crate::runtime::APP_DOTNET_LINK_TARGET);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        // An earlier release's link to its own .NET, which is still there,
        // and one to a .NET that is gone.
        let earlier = root.join("runner/dotnet-6.0.30-x86/shared");
        fs::create_dir_all(&earlier).unwrap();
        for target in [
            "../../../runner/dotnet-6.0.30-x86",
            "../../../runner/dotnet-6.0.20-x86",
        ] {
            symlink(target, &link).unwrap();
            link_app_dotnet(root).unwrap();
            assert_eq!(fs::read_link(&link).unwrap(), current);
            fs::remove_file(&link).unwrap();
        }
        // Only the link went, never what it led to.
        assert!(earlier.is_dir());
        // The current link stays, also before the app's .NET is there.
        symlink(current, &link).unwrap();
        let made = fs::symlink_metadata(&link).unwrap().ino();
        link_app_dotnet(root).unwrap();
        assert_eq!(fs::symlink_metadata(&link).unwrap().ino(), made);
        fs::remove_file(&link).unwrap();
        // A real folder, with what it holds, and a real file are kept.
        fs::create_dir_all(link.join("shared")).unwrap();
        link_app_dotnet(root).unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().is_dir());
        assert!(link.join("shared").is_dir());
        fs::remove_dir_all(&link).unwrap();
        fs::write(&link, b"a file").unwrap();
        link_app_dotnet(root).unwrap();
        assert_eq!(fs::read(&link).unwrap(), b"a file");
    }

    /// The Android app's prefix template in the managed root `root`, as the
    /// app makes it with Wine: its hives (`user.reg` with `UseEGL` "N"), its
    /// update switch, and Wine's folders.
    fn app_prefix_template_fixture(root: &Path) -> PathBuf {
        let template = root.join(crate::runtime::APP_PREFIX_TEMPLATE);
        for folder in ["drive_c/windows/system32", "drive_c/windows/syswow64"] {
            fs::create_dir_all(template.join(folder)).unwrap();
        }
        fs::write(template.join("system.reg"), b"WINE REGISTRY Version 2\n").unwrap();
        fs::write(
            template.join("user.reg"),
            "WINE REGISTRY Version 2\n\n[Software\\\\Wine\\\\X11 Driver] 1\n\"UseEGL\"=\"N\"\n",
        )
        .unwrap();
        fs::write(template.join("userdef.reg"), b"WINE REGISTRY Version 2\n").unwrap();
        fs::write(template.join(".update-timestamp"), b"disable\n").unwrap();
        template
    }

    /// Gives `folders` the owner's write again, so the fixture can go.
    fn writable_again(folders: &[PathBuf]) {
        for folder in folders {
            let _ = fs::set_permissions(folder, fs::Permissions::from_mode(0o755));
        }
    }

    #[test]
    fn a_template_folder_without_the_owners_write_is_copied_with_its_mode() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let template = app_prefix_template_fixture(root);
        // Wine gives a folder with the read-only attribute such a mode.
        let windows = template.join("drive_c/windows");
        fs::write(windows.join("win.ini"), b"[windows]\n").unwrap();
        fs::set_permissions(&windows, fs::Permissions::from_mode(0o555)).unwrap();
        let copied = copy_app_prefix_template(root, crate::runtime::PrefixTemplate::Plain);
        let copy = root.join("prefix/drive_c/windows");
        let mode = fs::metadata(&copy).map(|metadata| metadata.permissions().mode() & 0o777);
        writable_again(&[
            windows.clone(),
            copy.clone(),
            root.join(".prefix.partial/drive_c/windows"),
        ]);
        copied.unwrap();
        // The folder has the template's mode, and every entry under it.
        assert_eq!(mode.unwrap(), 0o555);
        assert_eq!(fs::read(copy.join("win.ini")).unwrap(), b"[windows]\n");
        assert!(copy.join("system32").is_dir() && copy.join("syswow64").is_dir());
    }

    #[test]
    fn the_prefix_gets_its_name_only_after_the_whole_template_is_copied() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let template = app_prefix_template_fixture(root);
        // A template file the copy cannot read stops it part way.
        let unreadable = template.join("drive_c/windows/system32/unreadable.dll");
        fs::write(&unreadable, b"MZ").unwrap();
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
        if File::open(&unreadable).is_ok() {
            // Root reads every file: nothing to show.
            return;
        }
        assert!(copy_app_prefix_template(root, crate::runtime::PrefixTemplate::Plain).is_err());
        assert!(!root.join("prefix").exists(), "no prefix from a part");
        // The next copy goes to the end, and only then has the name.
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();
        copy_app_prefix_template(root, crate::runtime::PrefixTemplate::Plain).unwrap();
        assert!(!root.join(".prefix.partial").exists());
        let prefix = root.join("prefix");
        assert_eq!(
            fs::read(prefix.join("user.reg")).unwrap(),
            fs::read(template.join("user.reg")).unwrap()
        );
        assert_eq!(
            fs::read(prefix.join("drive_c/windows/system32/unreadable.dll")).unwrap(),
            b"MZ"
        );
    }

    #[test]
    fn a_stale_copy_whose_folder_the_owner_cannot_write_is_removed() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        app_prefix_template_fixture(root);
        // A crash after a copied folder got the template's read-only mode.
        let stale = root.join(".prefix.partial/drive_c/windows");
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("leftover.dll"), b"MZ").unwrap();
        fs::set_permissions(&stale, fs::Permissions::from_mode(0o555)).unwrap();
        let copied = copy_app_prefix_template(root, crate::runtime::PrefixTemplate::Plain);
        writable_again(&[stale]);
        copied.unwrap();
        assert!(!root.join(".prefix.partial").exists());
        assert!(!root.join("prefix/drive_c/windows/leftover.dll").exists());
        assert!(root.join("prefix/system.reg").is_file());
    }

    /// A managed root with the Android app's runtime and its prefix
    /// template, and its mutation lease.
    fn android_setup_fixture() -> (tempfile::TempDir, ManagedPaths, File) {
        let fixture = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(fixture.path()).unwrap();
        app_runtime_fixture(fixture.path());
        app_prefix_template_fixture(fixture.path());
        let lease = fixture_lease(fixture.path());
        (fixture, paths, lease)
    }

    /// A prefix kept from an earlier release (FEX 0.2.x): Wine's hives
    /// without `UseEGL`, and the game. Returns the game's `Game.exe`.
    fn kept_app_prefix(root: &Path) -> PathBuf {
        let prefix = initialized_prefix_fixture(root);
        let game = prefix.join("drive_c/Program Files (x86)/Diablo II/Game.exe");
        fs::create_dir_all(game.parent().unwrap()).unwrap();
        fs::write(&game, b"game").unwrap();
        game
    }

    #[test]
    fn an_abandoned_app_prefix_is_made_again_from_the_template() {
        let (fixture, paths, lease) = android_setup_fixture();
        let root = fixture.path();
        // Wine never finished it: no hive, no game and no saves.
        let leftover = root.join("prefix/drive_c/windows/leftover.dll");
        fs::create_dir_all(leftover.parent().unwrap()).unwrap();
        fs::write(&leftover, b"MZ").unwrap();
        let host = Arc::new(RecordingHost::default());
        let control = OperationControl::default();
        let parts = reported_parts(&control);
        ProductionInstallDriver::new(host.clone())
            .on_android_host(true)
            .provision_runtime(&paths, None, PrefixArtifacts::default(), &lease, &control)
            .unwrap();
        // The template's copy, with the XInput bridge and the .NET link.
        let template = root.join(crate::runtime::APP_PREFIX_TEMPLATE);
        assert!(!leftover.exists());
        for hive in ["system.reg", "user.reg", "userdef.reg"] {
            assert_eq!(
                fs::read(root.join("prefix").join(hive)).unwrap(),
                fs::read(template.join(hive)).unwrap()
            );
        }
        assert!(root.join(XINPUT_BRIDGE_PATH).is_file());
        assert_eq!(
            fs::read_link(root.join(crate::runtime::APP_DOTNET_LINK)).unwrap(),
            Path::new(crate::runtime::APP_DOTNET_LINK_TARGET)
        );
        // Wine ran for the WPF value alone: the copy has `UseEGL` "N", and
        // its "disable" keeps Wine's update away.
        let requests = host.requests.lock().unwrap();
        let wine = requests
            .iter()
            .filter(|request| request.executable == Path::new("/proc/self/fd/230"))
            .collect::<Vec<_>>();
        assert_eq!(wine.len(), 1);
        assert!(
            wine[0]
                .arguments
                .ends_with(&wpf_software_rendering_arguments(true))
        );
        let planned = [EnvironmentPart::Windows];
        assert_eq!(
            *parts.lock().unwrap(),
            [EnvironmentProgress::new(EnvironmentPart::Windows, &planned)]
        );
    }

    #[test]
    fn a_kept_app_prefix_stays_and_gets_glx_and_the_dotnet_link() {
        let (fixture, paths, lease) = android_setup_fixture();
        let root = fixture.path();
        let game = kept_app_prefix(root);
        let host = Arc::new(RecordingHost::default());
        let control = OperationControl::default();
        let parts = reported_parts(&control);
        ProductionInstallDriver::new(host.clone())
            .on_android_host(true)
            .provision_runtime(&paths, None, PrefixArtifacts::default(), &lease, &control)
            .unwrap();
        // Kept as it is: its own hives and the game, no copy of the template.
        assert_eq!(
            fs::read(root.join("prefix/system.reg")).unwrap(),
            b"WINE REGISTRY"
        );
        assert_eq!(fs::read(&game).unwrap(), b"game");
        assert!(!root.join(XINPUT_BRIDGE_PATH).exists());
        // Wine's update for the app's Wine (another Wine updated the prefix
        // last), as before any Wine program there, and then `reg add` for
        // `UseEGL` "N", which its user.reg lacks. No WPF value: only a new
        // prefix gets it here.
        let requests = host.requests.lock().unwrap();
        let wine = requests
            .iter()
            .filter(|request| request.executable == Path::new("/proc/self/fd/230"))
            .map(|request| request.arguments.clone())
            .collect::<Vec<_>>();
        let loader = loader_arguments(InstalledRuntime::App);
        assert_eq!(
            wine,
            [
                [loader.clone(), vec!["wineboot".into(), "-u".into()]].concat(),
                [loader, vec!["reg".into()], x11_glx_arguments()].concat(),
            ]
        );
        assert_eq!(
            fs::read_link(root.join(crate::runtime::APP_DOTNET_LINK)).unwrap(),
            Path::new(crate::runtime::APP_DOTNET_LINK_TARGET)
        );
        assert!(parts.lock().unwrap().is_empty());
    }

    /// The GOLDEN template beside the PLAIN one of `root`: the PLAIN copy
    /// with a file of the golden layer, and each template's mark.
    fn golden_template_fixture(root: &Path) {
        let plain = root.join(crate::runtime::APP_PREFIX_TEMPLATE);
        fs::write(plain.join(crate::runtime::PREFIX_TEMPLATE_MARK), b"plain\n").unwrap();
        let golden = root.join(crate::runtime::APP_PREFIX_TEMPLATE_GOLDEN);
        LegacyCopy::tree(&plain, &golden);
        fs::write(
            golden.join(crate::runtime::PREFIX_TEMPLATE_MARK),
            b"golden\n",
        )
        .unwrap();
        fs::write(
            golden.join("drive_c/windows/syswow64/mfc140u.dll"),
            b"MZ golden",
        )
        .unwrap();
    }

    /// A plain copy of a fixture's folder tree (files and folders).
    struct LegacyCopy;

    impl LegacyCopy {
        fn tree(from: &Path, to: &Path) {
            fs::create_dir_all(to).unwrap();
            for entry in fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let target = to.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() {
                    Self::tree(&entry.path(), &target);
                } else {
                    fs::copy(entry.path(), &target).unwrap();
                }
            }
        }
    }

    /// The Android host's managed root: the runtime, both templates, the
    /// layout with the game folder outside the prefix, and the lease.
    fn android_layout_fixture() -> (tempfile::TempDir, ManagedPaths, File) {
        let fixture = tempfile::tempdir().unwrap();
        app_runtime_fixture(fixture.path());
        app_prefix_template_fixture(fixture.path());
        golden_template_fixture(fixture.path());
        let paths = ManagedPaths::prepare_on(fixture.path(), true).unwrap();
        let lease = fixture_lease(fixture.path());
        (fixture, paths, lease)
    }

    fn template_mark(root: &Path) -> String {
        fs::read_to_string(
            root.join("prefix")
                .join(crate::runtime::PREFIX_TEMPLATE_MARK),
        )
        .unwrap()
        .trim()
        .to_owned()
    }

    #[test]
    fn on_android_a_new_prefix_is_a_copy_of_the_sources_template_with_the_game_link() {
        for template in [
            crate::runtime::PrefixTemplate::Plain,
            crate::runtime::PrefixTemplate::Golden,
        ] {
            let (fixture, paths, lease) = android_layout_fixture();
            let root = fixture.path();
            ProductionInstallDriver::new(Arc::new(RecordingHost::default()))
                .on_android_host(true)
                .provision_runtime(
                    &paths,
                    None,
                    PrefixArtifacts {
                        template,
                        ..PrefixArtifacts::default()
                    },
                    &lease,
                    &OperationControl::default(),
                )
                .unwrap();
            assert_eq!(template_mark(root), template.mark());
            assert_eq!(
                root.join("prefix/drive_c/windows/syswow64/mfc140u.dll")
                    .exists(),
                template == crate::runtime::PrefixTemplate::Golden
            );
            // The game folder outside the prefix, and the link to it.
            assert!(
                root.join(crate::game_folder::ANDROID_GAME_RELATIVE)
                    .is_dir()
            );
            assert_eq!(
                fs::read_link(root.join(crate::game_folder::GAME_RELATIVE)).unwrap(),
                Path::new(crate::game_folder::GAME_LINK_TARGET)
            );
        }
    }

    #[test]
    fn on_android_the_prefix_of_the_other_template_is_made_again_before_the_game() {
        let (fixture, paths, lease) = android_layout_fixture();
        let root = fixture.path();
        let golden = PrefixArtifacts {
            template: crate::runtime::PrefixTemplate::Golden,
            ..PrefixArtifacts::default()
        };
        let driver =
            ProductionInstallDriver::new(Arc::new(RecordingHost::default())).on_android_host(true);
        let control = OperationControl::default();
        // Blizzard's installers first made a PLAIN prefix; then the player
        // chose Fast download before any file of the game was there.
        driver
            .provision_runtime(&paths, None, PrefixArtifacts::default(), &lease, &control)
            .unwrap();
        assert_eq!(template_mark(root), "plain");
        driver
            .provision_runtime(&paths, None, golden, &lease, &control)
            .unwrap();
        assert_eq!(template_mark(root), "golden");
        // With a file of the game there, the prefix is kept.
        let game = root.join(crate::game_folder::ANDROID_GAME_RELATIVE);
        fs::write(game.join("Game.exe"), b"game").unwrap();
        driver
            .provision_runtime(&paths, None, PrefixArtifacts::default(), &lease, &control)
            .unwrap();
        assert_eq!(template_mark(root), "golden");
        assert_eq!(fs::read(game.join("Game.exe")).unwrap(), b"game");
    }

    #[test]
    fn on_android_an_import_moves_the_copy_into_the_game_folder_by_one_rename() {
        use std::os::unix::fs::MetadataExt;
        let (fixture, paths, lease) = android_layout_fixture();
        let root = fixture.path();
        let driver =
            ProductionInstallDriver::new(Arc::new(RecordingHost::default())).on_android_host(true);
        let control = OperationControl::default();
        driver
            .provision_runtime(
                &paths,
                None,
                PrefixArtifacts {
                    template: crate::runtime::PrefixTemplate::Golden,
                    ..PrefixArtifacts::default()
                },
                &lease,
                &control,
            )
            .unwrap();
        // The app's checked copy, below its own token folder outside the
        // managed root (on the same file system).
        let files = tempfile::tempdir_in(fixture.path().parent().unwrap()).unwrap();
        let copy = files.path().join("import/t1/Diablo II");
        crate::game_folder::tests::write_game_folder(&copy, b"game", b"archive");
        fs::write(copy.join("ProjectD2/loot.filter"), b"mine").unwrap();
        let inode = fs::metadata(&copy).unwrap().ino();
        let existing = crate::game_folder::validate_existing(&copy, root).unwrap();
        driver
            .import_existing(
                &paths,
                &existing,
                crate::game_folder::ImportMode::InPlace,
                &control,
            )
            .unwrap();
        let game = root.join(crate::game_folder::ANDROID_GAME_RELATIVE);
        // The same folder (a rename, no copy), with everything in it.
        assert_eq!(fs::metadata(&game).unwrap().ino(), inode);
        assert!(!copy.exists());
        assert_eq!(
            fs::read(game.join("ProjectD2/loot.filter")).unwrap(),
            b"mine"
        );
        assert_eq!(fs::read(game.join("Save/hero.d2s")).unwrap(), b"hero");
        // Through the link at C:\Program Files (x86)\Diablo II; no G: drive
        // and no record of a folder used in place.
        assert!(
            root.join(crate::game_folder::GAME_RELATIVE)
                .join("Game.exe")
                .is_file()
        );
        assert!(fs::symlink_metadata(root.join("prefix/dosdevices/g:")).is_err());
        assert!(
            !root
                .join("prefix")
                .join(crate::game_folder::POINTER_NAME)
                .exists()
        );
        // The import record is in the managed root, with the C: path.
        assert!(
            !root
                .join("prefix")
                .join(crate::game_folder::IMPORT_RECORD_NAME)
                .exists()
        );
        assert_eq!(
            fs::read_to_string(root.join(crate::game_folder::IMPORT_RECORD_NAME)).unwrap(),
            r#"{"schema":1,"mode":"in-place","install_path":"C:\\Program Files (x86)\\Diablo II"}"#
        );
        assert_eq!(
            crate::game_folder::GameFolder::read(paths.directory()),
            Ok(crate::game_folder::GameFolder::Managed)
        );
    }

    #[test]
    fn on_android_an_import_into_a_game_folder_with_files_moves_nothing() {
        let (fixture, paths, lease) = android_layout_fixture();
        let root = fixture.path();
        let driver =
            ProductionInstallDriver::new(Arc::new(RecordingHost::default())).on_android_host(true);
        let control = OperationControl::default();
        driver
            .provision_runtime(&paths, None, PrefixArtifacts::default(), &lease, &control)
            .unwrap();
        let game = root.join(crate::game_folder::ANDROID_GAME_RELATIVE);
        fs::write(game.join("Game.exe"), b"other game").unwrap();
        let files = tempfile::tempdir_in(fixture.path().parent().unwrap()).unwrap();
        let copy = files.path().join("import/t1/Diablo II");
        crate::game_folder::tests::write_game_folder(&copy, b"game", b"archive");
        let existing = crate::game_folder::validate_existing(&copy, root).unwrap();
        assert!(
            driver
                .import_existing(
                    &paths,
                    &existing,
                    crate::game_folder::ImportMode::InPlace,
                    &control,
                )
                .is_err()
        );
        assert_eq!(fs::read(copy.join("Game.exe")).unwrap(), b"game");
        assert_eq!(fs::read(game.join("Game.exe")).unwrap(), b"other game");
        assert!(!root.join(crate::game_folder::IMPORT_RECORD_NAME).exists());
    }

    #[test]
    fn on_android_the_official_launcher_starts_in_the_game_folder_through_the_link() {
        use std::os::unix::fs::MetadataExt;
        for android in [true, false] {
            let (fixture, _) = managed_paths_with_runtime();
            let root = fixture.path();
            app_runtime_fixture(root);
            initialized_prefix_fixture(root);
            fs::create_dir_all(root.join("prefix/drive_c/Program Files (x86)")).unwrap();
            let paths = ManagedPaths::prepare_on(root, android).unwrap();
            let project = paths.game().join("ProjectD2");
            fs::create_dir_all(&project).unwrap();
            fs::write(project.join(OFFICIAL_LAUNCHER), minimal_pe()).unwrap();
            let lease = fixture_lease(root);
            let host = Arc::new(RecordingHost::default());
            ProductionInstallDriver::new(host.clone())
                .on_android_host(android)
                .run_official_launcher(root, &lease, &OperationControl::default())
                .unwrap();
            let requests = host.requests.lock().unwrap();
            let launcher = requests
                .iter()
                .find(|request| {
                    request
                        .arguments
                        .ends_with(&[OsString::from(OFFICIAL_LAUNCHER)])
                })
                .unwrap();
            assert_eq!(
                launcher.working_directory.as_deref(),
                Some(project.as_path())
            );
            let pwd = environment_value(launcher, "PWD");
            if android {
                let pwd = Path::new(pwd.unwrap());
                assert_eq!(
                    pwd,
                    root.join("prefix/drive_c/Program Files (x86)/Diablo II/ProjectD2")
                );
                // The same folder as the working folder, through the link.
                let (by_pwd, real) = (fs::metadata(pwd).unwrap(), fs::metadata(&project).unwrap());
                assert_eq!((by_pwd.dev(), by_pwd.ino()), (real.dev(), real.ino()));
            } else {
                assert_eq!(pwd, None);
            }
        }
    }

    /// Records the arguments of every request. The first `reg` runs until
    /// the player's stop, which comes while it runs (`stop`), or exits with
    /// 1 (no `stop`).
    struct RegHost {
        stop: Option<OperationControl>,
        stopped: AtomicBool,
        requests: Mutex<Vec<Vec<OsString>>>,
    }

    impl RegHost {
        fn new(stop: Option<OperationControl>) -> Self {
            Self {
                stop,
                stopped: AtomicBool::new(false),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl HostExecution for RegHost {
        fn spawn(&self, request: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
            let arguments = request.arguments().to_vec();
            let reg = arguments.contains(&OsString::from("reg"));
            self.requests.lock().unwrap().push(arguments);
            if !reg || self.stopped.swap(true, Ordering::AcqRel) {
                return Ok(Box::new(CompleteProcess));
            }
            match &self.stop {
                Some(control) => {
                    control.request_cancel();
                    Ok(Box::new(CancelProcess {
                        state: Arc::default(),
                    }))
                }
                None => Ok(Box::new(FailedProcess)),
            }
        }
    }

    #[test]
    fn a_stop_during_reg_add_ends_the_setup_safely_and_the_next_one_finishes_the_prefix() {
        let (fixture, paths, lease) = android_setup_fixture();
        let root = fixture.path();
        let game = kept_app_prefix(root);
        let link = root.join(crate::runtime::APP_DOTNET_LINK);
        // The player's stop during the `reg add` for `UseEGL`: the setup
        // ends there, proven safe, and the prefix stays.
        let control = OperationControl::default();
        let host = Arc::new(RegHost::new(Some(control.clone())));
        let driver = ProductionInstallDriver::new(host.clone()).on_android_host(true);
        assert!(
            driver
                .provision_runtime(&paths, None, PrefixArtifacts::default(), &lease, &control)
                .is_err()
        );
        assert!(host.stopped.load(Ordering::Acquire));
        assert_eq!(
            control.cancellation_safety(),
            crate::operation::CancellationSafety::ProvenSafe
        );
        assert_eq!(fs::read(&game).unwrap(), b"game");
        assert!(fs::symlink_metadata(&link).is_err());
        // The next setup sets it and makes the link: nothing was lost.
        let host = Arc::new(RecordingHost::default());
        ProductionInstallDriver::new(host.clone())
            .on_android_host(true)
            .provision_runtime(
                &paths,
                None,
                PrefixArtifacts::default(),
                &lease,
                &OperationControl::default(),
            )
            .unwrap();
        assert!(
            host.requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.arguments.ends_with(&x11_glx_arguments()))
        );
        assert_eq!(
            fs::read_link(&link).unwrap(),
            Path::new(crate::runtime::APP_DOTNET_LINK_TARGET)
        );
        assert_eq!(fs::read(&game).unwrap(), b"game");
    }

    #[test]
    fn a_failed_reg_add_without_a_stop_does_not_fail_the_setup() {
        let (fixture, paths, lease) = android_setup_fixture();
        let root = fixture.path();
        let game = kept_app_prefix(root);
        let host = Arc::new(RegHost::new(None));
        // Without `UseEGL` "N" the game still draws, through EGL.
        ProductionInstallDriver::new(host.clone())
            .on_android_host(true)
            .provision_runtime(
                &paths,
                None,
                PrefixArtifacts::default(),
                &lease,
                &OperationControl::default(),
            )
            .unwrap();
        assert!(host.stopped.load(Ordering::Acquire));
        assert_eq!(
            fs::read_link(root.join(crate::runtime::APP_DOTNET_LINK)).unwrap(),
            Path::new(crate::runtime::APP_DOTNET_LINK_TARGET)
        );
        assert_eq!(fs::read(&game).unwrap(), b"game");
    }

    #[test]
    fn the_box64_settings_go_only_to_the_programs_they_name_and_never_on_the_desktop() {
        // A name this test's own environment sets is left out: on Android its
        // value reaches every Wine process.
        let own = env::vars_os()
            .map(|(name, _)| name)
            .collect::<BTreeSet<_>>();
        // The Box64 dynarec settings of a request, by name.
        let box64 = |request: &RequestSnapshot| {
            request
                .environment
                .iter()
                .filter(|(name, _)| {
                    name.to_string_lossy().starts_with("BOX64_DYNAREC_") && !own.contains(name)
                })
                .cloned()
                .collect::<BTreeMap<_, _>>()
        };
        // Those of a run on Android.
        let expected = |run: crate::box64::Run| {
            crate::box64::settings(run, true)
                .into_iter()
                .filter(|setting| !own.contains(&setting.name))
                .map(|setting| (setting.name, setting.value))
                .collect::<BTreeMap<_, _>>()
        };
        for (android, strict) in [(true, false), (true, true), (false, false), (false, true)] {
            let (fixture, paths) = managed_paths_with_runtime();
            app_runtime_fixture(fixture.path());
            initialized_prefix_fixture(fixture.path());
            // The game folder of the host's layout (on Android outside the prefix).
            let project = ManagedPaths::prepare_on(fixture.path(), android)
                .unwrap()
                .game()
                .join("ProjectD2");
            fs::create_dir_all(&project).unwrap();
            fs::write(project.join(OFFICIAL_LAUNCHER), minimal_pe()).unwrap();
            let lease = fixture_lease(fixture.path());
            let host = Arc::new(RecordingHost::default());
            let driver = ProductionInstallDriver::new(host.clone())
                .on_android_host(android)
                .with_game_strict_memory_order(strict);
            // PD2's installer, then the official launcher, each with the
            // other Wine runs around it (`reg`, Wine's update, `wineserver`).
            let bootstrap = verified_test_artifact(
                ArtifactKind::ClientBootstrap,
                fixture.path().join("ProjectD2Installer.exe"),
            );
            let _ = driver.install_client_bootstrap(
                &paths,
                &bootstrap,
                &lease,
                &OperationControl::default(),
            );
            driver
                .run_official_launcher(fixture.path(), &lease, &OperationControl::default())
                .unwrap();
            let requests = host.requests.lock().unwrap();
            let (mut installer, mut launcher) = (0, 0);
            for request in requests.iter() {
                let named = if request.arguments.contains(&OsString::from("/VERYSILENT")) {
                    installer += 1;
                    expected(crate::box64::Run::Installer)
                } else if request
                    .arguments
                    .ends_with(&[OsString::from(OFFICIAL_LAUNCHER)])
                {
                    launcher += 1;
                    expected(crate::box64::Run::OfficialLauncher {
                        game_strict_memory_order: strict,
                    })
                } else {
                    BTreeMap::new()
                };
                let want = if android { named } else { BTreeMap::new() };
                assert_eq!(
                    box64(request),
                    want,
                    "android {android}, strict {strict}: {:?}",
                    request.arguments
                );
            }
            assert_eq!(
                (installer, launcher),
                (1, 1),
                "android {android}, strict {strict}"
            );
            assert!(requests.len() > 2, "the other runs were seen");
        }
    }

    #[test]
    fn the_official_launcher_gets_detailed_logging_and_strict_memory_order_as_asked() {
        for (strict, detailed) in [(false, false), (false, true), (true, false), (true, true)] {
            let (fixture, _) = managed_paths_with_runtime();
            app_runtime_fixture(fixture.path());
            initialized_prefix_fixture(fixture.path());
            // The game folder of the host's layout (on Android outside the prefix).
            let project = ManagedPaths::prepare_on(fixture.path(), true)
                .unwrap()
                .game()
                .join("ProjectD2");
            fs::create_dir_all(&project).unwrap();
            fs::write(project.join(OFFICIAL_LAUNCHER), minimal_pe()).unwrap();
            let lease = fixture_lease(fixture.path());
            let host = Arc::new(RecordingHost::default());
            ProductionInstallDriver::new(host.clone())
                .on_android_host(true)
                .with_game_strict_memory_order(strict)
                .with_detailed_logging(detailed)
                .run_official_launcher(fixture.path(), &lease, &OperationControl::default())
                .unwrap();
            let requests = host.requests.lock().unwrap();
            let launcher = requests
                .iter()
                .find(|request| {
                    request
                        .arguments
                        .ends_with(&[OsString::from(OFFICIAL_LAUNCHER)])
                })
                .expect("the official launcher ran");
            let value = |name: &str| {
                environment_value(launcher, name).map(|value| value.to_string_lossy().into_owned())
            };
            let programs = value("BOX64_DYNAREC_PROGRAMS").unwrap_or_default();
            let game = programs
                .split(';')
                .find(|entry| entry.split(':').next() == Some("Game.exe"))
                .unwrap_or_default();
            assert_eq!(
                game.split(':').any(|setting| setting == "STRONGMEM"),
                strict,
                "strict {strict}, detailed {detailed}: {programs}"
            );
            let winedebug = value("WINEDEBUG").unwrap_or_default();
            assert_eq!(
                winedebug.contains("+winsock"),
                detailed,
                "strict {strict}, detailed {detailed}: {winedebug}"
            );
            assert_eq!(
                value("BOX64_LOG").as_deref() == Some("1"),
                detailed,
                "strict {strict}, detailed {detailed}"
            );
        }
    }

    #[test]
    fn an_android_program_copy_that_does_not_match_is_refused_and_removed() {
        let fixture = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(fixture.path()).unwrap();
        let mut sealed = UiHelper::Vendor.sealed_file().unwrap();
        let result = android_program_copy(
            paths.directory(),
            paths.root(),
            &mut sealed,
            "copy.exe",
            UiHelper::Downloader.sha256(),
        );
        assert!(result.is_err());
        assert!(
            !paths
                .root()
                .join(ANDROID_PROGRAM_COPIES)
                .join("copy.exe")
                .exists()
        );
    }

    fn initialized_prefix_fixture(root: &Path) -> PathBuf {
        let prefix = root.join("prefix");
        fs::create_dir_all(&prefix).unwrap();
        for hive in ["system.reg", "user.reg", "userdef.reg"] {
            fs::write(prefix.join(hive), b"WINE REGISTRY").unwrap();
        }
        prefix
    }

    /// Stands in for msiexec and the .NET installer by creating the tree
    /// each leaves in the prefix.
    struct DependencyInstallingHost {
        prefix: PathBuf,
        requests: Mutex<Vec<RequestSnapshot>>,
    }

    impl HostExecution for DependencyInstallingHost {
        fn spawn(&self, request: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
            let arguments = request.arguments().to_vec();
            if arguments.contains(&OsString::from("msiexec")) {
                fs::create_dir_all(self.prefix.join("drive_c/windows/mono/mono-2.0"))?;
            }
            if arguments.contains(&OsString::from("/install")) {
                fs::create_dir_all(self.prefix.join(
                    "drive_c/Program Files (x86)/dotnet/shared/Microsoft.WindowsDesktop.App/6.0.40",
                ))?;
            }
            self.requests.lock().unwrap().push(RequestSnapshot {
                executable: request.executable().to_owned(),
                guest: request.is_guest(),
                arguments,
                working_directory: request.working_directory().map(Path::to_owned),
                clears_environment: request.clears_environment(),
                environment: request.environment().to_vec(),
                output_limit: request.output_limit(),
                inherited_descriptors: request.inherited_file_descriptors().collect(),
                rendered_debug: format!("{request:?}"),
            });
            Ok(Box::new(CompleteProcess))
        }
    }

    fn environment_value<'a>(request: &'a RequestSnapshot, name: &str) -> Option<&'a OsStr> {
        request
            .environment
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_os_str())
    }

    #[test]
    fn the_android_gl_probe_runs_as_a_checked_copy_that_is_removed_after() {
        let fixture = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(fixture.path()).unwrap();
        let probe = gl_probe_helper().unwrap();
        let copy = paths
            .root()
            .join(ANDROID_PROGRAM_COPIES)
            .join("pd2launcher-gl-probe.exe");
        let (program, guard) =
            android_gl_probe_copy(paths.directory(), paths.root(), &probe).unwrap();
        assert_eq!(
            program,
            OsString::from(format!(
                "{WINDOWS_ANDROID_PROGRAM_COPIES}\\pd2launcher-gl-probe.exe"
            ))
        );
        let mut written = File::open(&copy).unwrap();
        assert_eq!(
            sha256_open_file(&mut written).unwrap(),
            GL_PROBE_HELPER_SHA256
        );
        drop(guard);
        assert!(!copy.exists());
    }

    #[test]
    fn a_prefix_last_updated_by_another_wine_is_updated_without_prompts_first() {
        let (fixture, paths) = managed_paths_with_runtime();
        let prefix = initialized_prefix_fixture(fixture.path());
        fs::create_dir_all(paths.game()).unwrap();
        let share = paths.runtime().join("share/wine");
        fs::create_dir_all(&share).unwrap();
        fs::write(share.join("wine.inf"), b"[version]").unwrap();
        let inf_mtime = fs::metadata(share.join("wine.inf")).unwrap().mtime();
        let bootstrap = verified_test_artifact(
            ArtifactKind::ClientBootstrap,
            fixture.path().join("bootstrap.exe"),
        );
        let lease = fixture_lease(fixture.path());
        let run = |timestamp: Option<i64>| {
            match timestamp {
                Some(value) => {
                    fs::write(prefix.join(".update-timestamp"), format!("{value}\n")).unwrap()
                }
                None => {
                    let _ = fs::remove_file(prefix.join(".update-timestamp"));
                }
            }
            let host = Arc::new(RecordingHost::default());
            let _ = ProductionInstallDriver::new(host.clone()).install_client_bootstrap(
                &paths,
                &bootstrap,
                &lease,
                &OperationControl::default(),
            );
            std::mem::take(&mut *host.requests.lock().unwrap())
        };
        // Another Wine build updated it last: the update runs first, with
        // Mono and Gecko disabled, before the stage's own program.
        for stale in [Some(inf_mtime - 1), None] {
            let requests = run(stale);
            assert!(requests[0].arguments.contains(&OsString::from("wineboot")));
            assert!(requests[0].arguments.contains(&OsString::from("-u")));
            assert_eq!(
                environment_value(&requests[0], "WINEDLLOVERRIDES"),
                Some(OsStr::new(PREFIX_INITIALIZATION_DLL_OVERRIDES))
            );
            assert!(
                requests
                    .iter()
                    .skip(1)
                    .any(|request| request.arguments.contains(&OsString::from("/VERYSILENT")))
            );
        }
        // Already updated by this Wine: nothing extra runs.
        let requests = run(Some(inf_mtime));
        assert!(
            requests
                .iter()
                .all(|request| !request.arguments.contains(&OsString::from("wineboot")))
        );
    }

    #[test]
    fn a_disabled_prefix_update_holds_only_on_the_app_runtime() {
        let (fixture, paths) = managed_paths_with_runtime();
        let prefix = initialized_prefix_fixture(fixture.path());
        app_runtime_fixture(fixture.path());
        let share = paths.runtime().join("share/wine");
        fs::create_dir_all(&share).unwrap();
        fs::write(share.join("wine.inf"), b"[version]").unwrap();
        let lease = fixture_lease(fixture.path());
        // Whether `wineboot -u` ran before the next Wine program, with
        // `timestamp` in the prefix's `.update-timestamp`.
        let updated = |runtime: InstalledRuntime, timestamp: &str| {
            fs::write(prefix.join(".update-timestamp"), timestamp).unwrap();
            let host = Arc::new(RecordingHost::default());
            ProductionInstallDriver::new(host.clone())
                .update_prefix_for_runtime(&paths, runtime, &lease, &OperationControl::default())
                .unwrap();
            let wineboot = [OsString::from("wineboot"), OsString::from("-u")];
            host.requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.arguments.ends_with(&wineboot))
        };
        // The Android app's prefix template: its runtime never updates it,
        // although it would update a prefix another Wine updated last.
        assert!(!updated(InstalledRuntime::App, "disable\n"));
        assert!(updated(InstalledRuntime::App, "1\n"));
        // The desktop's runtime updates the prefix whatever the file says.
        let desktop = InstalledRuntime::BuiltIn(managed_runtime());
        assert!(updated(desktop, "disable\n"));
        assert!(updated(desktop, "1\n"));
    }

    #[test]
    fn the_dotnet_installer_shows_its_progress_window_only_on_android() {
        let arguments = |android| -> Vec<String> {
            dotnet_desktop_arguments(android)
                .into_iter()
                .map(|argument| argument.into_string().unwrap())
                .collect()
        };
        assert_eq!(arguments(true), ["/install", "/passive", "/norestart"]);
        assert_eq!(arguments(false), ["/install", "/quiet", "/norestart"]);
    }

    #[test]
    fn a_dotnet_install_that_asks_for_a_restart_counts_only_with_its_folder() {
        struct ExitStatusProcess(i32);
        impl SupervisedProcess for ExitStatusProcess {
            fn try_wait(&mut self) -> io::Result<Option<ProcessExit>> {
                Ok(Some(ProcessExit::new(self.0 == 0, Some(self.0))))
            }
            fn terminate(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        /// The .NET installer exits with `status` and, when `creates`,
        /// leaves the pinned runtime folder; everything else succeeds.
        struct DotnetHost {
            prefix: PathBuf,
            status: i32,
            creates: bool,
        }
        impl HostExecution for DotnetHost {
            fn spawn(&self, request: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
                if request.arguments().contains(&OsString::from("/install")) {
                    if self.creates {
                        fs::create_dir_all(self.prefix.join(
                            "drive_c/Program Files (x86)/dotnet/shared/Microsoft.WindowsDesktop.App/6.0.36",
                        ))?;
                    }
                    return Ok(Box::new(ExitStatusProcess(self.status)));
                }
                Ok(Box::new(CompleteProcess))
            }
        }
        for (status, creates, installed) in [
            (DOTNET_RESTART_REQUIRED_STATUS, true, true),
            (DOTNET_RESTART_REQUIRED_STATUS, false, false),
            (1, true, false),
            (0, true, true),
        ] {
            let (fixture, paths) = managed_paths_with_runtime();
            let prefix = initialized_prefix_fixture(fixture.path());
            let mut dotnet = verified_test_artifact(
                ArtifactKind::DotnetDesktop,
                fixture.path().join("windowsdesktop-runtime.exe"),
            );
            // A fixture cannot hold the pinned installer's bytes, so the
            // source is custom; the exit policy is the same for both.
            dotnet.custom_source = Some("https://mirror.example/dotnet.exe".to_owned());
            let host = Arc::new(DotnetHost {
                prefix: prefix.clone(),
                status,
                creates,
            });
            let lease = fixture_lease(fixture.path());
            let result = ProductionInstallDriver::new(host).install_dotnet_desktop(
                &paths,
                InstalledRuntime::BuiltIn(managed_runtime()),
                &dotnet,
                &lease,
                &OperationControl::default(),
            );
            assert_eq!(
                result.is_ok(),
                installed,
                "status {status}, folder {creates}"
            );
            assert_eq!(
                prefix.join(CUSTOM_DOTNET_DESKTOP_RECEIPT).exists(),
                installed
            );
        }
    }

    #[test]
    fn an_existing_prefix_gets_wine_mono_and_dotnet_in_place_without_wineboot() {
        let (fixture, paths) = managed_paths_with_runtime();
        let prefix = initialized_prefix_fixture(fixture.path());
        let host = Arc::new(DependencyInstallingHost {
            prefix: prefix.clone(),
            requests: Mutex::new(Vec::new()),
        });
        let lease = fixture_lease(fixture.path());
        let mut mono = verified_test_artifact(ArtifactKind::WineMono, fixture.path().join("m.msi"));
        mono.custom_source = Some("https://mirror.example/mono.msi?token=secret".to_owned());
        let mut dotnet = verified_test_artifact(
            ArtifactKind::DotnetDesktop,
            fixture.path().join("dotnet.exe"),
        );
        dotnet.custom_source = Some("https://mirror.example/dotnet.exe".to_owned());

        let control = OperationControl::default();
        let parts = reported_parts(&control);
        ProductionInstallDriver::new(host.clone())
            .provision_runtime(
                &paths,
                None,
                PrefixArtifacts {
                    gecko: None,
                    mono: Some(&mono),
                    dotnet: Some(&dotnet),
                    ..PrefixArtifacts::default()
                },
                &lease,
                &control,
            )
            .unwrap();
        // Only the parts it installs, each as it starts.
        let planned = [EnvironmentPart::Dotnet, EnvironmentPart::LauncherSupport];
        assert_eq!(
            *parts.lock().unwrap(),
            [
                EnvironmentProgress::new(EnvironmentPart::Dotnet, &planned),
                EnvironmentProgress::new(EnvironmentPart::LauncherSupport, &planned),
            ]
        );

        let requests = host.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests[0].arguments,
            [
                runtime_loader_arguments(),
                ["msiexec", "/i", "/proc/self/fd/233", "/qn"]
                    .map(OsString::from)
                    .to_vec(),
            ]
            .concat()
        );
        assert_eq!(
            requests[0].inherited_descriptors,
            [235, 230, 231, 236, 234, 233]
        );
        assert_wineserver_wait_profile(&requests[1], &paths);
        assert_eq!(
            requests[2].arguments,
            [
                runtime_loader_arguments(),
                ["/proc/self/fd/233", "/install", "/quiet", "/norestart"]
                    .map(OsString::from)
                    .to_vec(),
            ]
            .concat()
        );
        assert_wineserver_wait_profile(&requests[3], &paths);
        for request in [&requests[0], &requests[2]] {
            // Installing into an existing prefix never disables Mono or
            // Gecko and never re-creates the prefix.
            assert_eq!(
                environment_value(request, "WINEDLLOVERRIDES"),
                Some(OsStr::new(WINE_DLL_OVERRIDES))
            );
            assert_eq!(environment_value(request, "WINEARCH"), None);
            assert!(!request.arguments.contains(&OsString::from("wineboot")));
        }
        let mono_receipt = fs::read_to_string(prefix.join(CUSTOM_MONO_RECEIPT)).unwrap();
        assert!(mono_receipt.contains("custom-unverified"));
        assert!(!mono_receipt.contains("secret"));
        assert!(prefix.join(CUSTOM_DOTNET_DESKTOP_RECEIPT).is_file());
        assert!(!prefix.join(MONO_RECEIPT).exists());
        assert!(!prefix.join(DOTNET_DESKTOP_RECEIPT).exists());
        let prefix_directory = Directory::open(&prefix).unwrap();
        assert!(PrefixDependency::WineMono.installed(&prefix_directory, true));
        assert!(!PrefixDependency::WineMono.installed(&prefix_directory, false));
        assert!(PrefixDependency::DotnetDesktop.installed(&prefix_directory, true));
        drop(requests);

        // Accepted components are not installed again.
        ProductionInstallDriver::new(host.clone())
            .provision_runtime(
                &paths,
                None,
                PrefixArtifacts {
                    gecko: None,
                    mono: Some(&mono),
                    dotnet: Some(&dotnet),
                    ..PrefixArtifacts::default()
                },
                &lease,
                &OperationControl::default(),
            )
            .unwrap();
        assert_eq!(host.requests.lock().unwrap().len(), 4);
    }

    #[test]
    fn prefix_creation_disables_the_mono_and_gecko_prompts_and_needs_every_component() {
        let (fixture, paths) = managed_paths_with_runtime();
        let host = Arc::new(RecordingHost::default());
        let lease = fixture_lease(fixture.path());
        ProductionInstallDriver::new(host.clone())
            .run_wine(WineInvocation {
                paths: &paths,
                runtime: InstalledRuntime::BuiltIn(managed_runtime()),
                working_directory: paths.root(),
                program: WineProgram::Builtin(OsString::from("wineboot")),
                verified_argument: None,
                arguments: vec![OsString::from("-u")],
                profile: WineProfile::PrefixInitialization,
                timeout: RUNTIME_TIMEOUT,
                mutation_lease: &lease,
                control: &OperationControl::default(),
                ui_helper: None,
                stall: None,
                accepted_exit_codes: &[],
            })
            .unwrap();
        let requests = host.requests.lock().unwrap();
        assert_eq!(
            environment_value(&requests[0], "WINEARCH"),
            Some(OsStr::new("win64"))
        );
        assert_eq!(
            environment_value(&requests[0], "WINEDLLOVERRIDES"),
            Some(OsStr::new(PREFIX_INITIALIZATION_DLL_OVERRIDES))
        );
        drop(requests);

        // No prefix yet: without every component, nothing is started. On
        // the desktop that includes the .NET Desktop Runtime.
        let gecko = verified_test_artifact(ArtifactKind::Gecko, fixture.path().join("g.msi"));
        let mono = verified_test_artifact(ArtifactKind::WineMono, fixture.path().join("m.msi"));
        for artifacts in [
            PrefixArtifacts {
                gecko: Some(&gecko),
                ..PrefixArtifacts::default()
            },
            PrefixArtifacts {
                gecko: Some(&gecko),
                mono: Some(&mono),
                dotnet: None,
                ..PrefixArtifacts::default()
            },
        ] {
            let host = Arc::new(RecordingHost::default());
            assert!(
                ProductionInstallDriver::new(host.clone())
                    .provision_runtime(
                        &paths,
                        None,
                        artifacts,
                        &lease,
                        &OperationControl::default(),
                    )
                    .is_err()
            );
            assert!(host.requests.lock().unwrap().is_empty());
            assert!(!paths.prefix().exists());
        }
    }

    #[test]
    fn a_new_desktop_prefix_needs_all_three_and_an_android_one_no_gecko_or_mono() {
        let fixture = tempfile::tempdir().unwrap();
        let gecko = verified_test_artifact(ArtifactKind::Gecko, fixture.path().join("g.msi"));
        let mono = verified_test_artifact(ArtifactKind::WineMono, fixture.path().join("m.msi"));
        let dotnet = verified_test_artifact(
            ArtifactKind::DotnetDesktop,
            fixture.path().join("dotnet.exe"),
        );
        // The desktop: Gecko, Wine Mono and the .NET Desktop Runtime.
        assert!(matches!(
            new_prefix_components(Some(&gecko), Some(&mono), Some(&dotnet), false),
            Some((Some(_), Some(_), Some(_)))
        ));
        assert!(new_prefix_components(None, Some(&mono), Some(&dotnet), false).is_none());
        assert!(new_prefix_components(Some(&gecko), None, Some(&dotnet), false).is_none());
        assert!(new_prefix_components(Some(&gecko), Some(&mono), None, false).is_none());
        // The Android host: the .NET Desktop Runtime when the audit asks for
        // it, and never Gecko or Wine Mono, even when they are given.
        assert!(matches!(
            new_prefix_components(None, None, Some(&dotnet), true),
            Some((None, None, Some(_)))
        ));
        assert!(matches!(
            new_prefix_components(Some(&gecko), Some(&mono), Some(&dotnet), true),
            Some((None, None, Some(_)))
        ));
        assert!(matches!(
            new_prefix_components(None, None, None, true),
            Some((None, None, None))
        ));
    }

    #[test]
    fn only_the_desktop_makes_a_new_prefix_with_wineboot_u() {
        assert_eq!(new_prefix_wineboot_arguments(false), [OsString::from("-u")]);
        assert!(new_prefix_wineboot_arguments(true).is_empty());
    }

    /// The game environment's parts `control` reports as each starts.
    fn reported_parts(control: &OperationControl) -> Arc<Mutex<Vec<EnvironmentProgress>>> {
        let parts = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&parts);
        control.observe(Arc::new(move |progress: WorkProgress| {
            if let Some(environment) = progress.environment() {
                assert_eq!(progress.phase(), WorkPhase::PrefixInitialization);
                seen.lock().unwrap().push(environment);
            }
        }));
        parts
    }

    #[test]
    fn a_new_desktop_prefix_is_made_by_wineboot_u_and_each_part_is_reported() {
        let (fixture, paths) = managed_paths_with_runtime();
        let lease = fixture_lease(fixture.path());
        let gecko = verified_test_artifact(ArtifactKind::Gecko, fixture.path().join("g.msi"));
        let mono = verified_test_artifact(ArtifactKind::WineMono, fixture.path().join("m.msi"));
        let dotnet = verified_test_artifact(
            ArtifactKind::DotnetDesktop,
            fixture.path().join("dotnet.exe"),
        );
        let host = Arc::new(RecordingHost::default());
        let control = OperationControl::default();
        let parts = reported_parts(&control);
        // The recording host creates no prefix, so provisioning stops after
        // Gecko; only the parts up to there are observed.
        let _ = ProductionInstallDriver::new(host.clone()).provision_runtime(
            &paths,
            None,
            PrefixArtifacts {
                gecko: Some(&gecko),
                mono: Some(&mono),
                dotnet: Some(&dotnet),
                ..PrefixArtifacts::default()
            },
            &lease,
            &control,
        );
        // The desktop makes it with `wineboot -u` (the Android host with
        // Wine's first start alone).
        let requests = host.requests.lock().unwrap();
        assert_eq!(
            requests[0].arguments,
            [
                runtime_loader_arguments(),
                vec![OsString::from("wineboot"), OsString::from("-u")]
            ]
            .concat()
        );
        assert_eq!(
            environment_value(&requests[0], "WINEDLLOVERRIDES"),
            Some(OsStr::new(PREFIX_INITIALIZATION_DLL_OVERRIDES))
        );
        let all = EnvironmentPart::ALL;
        assert_eq!(
            *parts.lock().unwrap(),
            [
                EnvironmentProgress::new(EnvironmentPart::Windows, &all),
                EnvironmentProgress::new(EnvironmentPart::WebComponents, &all),
            ]
        );
    }

    #[test]
    fn a_new_prefix_gets_wpf_software_rendering_before_gecko_only_when_the_setting_is_on() {
        for enabled in [true, false] {
            let (fixture, paths) = managed_paths_with_runtime();
            let lease = fixture_lease(fixture.path());
            let gecko = verified_test_artifact(ArtifactKind::Gecko, fixture.path().join("g.msi"));
            let mono = verified_test_artifact(ArtifactKind::WineMono, fixture.path().join("m.msi"));
            let dotnet = verified_test_artifact(
                ArtifactKind::DotnetDesktop,
                fixture.path().join("dotnet.exe"),
            );
            let host = Arc::new(RecordingHost::default());
            // The recording host creates no prefix, so provisioning stops
            // after Gecko; only the order up to there is observed.
            let _ = ProductionInstallDriver::new(host.clone())
                .with_wpf_software_rendering(enabled)
                .provision_runtime(
                    &paths,
                    None,
                    PrefixArtifacts {
                        gecko: Some(&gecko),
                        mono: Some(&mono),
                        dotnet: Some(&dotnet),
                        ..PrefixArtifacts::default()
                    },
                    &lease,
                    &OperationControl::default(),
                );
            let requests = host.requests.lock().unwrap();
            let position = |argument: &str| {
                requests
                    .iter()
                    .position(|request| request.arguments.iter().any(|value| value == argument))
            };
            let wineboot = position("wineboot").expect("prefix created");
            let gecko = position("msiexec").expect("gecko installed");
            match position("DisableHWAcceleration") {
                Some(wpf) if enabled => {
                    assert!(wineboot < wpf && wpf < gecko);
                    assert!(
                        requests[wpf]
                            .arguments
                            .ends_with(&wpf_software_rendering_arguments(true))
                    );
                }
                None if !enabled => {}
                other => panic!("software rendering {enabled}: WPF request at {other:?}"),
            }
        }
    }

    #[test]
    fn a_prefix_gets_glx_once_and_a_user_reg_that_has_it_is_left_alone() {
        let (fixture, paths) = managed_paths_with_runtime();
        let lease = fixture_lease(fixture.path());
        let prefix = fixture.path().join("prefix");
        fs::create_dir_all(&prefix).unwrap();
        // A prefix kept from FEX 0.2.x: an X11 driver section without it.
        fs::write(
            prefix.join("user.reg"),
            "WINE REGISTRY Version 2\n\n[Software\\\\Wine\\\\X11 Driver] 1792400000\n\"Decorated\"=\"N\"\n",
        )
        .unwrap();
        let host = Arc::new(RecordingHost::default());
        let driver = ProductionInstallDriver::new(host.clone());
        let runtime = driver.installed_runtime(&paths);
        driver
            .sync_x11_glx(&paths, runtime, &lease, &OperationControl::default())
            .unwrap();
        let ran = {
            let requests = host.requests.lock().unwrap();
            let reg = requests
                .iter()
                .filter(|request| request.arguments.ends_with(&x11_glx_arguments()))
                .count();
            assert_eq!(reg, 1);
            requests.len()
        };
        // Once it is there (Wine wrote it, or the template had it): no Wine.
        fs::write(
            prefix.join("user.reg"),
            "WINE REGISTRY Version 2\n\n[Software\\\\Wine\\\\X11 Driver] 1792400000\n\"UseEGL\"=\"N\"\n",
        )
        .unwrap();
        driver
            .sync_x11_glx(&paths, runtime, &lease, &OperationControl::default())
            .unwrap();
        assert_eq!(host.requests.lock().unwrap().len(), ran);
    }

    #[test]
    fn glx_is_seen_only_in_the_x11_driver_section() {
        assert_eq!(
            x11_glx_arguments(),
            [
                "add",
                r"HKCU\Software\Wine\X11 Driver",
                "/v",
                "UseEGL",
                "/t",
                "REG_SZ",
                "/d",
                "N",
                "/f"
            ]
            .map(OsString::from)
        );
        let section = "[Software\\\\Wine\\\\X11 Driver] 1792400000\n#time=1dc5f\n";
        assert!(x11_glx_set(&format!("{section}\"UseEGL\"=\"N\"\n")));
        // Key names in any case; a line end of Windows' kind.
        assert!(x11_glx_set(
            "[software\\\\wine\\\\x11 driver] 1\r\n\"UseEGL\"=\"N\"\r\n"
        ));
        assert!(!x11_glx_set(""));
        assert!(!x11_glx_set(&format!("{section}\"UseEGL\"=\"Y\"\n")));
        // The same value under another key is not it.
        assert!(!x11_glx_set(
            "[Software\\\\Wine\\\\AppDefaults\\\\x.exe\\\\X11 Driver] 1\n\"UseEGL\"=\"N\"\n"
        ));
        assert!(!x11_glx_set(&format!(
            "{section}\n[Software\\\\Wine\\\\Direct3D] 1\n\"UseEGL\"=\"N\"\n"
        )));
    }

    #[test]
    fn builtin_dependency_receipts_need_the_pinned_installer_and_its_tree() {
        let fixture = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(fixture.path()).unwrap();
        let prefix = initialized_prefix_fixture(fixture.path());
        let mut dotnet = verified_test_artifact(
            ArtifactKind::DotnetDesktop,
            fixture.path().join("dotnet.exe"),
        );
        // The pinned version's tree is missing.
        dotnet.observed_sha256 = crate::audit::DOTNET_DESKTOP_INSTALLER_SHA256.to_owned();
        assert!(
            record_dependency_provenance(&paths, PrefixDependency::DotnetDesktop, &dotnet).is_err()
        );
        fs::create_dir_all(
            prefix.join(
                "drive_c/Program Files (x86)/dotnet/shared/Microsoft.WindowsDesktop.App/6.0.36",
            ),
        )
        .unwrap();
        fs::write(prefix.join(CUSTOM_DOTNET_DESKTOP_RECEIPT), b"stale").unwrap();
        record_dependency_provenance(&paths, PrefixDependency::DotnetDesktop, &dotnet).unwrap();
        assert_eq!(
            fs::read_to_string(prefix.join(DOTNET_DESKTOP_RECEIPT)).unwrap(),
            crate::audit::DOTNET_DESKTOP_INSTALLER_SHA256
        );
        assert!(!prefix.join(CUSTOM_DOTNET_DESKTOP_RECEIPT).exists());

        // A built-in receipt is never written for bytes that are not pinned.
        fs::create_dir_all(prefix.join("drive_c/windows/mono/mono-2.0")).unwrap();
        let mono = verified_test_artifact(ArtifactKind::WineMono, fixture.path().join("m.msi"));
        assert!(record_dependency_provenance(&paths, PrefixDependency::WineMono, &mono).is_err());
        assert!(!prefix.join(MONO_RECEIPT).exists());
    }

    #[test]
    fn the_builtin_mono_receipt_requires_the_pinned_registered_version() {
        let registry = |version: &str| {
            format!(
                "WINE REGISTRY Version 2\n\n[Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Uninstall\\\\{{A}}] 1\n\"DisplayName\"=\"Wine Mono Runtime\"\n\"DisplayVersion\"=\"{version}\"\n\n[Software\\\\Other] 1\n\"DisplayName\"=\"Other\"\n"
            )
        };
        assert!(mono_versions_pinned(&registry("11.3.0")));
        assert!(!mono_versions_pinned(&registry("9.4.0")));
        assert!(!mono_versions_pinned("WINE REGISTRY Version 2\n"));
        assert!(!mono_versions_pinned(&format!(
            "{}{}",
            registry("11.3.0"),
            registry("9.4.0").replace("{A}", "{B}")
        )));

        let fixture = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(fixture.path()).unwrap();
        let prefix = fixture.path().join("prefix");
        fs::create_dir_all(prefix.join("drive_c/windows/mono/mono-2.0")).unwrap();
        fs::write(prefix.join(CUSTOM_MONO_RECEIPT), b"custom mono").unwrap();
        let mut mono = verified_test_artifact(ArtifactKind::WineMono, fixture.path().join("m.msi"));
        mono.observed_sha256 = crate::audit::WINE_MONO_MSI_SHA256.to_owned();
        // A custom Mono the pinned MSI did not replace keeps its tree.
        fs::write(prefix.join("system.reg"), registry("9.4.0")).unwrap();
        assert!(record_dependency_provenance(&paths, PrefixDependency::WineMono, &mono).is_err());
        assert!(!prefix.join(MONO_RECEIPT).exists());
        assert!(prefix.join(CUSTOM_MONO_RECEIPT).exists());
        fs::write(prefix.join("system.reg"), registry("11.3.0")).unwrap();
        record_dependency_provenance(&paths, PrefixDependency::WineMono, &mono).unwrap();
        assert!(prefix.join(MONO_RECEIPT).exists());
        assert!(!prefix.join(CUSTOM_MONO_RECEIPT).exists());
    }

    #[test]
    fn a_failed_custom_archive_never_blocks_the_next_one() {
        let fixture = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(fixture.path()).unwrap();
        let runner = fixture.path().join("runner");
        // Archive A extracts, but its `bin/wine` is not an ELF executable.
        let broken = build_archive(|builder| {
            append_directory(builder, "broken/");
            append_file(builder, "broken/bin/wine", b"#!/bin/sh");
            append_file(builder, "broken/bin/wineserver", b"server");
        });
        let artifact = custom_runtime_artifact(&paths, &broken);
        assert!(ensure_runtime(&paths, &artifact, &OperationControl::default()).is_err());
        assert!(!runner.join("runner/custom-wine").exists());
        assert!(!runner.join("custom-wine").exists());
        assert!(runner.join("custom-wine.staging").is_dir());

        // Archive B replaces the stale, Launcher-owned staging tree.
        let archive = custom_runtime_archive("wine-b");
        let artifact = custom_runtime_artifact(&paths, &archive);
        assert_eq!(
            ensure_runtime(&paths, &artifact, &OperationControl::default()).unwrap(),
            InstalledRuntime::Custom
        );
        assert!(runner.join("custom-wine/bin/wine").is_file());
        assert!(!runner.join("custom-wine.staging").exists());
        assert!(!runner.join("custom-wine/broken").exists());

        // A staging tree the Launcher did not start is never removed.
        let foreign = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(foreign.path()).unwrap();
        fs::create_dir_all(foreign.path().join("runner/custom-wine.staging/keep")).unwrap();
        let artifact = custom_runtime_artifact(&paths, &archive);
        assert!(ensure_runtime(&paths, &artifact, &OperationControl::default()).is_err());
        assert!(
            foreign
                .path()
                .join("runner/custom-wine.staging/keep")
                .is_dir()
        );
    }

    fn append_raw(
        builder: &mut tar::Builder<XzEncoder<Vec<u8>>>,
        entry_type: tar::EntryType,
        path: &str,
        link: &str,
        bytes: &[u8],
    ) {
        // Raw header names keep a leading `./`, which tar::Builder strips.
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(entry_type);
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
        header.as_mut_bytes()[157..157 + link.len()].copy_from_slice(link.as_bytes());
        header.set_cksum();
        builder.append(&header, Cursor::new(bytes)).unwrap();
    }

    #[test]
    fn a_custom_archive_may_use_a_dot_root_pax_header_and_inner_directory_links() {
        use tar::EntryType::{Directory as Dir, Regular, Symlink, XGlobalHeader};
        let elf = b"\x7fELF custom wine";
        let archive = build_archive(|builder| {
            append_raw(
                builder,
                XGlobalHeader,
                "pax_global_header",
                "",
                b"16 comment=abcd\n",
            );
            append_raw(builder, Dir, "./", "", b"");
            append_raw(builder, Dir, "./wine-11.0-amd64/", "", b"");
            append_raw(builder, Dir, "./wine-11.0-amd64/bin/", "", b"");
            append_raw(builder, Regular, "./wine-11.0-amd64/bin/wine", "", elf);
            append_raw(
                builder,
                Regular,
                "./wine-11.0-amd64/bin/wineserver",
                "",
                b"srv",
            );
            append_raw(
                builder,
                Symlink,
                "./wine-11.0-amd64/bin/wine64",
                "wine",
                b"",
            );
            append_raw(builder, Dir, "./wine-11.0-amd64/lib/", "", b"");
            append_raw(builder, Dir, "./wine-11.0-amd64/lib/wine/", "", b"");
            append_raw(
                builder,
                Regular,
                "./wine-11.0-amd64/lib/wine/ntdll.so",
                "",
                b"lib",
            );
            append_raw(builder, Symlink, "./wine-11.0-amd64/lib64", "lib", b"");
            append_raw(
                builder,
                Symlink,
                "./wine-11.0-amd64/bin/ntdll.so",
                "../lib/wine/ntdll.so",
                b"",
            );
        });
        let fixture = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(fixture.path()).unwrap();
        let artifact = custom_runtime_artifact(&paths, &archive);
        assert_eq!(
            ensure_runtime(&paths, &artifact, &OperationControl::default()).unwrap(),
            InstalledRuntime::Custom
        );
        let runtime = fixture.path().join("runner/custom-wine");
        assert_eq!(
            fs::read_link(runtime.join("lib64")).unwrap(),
            Path::new("lib")
        );
        assert_eq!(
            fs::read(runtime.join("lib64/wine/ntdll.so")).unwrap(),
            b"lib"
        );
        assert_eq!(fs::read(runtime.join("bin/ntdll.so")).unwrap(), b"lib");
        assert!(!runtime.join("pax_global_header").exists());

        // A tarball of the runtime directory's contents (`./bin/wine`).
        let flat = build_archive(|builder| {
            append_raw(builder, Dir, "./", "", b"");
            append_raw(builder, Dir, "./bin/", "", b"");
            append_raw(builder, Regular, "./bin/wine", "", elf);
            append_raw(builder, Regular, "./bin/wineserver", "", b"srv");
        });
        let fixture = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(fixture.path()).unwrap();
        let artifact = custom_runtime_artifact(&paths, &flat);
        ensure_runtime(&paths, &artifact, &OperationControl::default()).unwrap();
        assert!(fixture.path().join("runner/custom-wine/bin/wine").is_file());

        // Links that leave the extraction root, or walk through another link,
        // are still rejected.
        for (link, target) in [
            ("./w/lib64", "../.."),
            ("./w/lib64", "../../outside"),
            ("./w/lib64", "/etc"),
            ("./w/bin/up", "../../../w"),
            ("./w/viaLink", "wine64/../lib"),
            ("./w/dangling", "missing"),
        ] {
            let escaping = build_archive(|builder| {
                append_raw(builder, Dir, "./w/", "", b"");
                append_raw(builder, Dir, "./w/bin/", "", b"");
                append_raw(builder, Regular, "./w/bin/wine", "", elf);
                append_raw(builder, Regular, "./w/bin/wineserver", "", b"srv");
                append_raw(builder, Dir, "./w/lib/", "", b"");
                append_raw(builder, Symlink, "./w/wine64", "bin", b"");
                append_raw(builder, Symlink, link, target, b"");
            });
            let fixture = tempfile::tempdir().unwrap();
            let paths = ManagedPaths::prepare(fixture.path()).unwrap();
            let artifact = custom_runtime_artifact(&paths, &escaping);
            assert!(
                ensure_runtime(&paths, &artifact, &OperationControl::default()).is_err(),
                "{link} -> {target}"
            );
            assert!(!fixture.path().join("runner/custom-wine").exists());
        }
    }

    fn managed_paths_with_runtime() -> (tempfile::TempDir, ManagedPaths) {
        let fixture = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(fixture.path()).unwrap();
        let runtime_bin = paths.runtime().join("bin");
        fs::create_dir_all(&runtime_bin).unwrap();
        for executable in [
            runtime_bin.join("wine-preloader"),
            runtime_bin.join("wine"),
            runtime_bin.join("wineserver"),
        ] {
            fs::write(&executable, b"fixture").unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        }
        (fixture, paths)
    }

    /// The Android app's runtime in the managed root `root`: its `wine`,
    /// its `wineserver` and its `wine.inf`.
    fn app_runtime_fixture(root: &Path) {
        let runtime = root
            .join("runner")
            .join(crate::runtime::APP_RUNTIME_DIRECTORY);
        fs::create_dir_all(runtime.join("bin")).unwrap();
        for program in ["wine", "wineserver"] {
            let path = runtime.join("bin").join(program);
            fs::write(&path, b"fixture").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::create_dir_all(runtime.join("share/wine")).unwrap();
        fs::write(runtime.join("share/wine/wine.inf"), b"[version]").unwrap();
    }

    fn fixture_lease(root: &Path) -> File {
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(root.join("mutation.lock"))
            .unwrap()
    }

    fn assert_guest_preloader(request: &RequestSnapshot) {
        assert!(request.guest, "Wine starts through the guest executor");
        assert_eq!(request.executable, Path::new("/proc/self/fd/230"));
    }

    fn assert_wineserver_wait_profile(request: &RequestSnapshot, paths: &ManagedPaths) {
        assert!(
            request.guest,
            "wineserver starts through the guest executor"
        );
        assert_eq!(request.executable, Path::new("/proc/self/fd/232"));
        assert_eq!(request.arguments, [OsString::from("-w")]);
        assert_eq!(request.working_directory.as_deref(), Some(paths.root()));
        assert!(request.clears_environment);
        assert_eq!(request.output_limit, Some(PROCESS_OUTPUT_LIMIT));
        assert_eq!(request.inherited_descriptors, [235, 232, 234]);
        assert_launcher_cache_home(request, paths);
    }

    /// Every Wine process uses the Launcher-owned cache, never ~/.cache/wine.
    fn assert_launcher_cache_home(request: &RequestSnapshot, paths: &ManagedPaths) {
        assert!(request.environment.iter().any(|(name, value)| {
            name == OsStr::new("XDG_CACHE_HOME")
                && value == OsStr::new("/proc/self/fd/235/cache/xdg")
        }));
        assert!(paths.root().join("cache/xdg").is_dir());
    }

    fn directory_contains(root: &Path, needle: &[u8]) -> bool {
        let mut pending = vec![root.to_owned()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    pending.push(entry.path());
                } else if fs::read(entry.path())
                    .is_ok_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
                {
                    return true;
                }
            }
        }
        false
    }

    fn public_cache_entries(root: &Path) -> Vec<PathBuf> {
        fs::read_dir(root)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.file_name() != ".pd2launcher-retained")
            .filter(|entry| {
                let name = entry.file_name();
                !(name.to_string_lossy().starts_with("artifact-")
                    && name.to_string_lossy().ends_with(".partial")
                    && entry.metadata().is_ok_and(|metadata| metadata.len() == 0))
            })
            .map(|entry| entry.path())
            .collect()
    }

    fn safe_archive() -> Vec<u8> {
        build_archive(|builder| {
            append_directory(builder, "runtime/");
            append_directory(builder, "runtime/bin/");
            append_file(builder, "runtime/bin/wine", b"wine");
        })
    }

    fn archive_with_symlink() -> Vec<u8> {
        build_archive(|builder| {
            append_directory(builder, "runtime/");
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_cksum();
            builder
                .append_link(&mut header, "runtime/link", "/tmp/outside")
                .unwrap();
        })
    }

    fn archive_with_safe_symlink() -> Vec<u8> {
        build_archive(|builder| {
            append_directory(builder, "runtime/");
            append_directory(builder, "runtime/lib/");
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_cksum();
            builder
                .append_link(&mut header, "runtime/lib/libwine.so", "libwine.so.1")
                .unwrap();
            append_file(builder, "runtime/lib/libwine.so.1", b"library");
        })
    }

    fn archive_with_dangling_symlink() -> Vec<u8> {
        build_archive(|builder| {
            append_directory(builder, "runtime/");
            append_directory(builder, "runtime/lib/");
            append_symlink(builder, "runtime/lib/libwine.so", "missing-libwine.so.1");
        })
    }

    fn archive_with_symlink_cycle() -> Vec<u8> {
        build_archive(|builder| {
            append_directory(builder, "runtime/");
            append_directory(builder, "runtime/lib/");
            append_symlink(builder, "runtime/lib/libwine.so", "libwine.so.1");
            append_symlink(builder, "runtime/lib/libwine.so.1", "libwine.so");
        })
    }

    fn append_symlink(builder: &mut tar::Builder<XzEncoder<Vec<u8>>>, path: &str, target: &str) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        builder.append_link(&mut header, path, target).unwrap();
    }

    fn archive_with_parent_path() -> Vec<u8> {
        build_archive(|builder| {
            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_mode(0o644);
            header.set_cksum();
            // Build the raw header path to exercise validation rather than
            // relying on tar::Builder's own path rejection.
            header.as_mut_bytes()[..22].copy_from_slice(b"runtime/../outside.txt");
            header.set_cksum();
            builder.append(&header, Cursor::new(b"evil")).unwrap();
        })
    }

    fn build_archive(write: impl FnOnce(&mut tar::Builder<XzEncoder<Vec<u8>>>)) -> Vec<u8> {
        let encoder = XzEncoder::new(Vec::new(), 6);
        let mut builder = tar::Builder::new(encoder);
        write(&mut builder);
        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap()
    }

    fn append_directory(builder: &mut tar::Builder<XzEncoder<Vec<u8>>>, path: &str) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, path, io::empty()).unwrap();
    }

    fn append_file(builder: &mut tar::Builder<XzEncoder<Vec<u8>>>, path: &str, bytes: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, path, Cursor::new(bytes))
            .unwrap();
    }

    /// Serves a fixed PD2 client and counts every object request.
    struct CountingPd2Source {
        files: Vec<(&'static str, &'static [u8])>,
        object_requests: Arc<AtomicUsize>,
    }

    impl CountingPd2Source {
        fn url(name: &str) -> String {
            format!("{}/{name}", crate::manifest::PD2_ORIGIN)
        }
    }

    impl Pd2Source for CountingPd2Source {
        fn fetch_manifest(
            &mut self,
        ) -> Result<crate::manifest::ManifestResponse, crate::network::NetworkError> {
            use md5::Digest as _;
            let checksum = self
                .files
                .iter()
                .map(|(name, bytes)| {
                    format!("{}  {name}", crate::manifest::hex(&md5::Md5::digest(bytes)))
                })
                .collect::<Vec<_>>();
            let body = serde_json::to_vec(&serde_json::json!({ "checksum": checksum })).unwrap();
            Ok(crate::manifest::ManifestResponse {
                requested_url: crate::manifest::PD2_MANIFEST_URL.to_owned(),
                final_url: crate::manifest::PD2_MANIFEST_URL.to_owned(),
                declared_length: Some(body.len() as u64),
                body,
            })
        }

        fn open_object(
            &mut self,
            request: &crate::network::ObjectRequest,
        ) -> Result<crate::network::ObjectResponse, crate::network::NetworkError> {
            self.object_requests.fetch_add(1, Ordering::SeqCst);
            let (_, bytes) = self
                .files
                .iter()
                .find(|(name, _)| Self::url(name) == request.url)
                .ok_or_else(|| crate::network::NetworkError::permanent("unknown object"))?;
            Ok(crate::network::ObjectResponse {
                final_url: request.url.clone(),
                status: 200,
                declared_length: Some(bytes.len() as u64),
                content_range: None,
                retry_after: None,
                body: Box::new(Cursor::new(bytes.to_vec())),
            })
        }
    }

    fn client_cache_objects(root: &Path) -> BTreeMap<String, (Vec<u8>, i64, i64, u64)> {
        fs::read_dir(root.join(crate::install::CLIENT_PREFETCH).join("objects"))
            .unwrap()
            .flatten()
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .map(|entry| {
                let metadata = entry.metadata().unwrap();
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    (
                        fs::read(entry.path()).unwrap(),
                        metadata.mtime(),
                        metadata.mtime_nsec(),
                        metadata.ino(),
                    ),
                )
            })
            .collect()
    }

    #[test]
    fn installing_the_client_keeps_its_download_so_start_over_fetches_nothing_again() {
        let fixture = tempfile::tempdir().unwrap();
        let paths = ManagedPaths::prepare(fixture.path()).unwrap();
        let object_requests = Arc::new(AtomicUsize::new(0));
        let files: Vec<(&'static str, &'static [u8])> =
            vec![("Game.exe", b"pd2 game"), ("ProjectDiablo.dll", b"pd2 dll")];
        let synchronizer = UpdaterClientSynchronizer {
            source: Mutex::new(Box::new(CountingPd2Source {
                files: files.clone(),
                object_requests: object_requests.clone(),
            })),
            sequence: AtomicU64::new(1),
        };
        let control = OperationControl::default();

        // Download.
        synchronizer.prefetch(&paths, &control).unwrap();
        assert_eq!(object_requests.load(Ordering::SeqCst), 2);
        let downloaded = client_cache_objects(fixture.path());
        assert_eq!(downloaded.len(), 2);

        // Install Project Diablo 2: served from the cache, which it keeps
        // exactly as the Download left it.
        fs::create_dir_all(paths.game().join("ProjectD2")).unwrap();
        synchronizer.synchronize(&paths, &control).unwrap();
        assert_eq!(object_requests.load(Ordering::SeqCst), 2);
        for (name, bytes) in &files {
            assert_eq!(
                fs::read(paths.game().join("ProjectD2").join(name)).unwrap(),
                *bytes
            );
        }
        assert_eq!(client_cache_objects(fixture.path()), downloaded);

        // Start over, then Download and install again: no object is fetched.
        let lease = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(fixture.path().join(".pd2launcher.lock"))
            .unwrap();
        crate::install::reset_managed_prefix(paths.directory(), &lease).unwrap();
        assert!(!paths.game().exists());
        synchronizer.prefetch(&paths, &control).unwrap();
        assert_eq!(object_requests.load(Ordering::SeqCst), 2);
        assert_eq!(client_cache_objects(fixture.path()), downloaded);
        fs::create_dir_all(paths.game().join("ProjectD2")).unwrap();
        synchronizer.synchronize(&paths, &control).unwrap();
        assert_eq!(object_requests.load(Ordering::SeqCst), 2);
        assert_eq!(client_cache_objects(fixture.path()), downloaded);
        assert_eq!(
            fs::read(paths.game().join("ProjectD2/Game.exe")).unwrap(),
            b"pd2 game"
        );
    }

    #[test]
    fn a_project_diablo_another_launcher_installed_is_adopted_fetching_only_what_differs() {
        let files: Vec<(&'static str, &'static [u8])> = vec![
            ("Game.exe", b"pd2 game"),
            ("ProjectDiablo.dll", b"pd2 dll"),
            ("D2Common.dll", b"pd2 common"),
            ("PD2Shared.dll", b"pd2 shared"),
        ];
        // `local` is the ProjectD2 the official launcher left: no
        // `.pd2launcher` update records.
        let adopt = |local: &[(&str, &[u8])]| {
            let fixture = tempfile::tempdir().unwrap();
            let paths = ManagedPaths::prepare(fixture.path()).unwrap();
            let project = paths.game().join("ProjectD2");
            fs::create_dir_all(&project).unwrap();
            for (name, bytes) in local {
                fs::write(project.join(name), bytes).unwrap();
            }
            let inodes = local
                .iter()
                .map(|(name, _)| fs::metadata(project.join(name)).unwrap().ino())
                .collect::<Vec<_>>();
            let object_requests = Arc::new(AtomicUsize::new(0));
            let synchronizer = UpdaterClientSynchronizer {
                source: Mutex::new(Box::new(CountingPd2Source {
                    files: files.clone(),
                    object_requests: object_requests.clone(),
                })),
                sequence: AtomicU64::new(1),
            };
            synchronizer
                .synchronize(&paths, &OperationControl::default())
                .unwrap();
            for (name, bytes) in &files {
                assert_eq!(fs::read(project.join(name)).unwrap(), *bytes, "{name}");
            }
            // The Launcher's records: later updates start from this manifest.
            let accepted = fs::read(project.join(".pd2launcher/accepted-manifest.json")).unwrap();
            let mut source = CountingPd2Source {
                files: files.clone(),
                object_requests: Arc::new(AtomicUsize::new(0)),
            };
            assert_eq!(accepted, source.fetch_manifest().unwrap().body);
            // Which local files kept their inode (were never replaced).
            let kept = local
                .iter()
                .zip(inodes)
                .filter(|((name, _), inode)| {
                    fs::metadata(project.join(name)).unwrap().ino() == *inode
                })
                .map(|((name, _), _)| (*name).to_owned())
                .collect::<Vec<_>>();
            (object_requests.load(Ordering::SeqCst), kept)
        };

        // Identical to the manifest (plus the official launcher's own
        // file): nothing is fetched, nothing replaced, the records written.
        let mut identical = files.clone();
        identical.push(("PD2Launcher.exe", b"official launcher"));
        assert_eq!(
            adopt(&identical),
            (
                0,
                vec![
                    "Game.exe".to_owned(),
                    "ProjectDiablo.dll".to_owned(),
                    "D2Common.dll".to_owned(),
                    "PD2Shared.dll".to_owned(),
                    "PD2Launcher.exe".to_owned(),
                ]
            )
        );
        // An older one: only the changed and the missing file are fetched.
        assert_eq!(
            adopt(&[
                ("Game.exe", b"pd2 game"),
                ("ProjectDiablo.dll", b"older dll"),
                ("PD2Shared.dll", b"pd2 shared"),
            ]),
            (2, vec!["Game.exe".to_owned(), "PD2Shared.dll".to_owned()])
        );
    }
}
