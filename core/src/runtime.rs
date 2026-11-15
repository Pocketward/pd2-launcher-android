//! The managed Wine runtime each compile-target row provisions and executes.
//!
//! The AArch64 row uses the FEX-translated Wine-GE 8-26 runtime. The x86-64
//! row uses an upstream Wine 11.11 build in WoW64 mode: 32-bit Windows code
//! runs inside 64-bit Unix processes, so the host needs no i386 loader,
//! 32-bit multilib, or `libunwind`.

/// Exact size and SHA-256 of one executable inside the extracted runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeFile {
    pub(crate) size: u64,
    pub(crate) sha256: &'static str,
}

/// Frozen identity and execution shape of one managed Wine runtime.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ManagedRuntime {
    /// Runner directory name, equal to the archive's single top-level entry.
    pub(crate) identity: &'static str,
    /// Player-facing runtime name.
    pub(crate) display_name: &'static str,
    /// Verified archive name in the Launcher's artifact cache.
    pub(crate) cache_name: &'static str,
    pub(crate) archive_url: &'static str,
    pub(crate) archive_size: u64,
    pub(crate) archive_sha512: &'static str,
    pub(crate) wine: RuntimeFile,
    /// The ELF the Launcher executes (`bin/<loader_name>`): Wine-GE's
    /// `wine-preloader`, or `wine` itself for builds without a preloader.
    pub(crate) loader_name: &'static str,
    pub(crate) loader: RuntimeFile,
    /// Wine's preloader takes the `wine` loader path as its first argument;
    /// a runtime executed through `wine` itself does not.
    pub(crate) loader_takes_wine_path: bool,
    /// Exact `wine --version` output accepted by the runtime preflight.
    pub(crate) version: &'static str,
    /// Windows version written to a new prefix, when the runtime's default
    /// is not one the 1.14d client accepts. Recent Wine reports Windows 10,
    /// whose critical-section debug layout Fog.dll rejects at startup.
    pub(crate) windows_version: Option<&'static str>,
}

pub(crate) const WINE_GE_8_26: ManagedRuntime = ManagedRuntime {
    identity: "lutris-GE-Proton8-26-x86_64",
    display_name: "Wine-GE 8-26",
    cache_name: "wine-ge-8-26.verified",
    archive_url: "https://github.com/GloriousEggroll/wine-ge-custom/releases/download/GE-Proton8-26/wine-lutris-GE-Proton8-26-x86_64.tar.xz",
    archive_size: 233_577_252,
    archive_sha512: "3abd578ed45a16096843936e0258799450e6a5751bc67afaf8c58cd245c40e62794a89af25ae482d74d5ca2b7527d17ff77232781c6d9cf97bc02976c2f960bd",
    wine: RuntimeFile {
        size: 13_788,
        sha256: "d66993551779e46fd982953f3b6b1572a81a2c7341421314cc7dd9ded345dcbc",
    },
    loader_name: "wine-preloader",
    loader: RuntimeFile {
        size: 17_004,
        sha256: "6f6b47455492a697ecd264bfbe960a5331b0fa28cb57530ce095fcde8f022a4e",
    },
    loader_takes_wine_path: true,
    version: "wine-8.0-3001-g39021e609a2 (Staging)",
    windows_version: None,
};

/// Kron4ek Wine-Builds 11.11 `amd64-wow64` (upstream release SHA-256
/// `529d443301c7c798cbdee80769e855f47848d02c9b325d572d3f91988603e226`).
/// Wine 11.12 and later need `wl_display_create_queue_with_name`
/// (libwayland-client 1.23) for the Wayland driver, which Ubuntu 24.04 and
/// Debian 12 lack.
pub(crate) const WINE_WOW64_11_11: ManagedRuntime = ManagedRuntime {
    identity: "wine-11.11-amd64-wow64",
    display_name: "Wine 11.11",
    cache_name: "wine-11.11-amd64-wow64.verified",
    archive_url: "https://github.com/Kron4ek/Wine-Builds/releases/download/11.11/wine-11.11-amd64-wow64.tar.xz",
    archive_size: 95_429_744,
    archive_sha512: "022af24738636a5513d73771abd83115baeb6f93dd25ee6fa690aa572a2db433ca0cfd69f521c0941c93fdffb68c13d82d9db89a2805da5dd113347de75bf469",
    wine: RuntimeFile {
        size: 13_424,
        sha256: "302823189c8d93a597a3d7a6dd7bf710fafac1be1c665fc57666143476bdfe4e",
    },
    loader_name: "wine",
    loader: RuntimeFile {
        size: 13_424,
        sha256: "302823189c8d93a597a3d7a6dd7bf710fafac1be1c665fc57666143476bdfe4e",
    },
    loader_takes_wine_path: false,
    version: "wine-11.11",
    windows_version: Some("win7"),
};

