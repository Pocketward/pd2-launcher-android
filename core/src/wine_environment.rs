//! The environment a Wine process starts with.
//!
//! The Launcher decides it: a Wine process starts from an empty
//! environment with the Launcher's settings, and keeps from the Launcher's
//! own environment only what describes the session it runs in (display,
//! audio, session bus) and the computer's graphics drivers. Nothing else of
//! the host reaches Wine, so the Launcher's own variables (its paths,
//! download sources, CD keys) never do.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt as _,
};

/// How a Launcher setting meets what the host passes through.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    /// Always the Launcher's value.
    Required,
    /// The Launcher's choice (debug channels, DLL overrides, FEX's strict
    /// settings); like `Required`, since no host value reaches it.
    Preference,
    /// Used only where the passed-through environment has none.
    Default,
}

/// One environment variable the Launcher wants a Wine process to have.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Setting {
    pub(crate) name: OsString,
    pub(crate) value: OsString,
    pub(crate) kind: Kind,
}

impl Setting {
    pub(crate) fn new(name: impl Into<OsString>, value: impl Into<OsString>, kind: Kind) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            kind,
        }
    }
}

/// The session a Wine program runs in: its display, audio, and session bus.
const SESSION: [&str; 7] = [
    "XDG_RUNTIME_DIR",
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XAUTHORITY",
    "DBUS_SESSION_BUS_ADDRESS",
    "PULSE_SERVER",
    "PIPEWIRE_REMOTE",
];

/// Prefixes of the variables describing the computer's graphics drivers:
/// which Mesa driver to load (Zink on DroidDeck), where the Vulkan driver is
/// (Turnip), and their options. `FD_` is Mesa freedreno's prefix, which
/// Turnip also reads: the Android app sets `FD_DEV_FEATURES` on an Adreno
/// 740 so that Turnip's TP UBWC flag hint is the same as that of the
/// phone's own driver. Without them the game may find no GPU
/// driver. NVIDIA's (`__NV_`, `__VK_LAYER_NV_`, `__GL_`) and Mesa's
/// `DRI_PRIME` come with them, so a laptop started with `prime-run` (or
/// `DRI_PRIME=1`) draws on the same GPU throughout.
const GRAPHICS_PREFIXES: [&str; 12] = [
    "MESA_",
    "GALLIUM_",
    "ZINK_",
    "TU_",
    "FD_",
    "VK_",
    "LIBGL_",
    "__GLX_",
    "__EGL_",
    "__NV_",
    "__VK_LAYER_NV_",
    "__GL_",
];

/// Other system settings Wine keeps: glibc's tunables (DroidDeck tunes them
/// for proot), gamescope's Vulkan layer switch, and Mesa's PRIME choice.
const SYSTEM: [&str; 3] = ["GLIBC_TUNABLES", "DISABLE_GAMESCOPE_WSI", "DRI_PRIME"];

/// What the Android app's guests also keep: the gamepad file of the XInput
/// bridge, the game's present counter and direct-present socket (read by the
/// app userland's Mesa in the game's own process), the userland's font and
/// X locale configuration, and Box64's own settings (`BOX64_*`, for
/// `wowbox64.dll` in every Wine process). PulseAudio's `PULSE_SERVER` is in
/// [`SESSION`].
const ANDROID: [&str; 5] = [
    "PD2_GAMEPAD_FILE",
    "PD2_PRESENT_COUNTER",
    "PD2_PRESENT_SOCKET",
    "FONTCONFIG_FILE",
    "XLOCALEDIR",
];
/// Prefixes of what the Android app's guests also keep: Box64's settings,
/// PanVK's (the Mali driver's), and those of the routes to the phone's own
/// driver: libhybris's (`HYBRIS_*`: its linker's folder and library path,
/// the Android API level) with the app's libhybris Vulkan driver's
/// (`PD2_HYBRIS_*`), and RADV's (`RADV_*`, Mesa's driver for Samsung's
/// Xclipse GPUs). The app starts the engine with an environment of its own
/// making, which has these values only for the route it takes.
const ANDROID_PREFIXES: [&str; 5] = ["BOX64_", "PAN_", "HYBRIS_", "PD2_HYBRIS_", "RADV_"];