/// Runtime of the row for `architecture` (a `std::env::consts::ARCH` value).
pub(crate) fn for_architecture(architecture: &str) -> &'static ManagedRuntime {
    match architecture {
        "x86_64" => &WINE_WOW64_11_11,
        _ => &WINE_GE_8_26,
    }
}

/// Runtime of the running Launcher binary's compile target.
pub(crate) fn current() -> &'static ManagedRuntime {
    for_architecture(std::env::consts::ARCH)
}

/// Receipt the Launcher writes last inside a pinned runtime directory.
pub(crate) const BUILTIN_RUNTIME_RECEIPT: &str = ".pd2launcher-archive.sha512";
/// Runner directory of a runtime materialized from a user-supplied custom
/// archive. The archive's own single top-level directory is
/// stripped, so any build layout with `bin/wine` works under one fixed name.
pub(crate) const CUSTOM_RUNTIME_DIRECTORY: &str = "custom-wine";
/// Custom-source provenance receipt written last inside the custom runtime.
pub(crate) const CUSTOM_RUNTIME_RECEIPT: &str = ".pd2launcher-custom-source.json";
/// Whether `runner/custom-wine` carries a custom-source receipt.
pub(crate) fn custom_receipt_present(root: &crate::safe_fs::Directory) -> bool {
    root.open_beneath(
        &std::path::Path::new("runner")
            .join(CUSTOM_RUNTIME_DIRECTORY)
            .join(CUSTOM_RUNTIME_RECEIPT),
        rustix::fs::OFlags::PATH,
    )
    .is_ok()
}

/// Windows version written to a prefix created by a custom runtime. Recent
/// Wine reports Windows 10, whose layout the 1.14d Fog.dll rejects.
const CUSTOM_WINDOWS_VERSION: &str = "win7";

/// Runner directory of the Android app's runtime: an ARM64 Wine 11.0 in
/// WoW64 mode, whose 32-bit x86 code Box64's `wowbox64.dll` runs. The app
/// unpacks it there from its APK (made for its own data folder, so it has no
/// pinned digest) and replaces it with each new app version; the Launcher
/// never downloads it.
pub(crate) const APP_RUNTIME_DIRECTORY: &str = "wine-11.0-arm64";
/// Player-facing name of the Android app's runtime.
pub(crate) const APP_RUNTIME_DISPLAY_NAME: &str = "Wine 11.0";
/// Exact `wine --version` output of the Android app's runtime.
pub(crate) const APP_RUNTIME_VERSION: &str = "wine-11.0";
/// The prefix the Android app's runtime starts from, in the managed root: a
/// copy of it becomes a new prefix (the app makes it with the same Wine, so
/// no `wineboot` runs on the phone). Game.exe already runs as Windows 7 in it.
/// This is the PLAIN template; [`APP_PREFIX_TEMPLATE_GOLDEN`] is the other.
pub(crate) const APP_PREFIX_TEMPLATE: &str = "runner/wine-11.0-arm64-prefix";
/// The GOLDEN template: the PLAIN one with what the official installers
/// create (Blizzard's Diablo II and Lord of Destruction installers, the
/// Project Diablo 2 installer with its Visual C++ redist), except Blizzard's
/// own files (the app's `runtime/golden.py`).
pub(crate) const APP_PREFIX_TEMPLATE_GOLDEN: &str = "runner/wine-11.0-arm64-prefix-golden";
/// The mark of a template's kind in it, and so in each prefix made from it:
/// `plain` or `golden`.
pub(crate) const PREFIX_TEMPLATE_MARK: &str = ".pd2-prefix-template";

/// Which template a new prefix of the Android app's runtime is a copy of.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum PrefixTemplate {
    /// For Blizzard's installers, which must never see an installed game.
    #[default]
    Plain,
    /// For Fast download and imports: the official installers' state is
    /// there from the start.
    Golden,
}