/// Added to the Launcher's DLL overrides on the Android host: the XInput
/// bridge (`tools/xinput-bridge.c`) installed as `xinput1_4.dll`, and no
/// `nsiproxy.sys`. Android's platform policy denies apps the NETLINK_ROUTE
/// `bind` and `RTM_GETLINK` (a neverallow for every `untrusted_app` domain;
/// only a permissive device allows them), so glibc's `if_nameindex()` fails.
/// Wine's nsiproxy dereferences that NULL while holding its interface lock,
/// and every later network query of the prefix waits on the lock forever:
/// the game's local IP lookup at start, and Blizzard's downloader before it
/// opens its window. Without the driver those queries fail at once and Wine
/// falls back to the host's resolver. The driver runs in `winedevice.exe`,
/// which the first Wine process of a prefix starts with its own environment,
/// so every Wine process carries the override.
/// Also no `mscoree` and no `mshtml`: the Android app's prefix has no Wine
/// Mono and no Wine Gecko (no program the Launcher runs there needs them),
/// and with both off Wine never offers to install them, also when it
/// updates the prefix for a new runtime. And none of Wine's HID and
/// Bluetooth drivers (`winebus.sys`, `winehid.sys`, `hidclass.sys`,
/// `winebth.sys`): the game's controller is the XInput bridge, and without
/// them every Wine process of the prefix uses less memory.
const ANDROID_DLL_OVERRIDES: &str = ";xinput1_4=n;nsiproxy.sys=d;mscoree=;mshtml=;\
winebus.sys=d;winehid.sys=d;hidclass.sys=d;winebth.sys=d";

/// `PD2_APP_COMPATIBILITY_<NAME>=<value>` in the Launcher engine's own
/// environment: a value of the Android app's Compatibility graphics driver,
/// which a Wine process gets as `<NAME>=<value>` (over a value of the same
/// name) while that driver is in use ([`use_compatibility_driver`]).
const ANDROID_COMPATIBILITY_PREFIX: &str = "PD2_APP_COMPATIBILITY_";

/// The Android app's last Play asked for its Compatibility graphics driver.
static COMPATIBILITY_DRIVER: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Held by the tests that change the graphics driver in use, which is the
/// whole process's.
#[cfg(test)]
pub(crate) static DRIVER_IN_USE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The Android app's graphics driver for every Wine process started from
/// now on: the Compatibility driver's values, or the standard ones. The
/// engine sets it at its start from the last Play's driver, and again at
/// each Play.
pub(crate) fn use_compatibility_driver(compatibility: bool) {
    COMPATIBILITY_DRIVER.store(compatibility, std::sync::atomic::Ordering::Relaxed);
    eprintln!(
        "pd2-launcher: graphics driver {}",
        if compatibility {
            "compatibility"
        } else {
            "standard"
        }
    );
}

/// Wine's debug channels that Detailed logging adds: each line's time and
/// process, every exception, each DLL that loads, and Winsock, whose
/// connects, closes and errors the engine keeps and whose other lines (one
/// for each send, receive and wait) it drops
/// ([`crate::activity::winsock_line`]).
pub(crate) const DETAILED_WINEDEBUG: &str = "+timestamp,+pid,warn+seh,+loaddll,+winsock";

/// What a Wine process of the game or the official launcher gets for the
/// Android app's Detailed logging (`detailed`, which a Play or a press of
/// the official launcher carries) over `winedebug`, its own debug channels:
/// those with [`DETAILED_WINEDEBUG`], and Box64's log (the settings it took
/// and the CPU it found). Nothing while Detailed logging is off.
pub(crate) fn detailed_settings(detailed: bool, winedebug: &str) -> Vec<Setting> {
    if !detailed {
        return Vec::new();
    }
    vec![
        Setting::new(
            "WINEDEBUG",
            format!("{winedebug},{DETAILED_WINEDEBUG}"),
            Kind::Preference,
        ),
        Setting::new("BOX64_LOG", "1", Kind::Preference),
    ]
}

/// The log line with the Box64 settings that `program` (a path or a name)
/// starts with: every `BOX64_*` of its `environment` ([`compose`] puts them
/// in name order); none when it has none (the desktop).
pub(crate) fn box64_line(program: &str, environment: &[(OsString, OsString)]) -> Option<String> {
    let settings = environment
        .iter()
        .filter(|(name, _)| name.as_bytes().starts_with(b"BOX64_"))
        .map(|(name, value)| format!("{}={}", name.to_string_lossy(), value.to_string_lossy()))
        .collect::<Vec<_>>();
    if settings.is_empty() {
        return None;
    }
    let name = program.rsplit(['\\', '/']).next().unwrap_or(program);
    // A setting from the host can name a path in the home folder.
    Some(crate::activity::redacted(&format!(
        "pd2-launcher: Box64 for {name}: {}",
        settings.join(" ")
    )))
}