impl PrefixTemplate {
    /// The template's folder in the managed root.
    pub(crate) const fn directory(self) -> &'static str {
        match self {
            Self::Plain => APP_PREFIX_TEMPLATE,
            Self::Golden => APP_PREFIX_TEMPLATE_GOLDEN,
        }
    }

    /// The kind as [`PREFIX_TEMPLATE_MARK`] holds it.
    pub(crate) const fn mark(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Golden => "golden",
        }
    }

    /// The template for an install that the source decides: GOLDEN for Fast
    /// download (`direct_download`) and for an import, PLAIN for Blizzard's
    /// installers.
    pub(crate) const fn for_source(direct_download: bool, import: bool) -> Self {
        if direct_download || import {
            Self::Golden
        } else {
            Self::Plain
        }
    }
}
/// The link a prefix of the Android app's runtime reaches the app's x86
/// .NET runtime (the official PD2 launcher's) by, and its target, relative
/// to the link's folder: `runner/dotnet-6.0.36-x86` in the managed root.
pub(crate) const APP_DOTNET_LINK: &str = "prefix/drive_c/Program Files (x86)/dotnet";
pub(crate) const APP_DOTNET_LINK_TARGET: &str = "../../../runner/dotnet-6.0.36-x86";

/// The runtime an installation actually uses: the row's pinned runtime, or a
/// runtime installed from a custom source. A custom runtime is
/// used only while the Wine download source is custom; with the built-in
/// source configured, only the pinned runtime is ever selected, audited, or
/// executed, whatever custom receipt may exist beside it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstalledRuntime {
    BuiltIn(&'static ManagedRuntime),
    Custom,
    /// The Android app's runtime ([`APP_RUNTIME_DIRECTORY`]), the only one
    /// an Android host runs.
    App,
}

impl InstalledRuntime {
    /// Select the runtime for an installation whose Wine source is currently
    /// custom (`custom_source`) or built-in. With a custom Wine source, a
    /// runtime carrying a custom-source receipt is used; otherwise (no custom
    /// runtime yet, or the built-in source) the row's pinned runtime is used
    /// and audited against its pins.
    pub(crate) fn select(root: &crate::safe_fs::Directory, custom_source: bool) -> Self {
        if crate::android_host() {
            return Self::App;
        }
        if custom_source && custom_receipt_present(root) {
            return Self::Custom;
        }
        Self::BuiltIn(current())
    }

    pub(crate) fn is_custom(self) -> bool {
        self == Self::Custom
    }

    /// Directory below `runner/`.
    pub(crate) fn directory(self) -> &'static str {
        match self {
            Self::BuiltIn(runtime) => runtime.identity,
            Self::Custom => CUSTOM_RUNTIME_DIRECTORY,
            Self::App => APP_RUNTIME_DIRECTORY,
        }
    }

    /// ELF the Launcher executes from `bin/`. Any Wine build can be started
    /// through `bin/wine` itself, so a custom runtime never needs a preloader.
    pub(crate) fn loader_name(self) -> &'static str {
        match self {
            Self::BuiltIn(runtime) => runtime.loader_name,
            Self::Custom | Self::App => "wine",
        }
    }

    pub(crate) fn loader_takes_wine_path(self) -> bool {
        match self {
            Self::BuiltIn(runtime) => runtime.loader_takes_wine_path,
            Self::Custom | Self::App => false,
        }
    }

    pub(crate) fn windows_version(self) -> Option<&'static str> {
        match self {
            Self::BuiltIn(runtime) => runtime.windows_version,
            Self::Custom => Some(CUSTOM_WINDOWS_VERSION),
            // Its prefix template sets Game.exe's own version.
            Self::App => None,
        }
    }

    /// Player-facing name of this runtime.
    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::BuiltIn(runtime) => runtime.display_name,
            Self::Custom => current().display_name,
            Self::App => APP_RUNTIME_DISPLAY_NAME,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_install_source_decides_the_prefix_template() {
        // Blizzard's installers must never see an installed game.
        assert_eq!(
            PrefixTemplate::for_source(false, false),
            PrefixTemplate::Plain
        );
        assert_eq!(
            PrefixTemplate::for_source(true, false),
            PrefixTemplate::Golden
        );
        assert_eq!(
            PrefixTemplate::for_source(false, true),
            PrefixTemplate::Golden
        );
        assert_eq!(
            PrefixTemplate::Plain.directory(),
            super::APP_PREFIX_TEMPLATE
        );
        assert_eq!(
            PrefixTemplate::Golden.directory(),
            "runner/wine-11.0-arm64-prefix-golden"
        );
        assert_eq!(PrefixTemplate::Plain.mark(), "plain");
        assert_eq!(PrefixTemplate::Golden.mark(), "golden");
    }

    #[test]
    fn each_row_selects_its_frozen_runtime() {
        assert_eq!(for_architecture("x86_64"), &WINE_WOW64_11_11);
        assert_eq!(for_architecture("aarch64"), &WINE_GE_8_26);
        assert!(
            WINE_WOW64_11_11
                .archive_url
                .ends_with(&format!("/{}.tar.xz", WINE_WOW64_11_11.identity))
        );
        assert_eq!(
            WINE_WOW64_11_11.cache_name,
            format!("{}.verified", WINE_WOW64_11_11.identity)
        );
        for runtime in [&WINE_GE_8_26, &WINE_WOW64_11_11] {
            assert_eq!(runtime.archive_sha512.len(), 128);
            assert_eq!(runtime.wine.sha256.len(), 64);
            assert_eq!(runtime.loader.sha256.len(), 64);
        }
    }

    #[test]
    fn each_row_names_and_caches_its_own_runtime() {
        assert_eq!(for_architecture("x86_64").display_name, "Wine 11.11");
        assert_eq!(for_architecture("x86_64").version, "wine-11.11");
        assert_eq!(for_architecture("aarch64").display_name, "Wine-GE 8-26");
        assert_eq!(
            for_architecture("x86_64").cache_name,
            "wine-11.11-amd64-wow64.verified"
        );
        assert_eq!(
            for_architecture("aarch64").cache_name,
            "wine-ge-8-26.verified"
        );
    }

    #[test]
    fn a_custom_runtime_is_selected_only_while_the_wine_source_is_custom() {
        let fixture = tempfile::tempdir().unwrap();
        let root = crate::safe_fs::Directory::open(fixture.path()).unwrap();
        let builtin = current();
        for custom_source in [false, true] {
            assert_eq!(
                InstalledRuntime::select(&root, custom_source),
                InstalledRuntime::BuiltIn(builtin)
            );
        }

        let custom = fixture.path().join("runner").join(CUSTOM_RUNTIME_DIRECTORY);
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(custom.join(CUSTOM_RUNTIME_RECEIPT), b"{}").unwrap();
        let detected = InstalledRuntime::select(&root, true);
        assert_eq!(detected, InstalledRuntime::Custom);
        assert_eq!(detected.directory(), "custom-wine");
        assert_eq!(detected.loader_name(), "wine");
        assert!(!detected.loader_takes_wine_path());
        assert_eq!(detected.windows_version(), Some("win7"));
        // With the built-in source configured, a custom receipt is ignored.
        assert_eq!(
            InstalledRuntime::select(&root, false),
            InstalledRuntime::BuiltIn(builtin)
        );

        // A custom source keeps using its custom runtime beside a pinned one.
        let pinned = fixture.path().join("runner").join(builtin.identity);
        std::fs::create_dir_all(&pinned).unwrap();
        std::fs::write(pinned.join(BUILTIN_RUNTIME_RECEIPT), b"tampered").unwrap();
        assert_eq!(
            InstalledRuntime::select(&root, true),
            InstalledRuntime::Custom
        );
        assert_eq!(
            InstalledRuntime::select(&root, false),
            InstalledRuntime::BuiltIn(builtin)
        );
    }

    #[test]
    fn a_runtime_without_a_preloader_executes_wine_directly() {
        assert_eq!(WINE_WOW64_11_11.loader_name, "wine");
        assert_eq!(WINE_WOW64_11_11.loader, WINE_WOW64_11_11.wine);
        const { assert!(!WINE_WOW64_11_11.loader_takes_wine_path) };
        assert_eq!(WINE_WOW64_11_11.windows_version, Some("win7"));
        const { assert!(WINE_GE_8_26.loader_takes_wine_path) };
    }
}