/// Whether the host's `name` reaches Wine.
fn passes_through(name: &OsStr, value: &OsStr, android: bool) -> bool {
    let name = name.as_bytes();
    let known = SESSION.iter().any(|known| name == known.as_bytes())
        || (android && ANDROID.iter().any(|known| name == known.as_bytes()))
        || (android
            && ANDROID_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix.as_bytes())))
        || SYSTEM.iter().any(|known| name == known.as_bytes())
        || GRAPHICS_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix.as_bytes()));
    let value = value.as_bytes();
    known
        && !value.is_empty()
        && value.len() <= 4096
        && !value.iter().any(|byte| *byte < b' ' || *byte == 0x7f)
}

/// The environment for a Wine process: what passes through from `host`,
/// with `settings` applied as their kind says.
pub(crate) fn compose(
    host: impl IntoIterator<Item = (OsString, OsString)>,
    settings: &[Setting],
) -> Vec<(OsString, OsString)> {
    compose_on(host, settings, crate::android_host())
}

/// [`compose`] on the Android host (`android`) or the desktop.
pub(crate) fn compose_on(
    host: impl IntoIterator<Item = (OsString, OsString)>,
    settings: &[Setting],
    android: bool,
) -> Vec<(OsString, OsString)> {
    let host = host.into_iter().collect::<BTreeMap<_, _>>();
    let compatibility =
        if android && COMPATIBILITY_DRIVER.load(std::sync::atomic::Ordering::Relaxed) {
            host.iter()
                .filter_map(|(name, value)| {
                    let name = name
                        .as_bytes()
                        .strip_prefix(ANDROID_COMPATIBILITY_PREFIX.as_bytes())?;
                    Some((OsStr::from_bytes(name).to_owned(), value.clone()))
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
    let mut environment = host
        .into_iter()
        .chain(compatibility)
        .filter(|(name, value)| passes_through(name, value, android))
        .collect::<BTreeMap<_, _>>();
    for setting in settings {
        if setting.kind == Kind::Default && environment.contains_key(&setting.name) {
            continue;
        }
        environment.insert(setting.name.clone(), setting.value.clone());
    }
    // The game's XInput is the Launcher's bridge in the prefix, never Wine's.
    // A Wine process started without the Launcher's overrides gets the
    // Android ones alone: with mshtml on, Wine updating the prefix (its first
    // start after the runtime changed, say) would open Gecko's dialog, which
    // nobody can answer on Android.
    if android {
        match environment.get_mut(OsStr::new("WINEDLLOVERRIDES")) {
            Some(overrides) => overrides.push(ANDROID_DLL_OVERRIDES),
            None => {
                environment.insert(
                    OsString::from("WINEDLLOVERRIDES"),
                    OsString::from(ANDROID_DLL_OVERRIDES.trim_start_matches(';')),
                );
            }
        }
    }
    environment.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    fn get<'a>(environment: &'a [(OsString, OsString)], name: &str) -> Option<&'a str> {
        environment
            .iter()
            .find(|(key, _)| key == name)
            .and_then(|(_, value)| value.to_str())
    }

    #[test]
    fn only_the_session_and_graphics_drivers_pass_and_the_launcher_decides_the_rest() {
        let settings = [
            Setting::new("WINEPREFIX", "/launcher/prefix", Kind::Required),
            Setting::new("WINEDEBUG", "fixme-all", Kind::Preference),
            Setting::new("LANG", "C.UTF-8", Kind::Default),
            Setting::new("PATH", "/usr/bin", Kind::Default),
        ];
        let environment = compose(
            host(&[
                ("DISPLAY", ":0"),
                ("GALLIUM_DRIVER", "zink"),
                ("VK_ICD_FILENAMES", "/icd.json"),
                ("GLIBC_TUNABLES", "glibc.pthread.rseq=0"),
                ("WINEPREFIX", "/somewhere/else"),
                ("WINEDEBUG", "+seh"),
                ("LANG", "en_US.UTF-8"),
                ("PATH", "/home/player/bin"),
                ("PD2_LAUNCHER_HOME", "/private"),
                ("D2_KEY", "secret"),
                ("DOTNET_ROOT", "/usr/share/dotnet"),
                ("MESA_SHADER_CACHE_DIR", "bad\nvalue"),
            ]),
            &settings,
        );
        assert_eq!(get(&environment, "DISPLAY"), Some(":0"));
        assert_eq!(get(&environment, "GALLIUM_DRIVER"), Some("zink"));
        assert_eq!(get(&environment, "VK_ICD_FILENAMES"), Some("/icd.json"));
        assert_eq!(
            get(&environment, "GLIBC_TUNABLES"),
            Some("glibc.pthread.rseq=0")
        );
        assert_eq!(get(&environment, "WINEPREFIX"), Some("/launcher/prefix"));
        assert_eq!(get(&environment, "WINEDEBUG"), Some("fixme-all"));
        assert_eq!(get(&environment, "LANG"), Some("C.UTF-8"));
        assert_eq!(get(&environment, "PATH"), Some("/usr/bin"));
        assert_eq!(get(&environment, "PD2_LAUNCHER_HOME"), None);
        assert_eq!(get(&environment, "D2_KEY"), None);
        assert_eq!(get(&environment, "DOTNET_ROOT"), None);
        assert_eq!(get(&environment, "MESA_SHADER_CACHE_DIR"), None);
    }

    #[test]
    fn the_android_host_also_passes_the_gamepad_file_and_pulseaudio() {
        let app = host(&[
            ("PD2_GAMEPAD_FILE", "/data/gamepad.bin"),
            ("PULSE_SERVER", "unix:/data/run/pulse/native"),
            ("PD2_PRESENT_COUNTER", "/data/run/presents"),
            ("BOX64_NOBANNER", "1"),
            ("PAN_I_WANT_A_BROKEN_VULKAN_DRIVER", "1"),
            ("PD2_ANDROID_APP", "1"),
            ("PD2_APP_COMPATIBILITY_TU_DEBUG", "sysmem,noubwc"),
        ]);
        let android = compose_on(app.clone(), &[], true);
        assert_eq!(get(&android, "PD2_GAMEPAD_FILE"), Some("/data/gamepad.bin"));
        assert_eq!(
            get(&android, "PULSE_SERVER"),
            Some("unix:/data/run/pulse/native")
        );
        assert_eq!(
            get(&android, "PD2_PRESENT_COUNTER"),
            Some("/data/run/presents")
        );
        assert_eq!(get(&android, "BOX64_NOBANNER"), Some("1"));
        assert_eq!(
            get(&android, "PAN_I_WANT_A_BROKEN_VULKAN_DRIVER"),
            Some("1")
        );
        assert_eq!(get(&android, "PD2_ANDROID_APP"), None);
        assert_eq!(get(&android, "PD2_APP_COMPATIBILITY_TU_DEBUG"), None);
        let desktop = compose_on(app, &[], false);
        assert_eq!(get(&desktop, "PD2_GAMEPAD_FILE"), None);
        assert_eq!(get(&desktop, "PD2_PRESENT_COUNTER"), None);
        assert_eq!(get(&desktop, "BOX64_NOBANNER"), None);
        assert_eq!(
            get(&desktop, "PULSE_SERVER"),
            Some("unix:/data/run/pulse/native")
        );
    }

    #[test]
    fn the_android_host_passes_the_phone_driver_routes_values_and_no_other() {
        let app = host(&[
            ("HYBRIS_LD_LIBRARY_PATH", "/data/hybris/lib:/vendor/lib64"),
            ("HYBRIS_LINKER_DIR", "/data/hybris/lib/libhybris/linker"),
            ("HYBRIS_ANDROID_SDK_VERSION", "36"),
            ("PD2_HYBRIS_DEBUG", "1"),
            ("PD2_HYBRIS_SEGV", "1"),
            ("RADV_XCLIPSE_LOG", "1"),
            ("RADV_DEBUG", "nocache"),
            ("HYBRIS", "1"),
            ("PD2_HYBRIS", "1"),
            ("RADV", "1"),
            ("XHYBRIS_LD_LIBRARY_PATH", "/elsewhere"),
            ("PD2_HYBRID_DEBUG", "1"),
            ("ACO_DEBUG", "1"),
            ("PD2_LAUNCHER_HOME", "/private"),
            ("HYBRIS_LD_DEBUG", "bad\nvalue"),
            ("RADV_PERFTEST", ""),
        ]);
        let android = compose_on(app.clone(), &[], true);
        for (name, value) in [
            ("HYBRIS_LD_LIBRARY_PATH", "/data/hybris/lib:/vendor/lib64"),
            ("HYBRIS_LINKER_DIR", "/data/hybris/lib/libhybris/linker"),
            ("HYBRIS_ANDROID_SDK_VERSION", "36"),
            ("PD2_HYBRIS_DEBUG", "1"),
            ("PD2_HYBRIS_SEGV", "1"),
            ("RADV_XCLIPSE_LOG", "1"),
            ("RADV_DEBUG", "nocache"),
        ] {
            assert_eq!(get(&android, name), Some(value), "{name}");
        }
        for name in [
            "HYBRIS",
            "PD2_HYBRIS",
            "RADV",
            "XHYBRIS_LD_LIBRARY_PATH",
            "PD2_HYBRID_DEBUG",
            "ACO_DEBUG",
            "PD2_LAUNCHER_HOME",
            "HYBRIS_LD_DEBUG",
            "RADV_PERFTEST",
        ] {
            assert_eq!(get(&android, name), None, "{name}");
        }
        // The desktop keeps none of them.
        let desktop = compose_on(app, &[], false);
        for name in [
            "HYBRIS_LD_LIBRARY_PATH",
            "PD2_HYBRIS_DEBUG",
            "RADV_XCLIPSE_LOG",
        ] {
            assert_eq!(get(&desktop, name), None, "{name}");
        }
    }

    #[test]
    fn the_compatibility_driver_values_reach_wine_only_while_it_is_in_use() {
        let _driver = DRIVER_IN_USE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let app = host(&[
            ("TU_DEBUG", "sysmem"),
            ("PD2_APP_COMPATIBILITY_TU_DEBUG", "sysmem,noubwc,nolrz"),
        ]);
        assert_eq!(
            get(&compose_on(app.clone(), &[], true), "TU_DEBUG"),
            Some("sysmem")
        );
        use_compatibility_driver(true);
        let compatibility = compose_on(app.clone(), &[], true);
        let desktop = compose_on(app.clone(), &[], false);
        use_compatibility_driver(false);
        assert_eq!(get(&compatibility, "TU_DEBUG"), Some("sysmem,noubwc,nolrz"));
        assert_eq!(get(&desktop, "TU_DEBUG"), Some("sysmem"));
        assert_eq!(get(&compose_on(app, &[], true), "TU_DEBUG"), Some("sysmem"));
    }

    #[test]
    fn mesa_freedrenos_options_reach_wine_with_either_graphics_driver() {
        let _driver = DRIVER_IN_USE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let app = host(&[
            ("FD_DEV_FEATURES", "enable_tp_ubwc_flag_hint=0"),
            ("PD2_APP_COMPATIBILITY_TU_DEBUG", "sysmem,noubwc,nolrz"),
            ("FDX", "1"),
        ]);
        let standard = compose_on(app.clone(), &[], true);
        use_compatibility_driver(true);
        let compatibility = compose_on(app.clone(), &[], true);
        use_compatibility_driver(false);
        let desktop = compose_on(app, &[], false);
        for environment in [&standard, &compatibility, &desktop] {
            assert_eq!(
                get(environment, "FD_DEV_FEATURES"),
                Some("enable_tp_ubwc_flag_hint=0")
            );
            assert_eq!(get(environment, "FDX"), None);
        }
    }

    #[test]
    fn detailed_logging_adds_wine_channels_and_box64s_log_over_the_programs_own() {
        assert!(detailed_settings(false, "fixme-all").is_empty());
        let mut settings = vec![Setting::new("WINEDEBUG", "fixme-all", Kind::Preference)];
        settings.extend(detailed_settings(true, "fixme-all"));
        let environment = compose_on(host(&[("BOX64_LOG", "0")]), &settings, true);
        assert_eq!(
            get(&environment, "WINEDEBUG"),
            Some("fixme-all,+timestamp,+pid,warn+seh,+loaddll,+winsock")
        );
        assert_eq!(get(&environment, "BOX64_LOG"), Some("1"));
    }

    #[test]
    fn the_box64_line_names_the_program_and_its_box64_settings_only() {
        let environment = host(&[
            ("BOX64_NOBANNER", "1"),
            ("BOX64_DYNAREC_PROGRAMS", "Game.exe;"),
            ("WINEDEBUG", "fixme-all"),
        ]);
        assert_eq!(
            box64_line("C:\\Diablo II\\ProjectD2\\Game.exe", &environment).as_deref(),
            Some(
                "pd2-launcher: Box64 for Game.exe: BOX64_NOBANNER=1 BOX64_DYNAREC_PROGRAMS=Game.exe;"
            )
        );
        assert_eq!(
            box64_line("Game.exe", &host(&[("WINEDEBUG", "-all")])),
            None
        );
    }

    #[test]
    fn only_the_android_host_loads_the_xinput_bridge_and_no_nsiproxy_mono_or_gecko() {
        let settings = [Setting::new(
            "WINEDLLOVERRIDES",
            "winemenubuilder.exe=;ddraw=n",
            Kind::Preference,
        )];
        let host = host(&[("WINEDLLOVERRIDES", "xinput1_4=b")]);
        let android = compose_on(host.clone(), &settings, true);
        assert_eq!(
            get(&android, "WINEDLLOVERRIDES"),
            Some(
                "winemenubuilder.exe=;ddraw=n;xinput1_4=n;nsiproxy.sys=d;mscoree=;mshtml=;\
                 winebus.sys=d;winehid.sys=d;hidclass.sys=d;winebth.sys=d"
            )
        );
        // The installers' and Blizzard's downloader's too: the first Wine
        // process of a prefix starts winedevice.exe with its environment.
        let install = [Setting::new(
            "WINEDLLOVERRIDES",
            "winemenubuilder.exe=;dsound=;winedbg.exe=d",
            Kind::Required,
        )];
        assert_eq!(
            get(&compose_on(Vec::new(), &install, true), "WINEDLLOVERRIDES"),
            Some(
                "winemenubuilder.exe=;dsound=;winedbg.exe=d;xinput1_4=n;nsiproxy.sys=d;\
                 mscoree=;mshtml=;winebus.sys=d;winehid.sys=d;hidclass.sys=d;winebth.sys=d"
            )
        );
        assert_eq!(
            get(&compose_on(Vec::new(), &install, false), "WINEDLLOVERRIDES"),
            Some("winemenubuilder.exe=;dsound=;winedbg.exe=d")
        );
        let desktop = compose_on(host, &settings, false);
        assert_eq!(
            get(&desktop, "WINEDLLOVERRIDES"),
            Some("winemenubuilder.exe=;ddraw=n")
        );
        // Without the Launcher's overrides, the Android ones alone; the
        // desktop sets none.
        assert_eq!(
            get(&compose_on(Vec::new(), &[], true), "WINEDLLOVERRIDES"),
            Some(
                "xinput1_4=n;nsiproxy.sys=d;mscoree=;mshtml=;winebus.sys=d;winehid.sys=d;\
                 hidclass.sys=d;winebth.sys=d"
            )
        );
        assert_eq!(
            get(&compose_on(Vec::new(), &[], false), "WINEDLLOVERRIDES"),
            None
        );
    }

    #[test]
    fn prime_offload_reaches_wine_whole() {
        let prime_run = [
            ("__NV_PRIME_RENDER_OFFLOAD", "1"),
            ("__VK_LAYER_NV_optimus", "NVIDIA_only"),
            ("__GLX_VENDOR_LIBRARY_NAME", "nvidia"),
            ("__GL_SYNC_TO_VBLANK", "0"),
            ("DRI_PRIME", "1"),
        ];
        let environment = compose(host(&prime_run), &[]);
        for (name, value) in prime_run {
            assert_eq!(get(&environment, name), Some(value), "{name}");
        }
    }

    #[test]
    fn gamescopes_vulkan_layer_stays_off_where_the_host_turned_it_off() {
        let set_by_host = compose(host(&[("DISABLE_GAMESCOPE_WSI", "1")]), &[]);
        assert_eq!(get(&set_by_host, "DISABLE_GAMESCOPE_WSI"), Some("1"));
    }
}
