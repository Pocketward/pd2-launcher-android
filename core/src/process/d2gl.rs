//! D2GL's configuration, `ProjectD2/d2gl.json`, before a Glide launch.
//!
//! D2GL sizes a full-screen game from the monitor (`GetMonitorInfo`): its
//! window and its render target, before it renders anything. The stored
//! `screen.window_size_*` is only the size of a windowed game, which D2GL
//! clamps to the screen when it loads the configuration and sets to the
//! monitor's size when it saves it in full screen
//! (<https://github.com/Project-Diablo-2/d2gl/blob/55148de0aec79d487f5bcef1842cfdc3effd2f62/d2gl/src/win32.cpp#L351-L396>).
//! So the Launcher never writes the window size.
//!
//! On the desktop the Launcher writes nothing into the configuration; it
//! only reports when D2GL has not written it yet ([`configuration_absent`]).
//!
//! On the Android host a handheld cannot toggle Fullscreen comfortably, so
//! when D2GL has not written its configuration yet the Launcher writes one
//! (fixed first-launch settings) before the first Glide launch. An existing
//! configuration gets the first launch's cursor and full-screen values
//! ([`ANDROID_SCREEN`]: an imported one keeps a desktop player's, often
//! windowed with a locked cursor, which the X screen cannot use) and
//! OpenGL 3.3 as the version D2GL asks for ([`gl_version`]), HD text off
//! for a copy in Chinese, Japanese or Korean ([`game_fonts`]), and the
//! app's graphics profile ([`GraphicsProfile`]) sets its few `graphics`
//! values when it is not the profile last applied ([`PROFILE_RECORD`]);
//! nothing else in it is touched (the player changes D2GL's graphics in the
//! game, Ctrl+O, and those changes stay until the profile changes). The
//! configuration is the player's, so a value that cannot be written is
//! left as it is and the launch continues.
//!
//! D2GL also loads the DLLs its configuration names (`load_dlls_early`,
//! `load_dlls_late`) into the game. On the Android host other apps the player
//! lets into the game folder can edit the configuration, so before D2GL can
//! start the Launcher sets both to the values PD2 ships ([`pin_dlls`]), and
//! the game does not start when it cannot.

use std::{
    ffi::OsStr,
    fs::File,
    io::{self, Read as _, Write as _},
    ops::Range,
    os::unix::fs::MetadataExt as _,
    path::Path,
};

use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags};
use serde_json::{Value, json};

use crate::{game_language::GameLanguage, safe_fs::Directory};

/// ProjectD2 below the game folder (the Managed one, or one used in place).
const PROJECT_D2: &str = "ProjectD2";
const CONFIG: &str = "d2gl.json";
const BACKUP: &str = "d2gl.json.bak";
/// The new configuration before its rename. Other apps on the Android host
/// cannot reach a name with the `.pd2launcher` prefix in the game folder.
const STAGED: &str = ".pd2launcher-d2gl.json.staged";
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";
/// The Launcher's own folder in ProjectD2, which also holds the update
/// transactions' state; the audit never counts it as a game file.
const LAUNCHER_FOLDER: &str = ".pd2launcher";
/// In [`LAUNCHER_FOLDER`]: the graphics profile last written into
/// `d2gl.json`, by name. It lives and goes with the game folder, so a new
/// installation or an imported one gets the profile at its first Play.
const PROFILE_RECORD: &str = "d2gl-profile";

/// The `screen` values the Android host writes into every configuration, a
/// new one and an existing one alike, because the game on the X screen
/// needs them. An imported configuration holds a desktop player's
/// choices, made for a real X server and window manager; on the desktop
/// they stand.
const ANDROID_SCREEN: [(&str, Value); 3] = [
    // Unlocked: D2GL's ClipCursor puts Wine in pointer-clipping mode, where
    // the X screen's XTEST pointer does not move the game's cursor and the
    // first click after a start is lost.
    ("unlock_cursor", Value::Bool(true)),
    // Full screen: Xvfb has no window manager, so Wine places a windowed
    // game as if one framed it, below a caption and partly off the X screen.
    ("window_fullscreen", Value::Bool(true)),
    // D2GL minimizes a full-screen game that loses focus when this is on,
    // and the X screen has no taskbar or window manager to bring it back.
    ("auto_minimize", Value::Bool(false)),
];

/// The `other` values for the OpenGL version D2GL asks for, 3.3, which the
/// Android host writes before each Glide start. D2GL tries 4.6 down to 3.3,
/// keeps the first that works, saves it here, and at its next start skips
/// each version whose major or minor is above the saved one: so a saved 4.0
/// to 4.2 never asks for 3.3. The game's shaders are GLSL 330 and, on the
/// 0.4.0 line of the Android app, every route gives OpenGL 3.3
/// (`android/userland/userland.env.in`), so 3.3 is the version on each of
/// them. These two keys are the one change of D2GL's own values the owner
/// approved (2026-11-04).
fn gl_version() -> [(&'static str, Value); 2] {
    [
        ("gl_ver_major", Value::from(3)),
        ("gl_ver_minor", Value::from(3)),
    ]
}

/// The `other` values naming the DLLs D2GL loads, as PD2 ships them: the
/// defaults of PD2's D2GL build, which it writes into a new configuration.
fn shipped_dlls() -> [(&'static str, Value); 2] {
    [
        ("load_dlls_early", Value::from("")),
        ("load_dlls_late", Value::from("SGD2FreeRes.dll")),
    ]
}

/// The `features` value for a copy in Chinese, Japanese or Korean
/// ([`GameLanguage::needs_game_fonts`]), which the Android host writes before
/// each Glide start: HD text off, so the game draws its text with its own
/// fonts. PD2's `d2gl.mpq` has no HD text glyphs for these languages, and
/// with HD text on every letter is an empty box (issue 19). An English copy
/// keeps the player's value.
fn game_fonts() -> [(&'static str, Value); 1] {
    [("hd_text", Value::Bool(false))]
}

/// D2GL's graphics profile, which the Android app sends with each Play (its
/// Settings → Game → Graphics profile, Automatic already resolved). The
/// desktop sends none.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphicsProfile {
    /// Project Diablo 2's own D2GL settings: the `advanced-aa` upscaling
    /// shader with sharpening and bloom.
    Default,
    /// D2GL's advanced settings tuned down for slower GPUs: the plain
    /// bilinear upscale, no sharpening, no bloom.
    Performance,
}

impl GraphicsProfile {
    /// The `graphics` values the profile sets; every other key stays.
    fn values(self) -> [(&'static str, Value); 3] {
        let (shader, effects) = match self {
            Self::Default => ("anti-aliasing\\advanced-aa.slangp", true),
            Self::Performance => ("bilinear.slangp", false),
        };
        [
            ("shader_preset", Value::from(shader)),
            ("sharpen", Value::from(effects)),
            ("bloom", Value::from(effects)),
        ]
    }

    /// Its name in [`PROFILE_RECORD`] and the log.
    fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Performance => "performance",
        }
    }
}

/// What a launch did to `ProjectD2/d2gl.json` before the game started.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum D2glSetup {
    /// The renderer does not use D2GL.
    NotRequested,
    /// Nothing was written: the desktop, or a configuration that already has
    /// the Android host's values.
    Unchanged,
    /// D2GL has not written its configuration yet: this is the game's first
    /// launch with it (the desktop; the Android host writes one).
    ConfigurationAbsent,
    /// The Android host wrote the configuration, or its screen values or
    /// graphics profile.
    Written,
    /// The Android host could not read the configuration or set any of its
    /// values in it, so it stays as it is.
    Skipped,
}

/// Writes the Android host's values into `d2gl.json` ([`ANDROID_SCREEN`] and
/// the Play's graphics `profile`), or a first-launch configuration when D2GL
/// has not written one. Only the Android host calls it.
pub(super) fn configure(game: &Directory, profile: Option<GraphicsProfile>) -> D2glSetup {
    configure_config(game, profile).unwrap_or(D2glSetup::Skipped)
}

/// Reports whether D2GL has not yet written its configuration, without
/// reading or creating it.
pub(super) fn configuration_absent(game: &Directory) -> bool {
    game.open_directory(Path::new(PROJECT_D2))
        .and_then(|project| project.open_file(Path::new(CONFIG)))
        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
}

/// Sets `d2gl.json`'s DLLs to [`shipped_dlls`]'s values, every other byte
/// as it is; the Android host calls it before anything that can start D2GL.
///
/// The configuration is written anew even when it has them already, so a
/// descriptor another app opened before no longer reaches the file D2GL
/// reads. One that cannot be edited (not JSON, over the size limit, or the
/// values not uniquely addressable) is replaced with its backup,
/// [`BACKUP`], with these values, as D2GL itself falls back to the backup
/// for a configuration it cannot parse (an unchecked backup would name any
/// DLL too); it is removed when the backup cannot be edited either, and
/// without a configuration D2GL starts from its defaults, which are these
/// values. Other spellings of the name go first ([`remove_spellings`]).
///
/// An error means the configuration may still name other DLLs, so D2GL
/// must not start.
pub(crate) fn pin_dlls(game: &Directory) -> io::Result<()> {
    let project = game.open_directory(Path::new(PROJECT_D2))?;
    remove_spellings(&project)?;
    let config = match project.open_file(Path::new(CONFIG)) {
        Ok(config) => config,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let edited = read_limited(&config).and_then(|raw| {
        let pinned = pinned(&raw)?;
        Ok((pinned != raw, pinned))
    });
    match edited {
        Ok((changed, bytes)) => {
            if changed {
                eprintln!("pd2-launcher: d2gl.json names only the DLLs PD2 ships");
            }
            replace(&project, &config, &bytes)
        }
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            let backup = project
                .open_file(Path::new(BACKUP))
                .and_then(|backup| read_limited(&backup))
                .and_then(|raw| pinned(&raw));
            if let Ok(bytes) = backup {
                eprintln!("pd2-launcher: d2gl.json is its backup with PD2's DLLs: {error}");
                replace(&project, &config, &bytes)
            } else {
                eprintln!(
                    "pd2-launcher: d2gl.json removed, D2GL starts from its defaults: {error}"
                );
                remove(&project, &config)
            }
        }
        Err(error) => Err(error),
    }
}

/// `raw` with [`shipped_dlls`]'s values. serde_json and D2GL's JSON library
/// both keep the last of repeated keys, so the values checked here are the
/// ones D2GL reads.
fn pinned(raw: &[u8]) -> io::Result<Vec<u8>> {
    Ok(set_values(raw, "other", &shipped_dlls())?.unwrap_or_else(|| raw.to_vec()))
}

/// Removes the names in ProjectD2 other than `d2gl.json` that Wine takes for
/// it, the same letters in another case: Wine opens one of them when
/// `d2gl.json` itself is missing. A folder among them is an error.
fn remove_spellings(project: &Directory) -> io::Result<()> {
    let config = wine_upper(CONFIG);
    let directory = project.try_clone_file()?;
    for name in project.names()? {
        if name == CONFIG || name.to_str().map(wine_upper).as_ref() != Some(&config) {
            continue;
        }
        rustix::fs::unlinkat(&directory, name.as_os_str(), AtFlags::empty())?;
        eprintln!(
            "pd2-launcher: ProjectD2/{} removed, which Wine would read as d2gl.json",
            name.to_string_lossy()
        );
    }
    Ok(())
}

/// `name` as Wine compares names that differ only in case: each character
/// upper-cased on its own (one whose capital takes more than one character
/// stays as it is). This matches Wine for an ASCII name like `d2gl.json`.
fn wine_upper(name: &str) -> String {
    name.chars()
        .map(|character| {
            let mut upper = character.to_uppercase();
            match (upper.next(), upper.next()) {
                (Some(capital), None) => capital,
                _ => character,
            }
        })
        .collect()
}

/// Reads the configuration, refusing one over [`MAX_CONFIG_BYTES`].
fn read_limited(config: &File) -> io::Result<Vec<u8>> {
    if config.metadata()?.len() > MAX_CONFIG_BYTES {
        return Err(invalid("D2GL configuration exceeded its size limit"));
    }
    let mut raw = Vec::new();
    config.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut raw)?;
    if raw.len() as u64 > MAX_CONFIG_BYTES {
        return Err(invalid("D2GL configuration exceeded its size limit"));
    }
    Ok(raw)
}

fn configure_config(game: &Directory, profile: Option<GraphicsProfile>) -> io::Result<D2glSetup> {
    let project = game.open_directory(Path::new(PROJECT_D2))?;
    let language = GameLanguage::of(game);
    if let Some(language) = language.filter(|language| !language.is_english()) {
        eprintln!(
            "pd2-launcher: the game is in {} ({}); PD2 needs an English Diablo II",
            language.name(),
            language.code()
        );
    }
    let needs_game_fonts = language.is_some_and(GameLanguage::needs_game_fonts);
    let config = match project.open_file(Path::new(CONFIG)) {
        Ok(config) => config,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let first = profile.unwrap_or(GraphicsProfile::Default);
            let mut bytes = first_launch_config(first);
            if needs_game_fonts {
                bytes = set_values(&bytes, "features", &game_fonts())?.unwrap_or(bytes);
                eprintln!(
                    "pd2-launcher: d2gl.json has HD text off, the game's own fonts show its language"
                );
            }
            create(&project, &bytes)?;
            // What was written, so an older record never stands for it.
            record_profile(&project, first);
            return Ok(D2glSetup::Written);
        }
        Err(error) => return Err(error),
    };
    let raw = read_limited(&config)?;
    // A profile is written only when it is not the one last applied, so the
    // player's own changes in the game stay until the profile changes.
    let changed = profile.filter(|profile| applied_profile(&project) != Some(*profile));
    // An existing configuration gets the first launch's cursor and
    // full-screen values too: an imported one keeps a desktop player's, with
    // which the game loses the first click after each start and runs
    // windowed partly off the X screen. Then OpenGL 3.3 ([`gl_version`]).
    // These values and the profile stand alone: one that cannot address the
    // configuration never keeps the others from being written. A profile
    // that cannot be written is not recorded, so the next Play tries again.
    let screened = set_values(&raw, "screen", &ANDROID_SCREEN).inspect_err(|error| {
        eprintln!("pd2-launcher: d2gl.json keeps its cursor and full-screen settings: {error}");
    });
    let base = match &screened {
        Ok(Some(screened)) => screened.as_slice(),
        _ => &raw,
    };
    let versioned = set_values(base, "other", &gl_version()).inspect_err(|error| {
        eprintln!("pd2-launcher: d2gl.json keeps its OpenGL version: {error}");
    });
    let base = match &versioned {
        Ok(Some(versioned)) => versioned.as_slice(),
        _ => base,
    };
    let fonted = needs_game_fonts.then(|| {
        set_values(base, "features", &game_fonts()).inspect_err(|error| {
            eprintln!("pd2-launcher: d2gl.json keeps its HD text: {error}");
        })
    });
    let base = match &fonted {
        Some(Ok(Some(fonted))) => fonted.as_slice(),
        _ => base,
    };
    let profiled = changed.and_then(|profile| match profiled_config(base, profile) {
        Ok(profiled) => Some((profile, profiled)),
        Err(error) => {
            eprintln!(
                "pd2-launcher: d2gl.json keeps its graphics, the {} profile could not be written: {error}",
                profile.name()
            );
            None
        }
    });
    let (applied, profiled) = profiled.unzip();
    let screen_written = matches!(screened, Ok(Some(_)));
    let version_written = matches!(versioned, Ok(Some(_)));
    let fonts_written = matches!(fonted, Some(Ok(Some(_))));
    // Each edit was made on the bytes of the edits before it.
    // A screen error with no profile written is reported (the caller shows
    // DisplayConfigSkipped), but only after the version is written.
    let skipped = match &screened {
        Err(error) if applied.is_none() => Some(io::Error::new(error.kind(), error.to_string())),
        _ => None,
    };
    let written = profiled
        .flatten()
        .or(fonted.and_then(|fonted| fonted.ok().flatten()))
        .or(versioned.ok().flatten())
        .or(screened.ok().flatten());
    if let Some(bytes) = &written {
        replace(&project, &config, bytes)?;
    }
    if screen_written {
        eprintln!("pd2-launcher: d2gl.json is full screen with the cursor unlocked");
    }
    if version_written {
        eprintln!(
            "pd2-launcher: d2gl.json asks for OpenGL 3.3, was {}",
            saved_gl_version(&raw)
        );
    }
    if fonts_written {
        eprintln!(
            "pd2-launcher: d2gl.json has HD text off, the game's own fonts show its language"
        );
    }
    if let Some(error) = skipped {
        return Err(error);
    }
    if let Some(profile) = applied {
        record_profile(&project, profile);
    }
    Ok(match written {
        None => D2glSetup::Unchanged,
        Some(_) => D2glSetup::Written,
    })
}

/// The OpenGL version that `raw` saves, for the log: `major.minor`, or
/// `not set` when it has neither key (D2GL then takes its default, 4.6), or
/// `major 4, minor not set` when it has one key only.
fn saved_gl_version(raw: &[u8]) -> String {
    let config = serde_json::from_slice::<Value>(raw.strip_prefix(UTF8_BOM).unwrap_or(raw)).ok();
    let part = |key: &str| {
        config
            .as_ref()
            .and_then(|config| config.get("other")?.get(key).cloned())
    };
    match (part("gl_ver_major"), part("gl_ver_minor")) {
        (None, None) => "not set".to_owned(),
        (major, minor) => {
            let shown = |part: Option<Value>| {
                part.map_or_else(|| "not set".to_owned(), |value| value.to_string())
            };
            match (&major, &minor) {
                (Some(major), Some(minor)) => format!("{major}.{minor}"),
                _ => format!("major {}, minor {}", shown(major), shown(minor)),
            }
        }
    }
}

/// The profile [`PROFILE_RECORD`] names, or `None` when there is none (no
/// Play has applied one to this game folder) or it cannot be read.
fn applied_profile(project: &Directory) -> Option<GraphicsProfile> {
    let record = project
        .open_directory(Path::new(LAUNCHER_FOLDER))
        .and_then(|folder| folder.open_file(Path::new(PROFILE_RECORD)))
        .ok()?;
    let mut name = String::new();
    record.take(64).read_to_string(&mut name).ok()?;
    [GraphicsProfile::Default, GraphicsProfile::Performance]
        .into_iter()
        .find(|profile| profile.name() == name.trim())
}

/// Records `profile` as written into `d2gl.json`. A record that fails only
/// means the next Play writes the profile's values again.
fn record_profile(project: &Directory, profile: GraphicsProfile) {
    let recorded = project
        .ensure_directory(Path::new(LAUNCHER_FOLDER))
        .and_then(|folder| {
            folder.atomic_write(
                OsStr::new(PROFILE_RECORD),
                format!("{}\n", profile.name()).as_bytes(),
            )
        });
    match recorded {
        Ok(()) => eprintln!(
            "pd2-launcher: d2gl.json has the {} graphics profile",
            profile.name()
        ),
        Err(error) => eprintln!(
            "pd2-launcher: d2gl.json has the {} graphics profile, not recorded: {error}",
            profile.name()
        ),
    }
}

/// Returns the configuration with `profile`'s `graphics` values set, or
/// `None` when it has them already ([`set_values`]).
fn profiled_config(raw: &[u8], profile: GraphicsProfile) -> io::Result<Option<Vec<u8>>> {
    set_values(raw, "graphics", &profile.values())
}

/// Returns the configuration with `values` set in its `section` object, or
/// `None` when it has them already. Present values are replaced in place and
/// missing ones inserted at the start of the section (or of the
/// configuration, in a new section), so every other byte stays as it is.
fn set_values(raw: &[u8], section: &str, values: &[(&str, Value)]) -> io::Result<Option<Vec<u8>>> {
    let (bom, body) = match raw.strip_prefix(UTF8_BOM) {
        Some(body) => (UTF8_BOM, body),
        None => (&[][..], raw),
    };
    let original: Value =
        serde_json::from_slice(body).map_err(|_| invalid("D2GL configuration was not JSON"))?;
    let mut expected = original.clone();
    let object = expected
        .as_object_mut()
        .ok_or_else(|| invalid("D2GL configuration was not an object"))?
        .entry(section)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| invalid(format!("D2GL {section} was not an object")))?;
    for (key, value) in values {
        object.insert((*key).to_owned(), value.clone());
    }
    if expected == original {
        return Ok(None);
    }

    let unaddressable = || {
        invalid(format!(
            "D2GL {section} values were not uniquely addressable"
        ))
    };
    let mut scanner = Scanner { bytes: body, at: 0 };
    let top = scanner.object_members().ok_or_else(unaddressable)?;
    // (span, text): spans are disjoint; an insertion is an empty span.
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let member = |members: &[(Range<usize>, Range<usize>)], name: &[u8]| {
        let mut found = members
            .iter()
            .filter(|(key, _)| &body[key.clone()] == name)
            .map(|(_, value)| value.clone());
        match (found.next(), found.next()) {
            (None, _) => Ok(None),
            (Some(value), None) => Ok(Some(value)),
            (Some(_), Some(_)) => Err(unaddressable()),
        }
    };
    let rendered = |key: &str, value: &Value| format!("\"{key}\": {value}");
    match member(&top, section.as_bytes())? {
        Some(span) => {
            let mut scanner = Scanner {
                bytes: body,
                at: span.start,
            };
            let members = scanner.object_members().ok_or_else(unaddressable)?;
            let mut missing = Vec::new();
            for (key, value) in values {
                match member(&members, key.as_bytes())? {
                    Some(span) => edits.push((span, value.to_string())),
                    None => missing.push(rendered(key, value)),
                }
            }
            if !missing.is_empty() {
                let mut text = missing.join(", ");
                if !members.is_empty() {
                    text.push_str(", ");
                }
                let open = span.start + 1;
                edits.push((open..open, text));
            }
        }
        None => {
            let open = body
                .iter()
                .position(|byte| *byte == b'{')
                .ok_or_else(unaddressable)?
                + 1;
            let members = values
                .iter()
                .map(|(key, value)| rendered(key, value))
                .collect::<Vec<_>>()
                .join(", ");
            let mut text = format!("\"{section}\": {{{members}}}");
            if !top.is_empty() {
                text.push_str(", ");
            }
            edits.push((open..open, text));
        }
    }
    edits.sort_by_key(|(span, _)| span.start);
    let mut edited = bom.to_vec();
    let mut cursor = 0;
    for (span, text) in edits {
        edited.extend_from_slice(&body[cursor..span.start]);
        edited.extend_from_slice(text.as_bytes());
        cursor = span.end;
    }
    edited.extend_from_slice(&body[cursor..]);

    // The textual splice must change exactly the given values.
    let observed: Value = serde_json::from_slice(&edited[bom.len()..])
        .map_err(|_| invalid("edited D2GL configuration was not JSON"))?;
    if observed != expected {
        return Err(invalid("edited D2GL configuration changed other values"));
    }
    Ok(Some(edited))
}

/// Minimal JSON span scanner. Values are validated by `serde_json` first;
/// this only recovers raw member key/value spans so formatting is preserved.
struct Scanner<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Scanner<'_> {
    fn skip_whitespace(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, expected: u8) -> Option<()> {
        self.skip_whitespace();
        (self.bytes.get(self.at) == Some(&expected)).then(|| self.at += 1)
    }

    /// Returns the raw span between the quotes.
    fn string(&mut self) -> Option<Range<usize>> {
        self.eat(b'"')?;
        let start = self.at;
        loop {
            match *self.bytes.get(self.at)? {
                b'"' => {
                    let span = start..self.at;
                    self.at += 1;
                    return Some(span);
                }
                b'\\' => self.at += 2,
                _ => self.at += 1,
            }
        }
    }

    fn value(&mut self) -> Option<Range<usize>> {
        self.skip_whitespace();
        let start = self.at;
        match *self.bytes.get(self.at)? {
            b'"' => {
                self.string()?;
            }
            b'{' | b'[' => {
                let mut depth = 0_usize;
                loop {
                    match *self.bytes.get(self.at)? {
                        b'"' => {
                            self.string()?;
                            continue;
                        }
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth = depth.checked_sub(1)?;
                            if depth == 0 {
                                self.at += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    self.at += 1;
                }
            }
            _ => {
                while self.bytes.get(self.at).is_some_and(|byte| {
                    !matches!(byte, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
                }) {
                    self.at += 1;
                }
            }
        }
        (self.at > start).then_some(start..self.at)
    }

    fn object_members(&mut self) -> Option<Vec<(Range<usize>, Range<usize>)>> {
        self.eat(b'{')?;
        let mut members = Vec::new();
        if self.eat(b'}').is_some() {
            return Some(members);
        }
        loop {
            let key = self.string()?;
            self.eat(b':')?;
            let value = self.value()?;
            members.push((key, value));
            if self.eat(b',').is_some() {
                continue;
            }
            self.eat(b'}')?;
            return Some(members);
        }
    }
}

/// Replaces `d2gl.json` by rename from a staged sibling, only while the name
/// still denotes the descriptor that was read. No lookup follows a symlink.
fn replace(project: &Directory, original: &File, bytes: &[u8]) -> io::Result<()> {
    let directory = project.try_clone_file()?;
    let read = original.metadata()?;
    match rustix::fs::unlinkat(&directory, STAGED, AtFlags::empty()) {
        Ok(()) => {}
        Err(error) if error == rustix::io::Errno::NOENT => {}
        Err(error) => return Err(error.into()),
    }
    let staged = File::from(rustix::fs::openat(
        &directory,
        STAGED,
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(read.mode() & 0o777),
    )?);
    let result = (|| {
        (&staged).write_all(bytes)?;
        staged.sync_all()?;
        let current = rustix::fs::statat(&directory, CONFIG, AtFlags::SYMLINK_NOFOLLOW)?;
        if current.st_dev != read.dev() || current.st_ino != read.ino() {
            return Err(invalid("D2GL configuration changed while it was edited"));
        }
        rustix::fs::renameat(&directory, STAGED, &directory, CONFIG)?;
        directory.sync_all()
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(&directory, STAGED, AtFlags::empty());
    }
    result
}

/// Removes `d2gl.json`, only while the name still denotes the descriptor
/// that was read. No lookup follows a symlink.
fn remove(project: &Directory, original: &File) -> io::Result<()> {
    let directory = project.try_clone_file()?;
    let read = original.metadata()?;
    let current = rustix::fs::statat(&directory, CONFIG, AtFlags::SYMLINK_NOFOLLOW)?;
    if current.st_dev != read.dev() || current.st_ino != read.ino() {
        return Err(io::Error::other(
            "D2GL configuration changed while it was read",
        ));
    }
    rustix::fs::unlinkat(&directory, CONFIG, AtFlags::empty())?;
    directory.sync_all()
}

/// The Android host's first-launch D2GL settings: D2GL's corner mini map
/// off, and [`ANDROID_SCREEN`]'s, [`shipped_dlls`]', [`gl_version`]'s and
/// `profile`'s values.
/// It has no window size: D2GL sizes the full-screen game from the X screen
/// and takes its own default for a windowed one. The player changes these
/// settings in the game (Ctrl+O); [`ANDROID_SCREEN`]'s are set again at the
/// next Play.
fn first_launch_config(profile: GraphicsProfile) -> Vec<u8> {
    let mut config = json!({
        "screen": {
            "window_centered": true,
            "window_position_x": 0,
            "window_position_y": 0,
            // These three keep their place here; their values are
            // ANDROID_SCREEN's, set below.
            "unlock_cursor": true,
            "window_fullscreen": true,
            "window_vsync": true,
            "foreground_fps_active": true,
            "foreground_fps_value": 60,
            "background_fps_active": true,
            "background_fps_value": 25,
            "auto_minimize": false,
            "window_dark_mode": true
        },
        "graphics": {
            "shader_preset": "anti-aliasing\\advanced-aa.slangp",
            "sharpen": true,
            "sharpen_strength": 1.2000000476837158_f64,
            "sharpen_clamp": 0.800000011920929_f64,
            "sharpen_radius": 0.4000000059604645_f64,
            "fxaa": false,
            "fxaa_preset": 1,
            "lut": 0,
            "bloom": true,
            "bloom_exposure": 1.0,
            "bloom_gamma": 0.75,
            "stretched_horizontal": false,
            "stretched_vertical": false
        },
        "features": {
            "hd_cursor": true,
            "hd_text": true,
            "hd_text_scale": 1.0,
            "mini_map": false,
            "mini_map_text_over": true,
            "mini_map_width": 200,
            "mini_map_height": 140,
            "motion_prediction": true,
            "skip_intro": false,
            "no_pickup": false,
            "show_item_quantity": true,
            "show_fps": false
        },
        "other": {
            // These two keep their place here; their values are
            // gl_version()'s, set below.
            "gl_ver_major": 0,
            "gl_ver_minor": 0,
            "use_compute_shader": false,
            "frame_latency": 1,
            // These two keep their place here; their values are
            // shipped_dlls()'s, set below.
            "load_dlls_early": "",
            "load_dlls_late": ""
        }
    });
    for (key, value) in ANDROID_SCREEN {
        config["screen"][key] = value;
    }
    for (key, value) in shipped_dlls().into_iter().chain(gl_version()) {
        config["other"][key] = value;
    }
    for (key, value) in profile.values() {
        config["graphics"][key] = value;
    }
    serde_json::to_vec_pretty(&config).expect("a JSON value serializes")
}

/// Writes a new `d2gl.json` from a staged sibling, never replacing a file
/// that appeared meanwhile. No lookup follows a symlink.
fn create(project: &Directory, bytes: &[u8]) -> io::Result<()> {
    let directory = project.try_clone_file()?;
    match rustix::fs::unlinkat(&directory, STAGED, AtFlags::empty()) {
        Ok(()) => {}
        Err(error) if error == rustix::io::Errno::NOENT => {}
        Err(error) => return Err(error.into()),
    }
    let staged = File::from(rustix::fs::openat(
        &directory,
        STAGED,
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o644),
    )?);
    let result = (|| {
        (&staged).write_all(bytes)?;
        staged.sync_all()?;
        rustix::fs::renameat_with(
            &directory,
            STAGED,
            &directory,
            CONFIG,
            RenameFlags::NOREPLACE,
        )?;
        directory.sync_all()
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(&directory, STAGED, AtFlags::empty());
    }
    result
}

fn invalid(message: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::symlink};

    use super::*;

    const DEFAULT_CONFIG: &str = r#"{
    "screen": {
        "window_size_width": 800,
        "window_size_height": 600,
        "window_centered": true,
        "window_fullscreen": true,
        "foreground_fps_value": 60
    },
    "graphics": {
        "shader_preset": "anti-aliasing\\advanced-aa.slangp",
        "sharpen_strength": 1.2000000476837158,
        "window_size_width": 800
    },
    "other": {
        "load_dlls_late": "SGD2FreeRes.dll"
    }
}"#;

    fn managed_root(config: Option<&[u8]>) -> (tempfile::TempDir, Directory) {
        let temporary = tempfile::tempdir().expect("temporary managed root");
        let project = temporary.path().join(PROJECT_D2);
        fs::create_dir_all(&project).expect("ProjectD2");
        if let Some(config) = config {
            fs::write(project.join(CONFIG), config).expect("d2gl.json");
        }
        let root = Directory::open(temporary.path()).expect("retained root");
        (temporary, root)
    }

    fn read_config(root: &tempfile::TempDir) -> Vec<u8> {
        fs::read(root.path().join(PROJECT_D2).join(CONFIG)).expect("read d2gl.json")
    }

    /// A configuration with the Android host's screen values already, so
    /// only its `other` section can change.
    fn screened_config(other: &str) -> String {
        format!(
            "{{\n    \"screen\": {{\n        \"unlock_cursor\": true,\n        \"window_fullscreen\": true,\n        \"auto_minimize\": false\n    }},\n    \"other\": {{{other}}}\n}}"
        )
    }

    #[test]
    fn the_android_host_asks_d2gl_for_opengl_3_3_and_every_other_byte_stays() {
        // D2GL tries only the versions up to the saved one, so a saved 4.0
        // to 4.2 never asks for 3.3; 3.3 on every route.
        for (major, minor) in [(4, 0), (4, 2), (4, 6)] {
            let other = |major, minor| {
                format!(
                    "\n        \"gl_ver_major\": {major},\n        \"gl_ver_minor\": {minor},\n        \"use_compute_shader\": false,\n        \"frame_latency\": 1,\n        \"load_dlls_early\": \"\",\n        \"load_dlls_late\": \"SGD2FreeRes.dll\"\n    "
                )
            };
            let saved = screened_config(&other(major, minor));
            assert_eq!(
                saved_gl_version(saved.as_bytes()),
                format!("{major}.{minor}")
            );
            let (temporary, root) = managed_root(Some(saved.as_bytes()));
            assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Written);
            assert_eq!(
                String::from_utf8(read_config(&temporary)).unwrap(),
                screened_config(&other(3, 3))
            );
        }
    }

    /// Puts a `d2data.mpq` with language `id` into the game folder.
    fn game_in_language(root: &tempfile::TempDir, id: u8) {
        let archive =
            crate::blizzard::build_test_archive(&[("data\\local\\use", &[id], false)], 16, 3);
        fs::write(root.path().join("d2data.mpq"), archive).expect("d2data.mpq");
    }

    #[test]
    fn a_chinese_japanese_or_korean_copy_gets_hd_text_off() {
        let full = |hd_text: bool| {
            format!(
                "{{\n    \"screen\": {{\n        \"unlock_cursor\": true,\n        \"window_fullscreen\": true,\n        \"auto_minimize\": false\n    }},\n    \"features\": {{\n        \"hd_cursor\": true,\n        \"hd_text\": {hd_text}\n    }},\n    \"other\": {{\"gl_ver_major\": 3, \"gl_ver_minor\": 3}}\n}}"
            )
        };
        for id in [6, 7, 8, 9] {
            let (temporary, root) = managed_root(Some(full(true).as_bytes()));
            game_in_language(&temporary, id);
            assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Written);
            assert_eq!(
                String::from_utf8(read_config(&temporary)).unwrap(),
                full(false)
            );
            assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Unchanged);
        }
        // English, another language, or none found: the player's value stays.
        for id in [Some(0), Some(2), Some(11), None] {
            let (temporary, root) = managed_root(Some(full(true).as_bytes()));
            if let Some(id) = id {
                game_in_language(&temporary, id);
            }
            assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Unchanged);
            assert_eq!(
                String::from_utf8(read_config(&temporary)).unwrap(),
                full(true)
            );
        }
    }

    #[test]
    fn a_chinese_copy_keeps_hd_text_off_with_a_new_graphics_profile() {
        let (temporary, root) = managed_root(Some(
            br#"{"screen": {"unlock_cursor": true, "window_fullscreen": true, "auto_minimize": false}, "features": {"hd_text": true}, "other": {"gl_ver_major": 3, "gl_ver_minor": 3}}"#,
        ));
        game_in_language(&temporary, 9);
        assert_eq!(
            configure_config(&root, Some(GraphicsProfile::Performance)).unwrap(),
            D2glSetup::Written
        );
        let written: Value = serde_json::from_slice(&read_config(&temporary)).unwrap();
        assert_eq!(written["features"]["hd_text"], false);
        for (key, value) in GraphicsProfile::Performance.values() {
            assert_eq!(written["graphics"][key], value);
        }
    }

    #[test]
    fn a_chinese_copy_s_first_configuration_has_hd_text_off() {
        let (temporary, root) = managed_root(None);
        game_in_language(&temporary, 9);
        assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Written);
        let written: Value = serde_json::from_slice(&read_config(&temporary)).unwrap();
        let mut expected: Value =
            serde_json::from_slice(&first_launch_config(GraphicsProfile::Default)).unwrap();
        expected["features"]["hd_text"] = Value::Bool(false);
        assert_eq!(written, expected);
    }

    #[test]
    fn a_saved_opengl_3_3_is_not_written_again() {
        let saved = screened_config("\"gl_ver_major\": 3, \"gl_ver_minor\": 3");
        let (temporary, root) = managed_root(Some(saved.as_bytes()));
        let path = temporary.path().join(PROJECT_D2).join(CONFIG);
        let inode = fs::metadata(&path).unwrap().ino();
        assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Unchanged);
        assert_eq!(read_config(&temporary), saved.as_bytes());
        // Not replaced by a rename: the same file.
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    }

    #[test]
    fn missing_opengl_keys_are_inserted_where_d2gl_reads_them() {
        // Without the keys D2GL takes its default, 4.6.
        let saved = screened_config("\"use_compute_shader\": false");
        assert_eq!(saved_gl_version(saved.as_bytes()), "not set");
        let (temporary, root) = managed_root(Some(saved.as_bytes()));
        assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Written);
        assert_eq!(
            String::from_utf8(read_config(&temporary)).unwrap(),
            screened_config(
                "\"gl_ver_major\": 3, \"gl_ver_minor\": 3, \"use_compute_shader\": false"
            )
        );

        // One key only: the other is inserted, the present one replaced.
        let saved = screened_config("\"gl_ver_minor\": 2");
        assert_eq!(saved_gl_version(saved.as_bytes()), "major not set, minor 2");
        let (temporary, root) = managed_root(Some(saved.as_bytes()));
        assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Written);
        assert_eq!(
            String::from_utf8(read_config(&temporary)).unwrap(),
            screened_config("\"gl_ver_major\": 3, \"gl_ver_minor\": 3")
        );

        // No `other` section: a new one, at the start.
        let saved = r#"{"screen": {"unlock_cursor": true, "window_fullscreen": true, "auto_minimize": false}}"#;
        let (temporary, root) = managed_root(Some(saved.as_bytes()));
        assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Written);
        assert_eq!(
            String::from_utf8(read_config(&temporary)).unwrap(),
            saved.replacen(
                '{',
                "{\"other\": {\"gl_ver_major\": 3, \"gl_ver_minor\": 3}, ",
                1
            )
        );
    }

    #[test]
    fn the_first_launch_configuration_asks_for_opengl_3_3() {
        for profile in [GraphicsProfile::Default, GraphicsProfile::Performance] {
            let written: Value = serde_json::from_slice(&first_launch_config(profile)).unwrap();
            assert_eq!(written["other"]["gl_ver_major"], 3);
            assert_eq!(written["other"]["gl_ver_minor"], 3);
        }
        let (temporary, root) = managed_root(None);
        assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Written);
        assert_eq!(saved_gl_version(&read_config(&temporary)), "3.3");
    }

    #[test]
    fn the_opengl_version_is_written_when_the_screen_values_cannot_be() {
        // A `screen` that is not an object keeps its value; the version
        // stands alone. The Play still reports the skipped screen values
        // (DisplayConfigSkipped), as before 0.1.14, on every Play.
        let saved = r#"{"screen": 1, "other": {"gl_ver_major": 4, "gl_ver_minor": 6}}"#;
        let (temporary, root) = managed_root(Some(saved.as_bytes()));
        assert_eq!(configure(&root, None), D2glSetup::Skipped);
        let written = saved.replace("4, \"gl_ver_minor\": 6", "3, \"gl_ver_minor\": 3");
        assert_eq!(String::from_utf8(read_config(&temporary)).unwrap(), written);
        assert_eq!(configure(&root, None), D2glSetup::Skipped);
        assert_eq!(String::from_utf8(read_config(&temporary)).unwrap(), written);
    }

    #[test]
    fn a_saved_opengl_version_with_one_key_is_logged_readably() {
        let one = |other: &str| format!(r#"{{"other": {{{other}}}}}"#);
        assert_eq!(
            saved_gl_version(one("\"gl_ver_major\": 4").as_bytes()),
            "major 4, minor not set"
        );
        assert_eq!(
            saved_gl_version(one("\"gl_ver_minor\": 2").as_bytes()),
            "major not set, minor 2"
        );
        assert_eq!(saved_gl_version(one("").as_bytes()), "not set");
    }

    #[test]
    fn the_android_host_writes_the_first_launch_configuration() {
        let (temporary, root) = managed_root(None);
        assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Written);
        let project = temporary.path().join(PROJECT_D2);
        let written: Value = serde_json::from_slice(&read_config(&temporary)).unwrap();
        // No window size: D2GL sizes the full-screen game from the X screen.
        assert_eq!(written["screen"].get("window_size_width"), None);
        assert_eq!(written["screen"].get("window_size_height"), None);
        assert_eq!(written["screen"]["window_fullscreen"], true);
        // A locked cursor puts Wine in pointer-clipping mode, where the
        // X screen's pointer stops reaching the game and the first click
        // is lost.
        assert_eq!(written["screen"]["unlock_cursor"], true);
        assert_eq!(
            written["graphics"]["shader_preset"],
            "anti-aliasing\\advanced-aa.slangp"
        );
        assert_eq!(written["other"]["load_dlls_late"], "SGD2FreeRes.dll");
        assert!(!project.join(STAGED).exists());

        // The next Play finds nothing to change.
        let before = read_config(&temporary);
        assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Unchanged);
        assert_eq!(read_config(&temporary), before);
    }

    #[test]
    fn the_android_host_never_changes_the_window_size() {
        // D2GL sizes a full-screen game from the monitor; the stored size is
        // only the windowed one, the player's or D2GL's own, whatever it is.
        for (width, height) in [(800, 600), (1068, 480), (1920, 1080), (3840, 2160)] {
            for profile in [None, Some(GraphicsProfile::Performance)] {
                let config = IMPORTED_CONFIG
                    .replacen("3840", &width.to_string(), 1)
                    .replacen("2160", &height.to_string(), 1);
                let (temporary, root) = managed_root(Some(config.as_bytes()));
                assert_eq!(
                    configure_config(&root, profile).unwrap(),
                    D2glSetup::Written
                );
                let written = String::from_utf8(read_config(&temporary)).unwrap();
                for size in [
                    format!("\"window_size_width\": {width},"),
                    format!("\"window_size_height\": {height},"),
                ] {
                    assert!(written.contains(&size), "{size} in {written}");
                }
                let written: Value = serde_json::from_str(&written).unwrap();
                assert_eq!(written["screen"]["window_size_width"], width);
                assert_eq!(written["screen"]["window_size_height"], height);
            }
        }

        // A size given twice stays as it is, and the screen values are
        // still written.
        let twice = r#"{"screen": {"window_size_width": 800, "window_size_height": 600, "window_size_width": 1280}, "other": {"gl_ver_major": 3, "gl_ver_minor": 3}}"#;
        let (temporary, root) = managed_root(Some(twice.as_bytes()));
        assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Written);
        assert_eq!(
            String::from_utf8(read_config(&temporary)).unwrap(),
            twice.replacen(
                "\"screen\": {",
                "\"screen\": {\"unlock_cursor\": true, \"window_fullscreen\": true, \"auto_minimize\": false, ",
                1
            )
        );
    }

    #[test]
    fn each_profile_has_its_first_launch_graphics_and_the_rest_is_the_same() {
        // D2GL's corner mini map is off (it costs frame rate; D2GL's in-game
        // options turn it on); the profile sets only its graphics values.
        let default: Value =
            serde_json::from_slice(&first_launch_config(GraphicsProfile::Default)).unwrap();
        assert_eq!(
            default["graphics"]["shader_preset"],
            "anti-aliasing\\advanced-aa.slangp"
        );
        assert_eq!(default["graphics"]["bloom"], true);
        assert_eq!(default["graphics"]["sharpen"], true);
        assert_eq!(default["features"]["mini_map"], false);

        let mut performance: Value =
            serde_json::from_slice(&first_launch_config(GraphicsProfile::Performance)).unwrap();
        assert_eq!(performance["graphics"]["shader_preset"], "bilinear.slangp");
        assert_eq!(performance["graphics"]["sharpen"], false);
        assert_eq!(performance["graphics"]["bloom"], false);
        assert_eq!(performance["features"]["mini_map"], false);
        for (key, value) in GraphicsProfile::Default.values() {
            performance["graphics"][key] = value;
        }
        assert_eq!(performance, default);
    }

    #[test]
    fn the_first_launch_configuration_keeps_the_order_of_its_keys() {
        // D2GL's own order, which serde_json keeps with its preserve_order
        // feature (Cargo.toml); sorted keys would put "features" first.
        let text = String::from_utf8(first_launch_config(GraphicsProfile::Default)).unwrap();
        let at = |key: &str| text.find(&format!("\"{key}\"")).unwrap();
        let order = [
            "screen",
            "window_centered",
            "unlock_cursor",
            "auto_minimize",
            "graphics",
            "shader_preset",
            "features",
            "hd_cursor",
            "other",
            "gl_ver_major",
            "load_dlls_early",
        ];
        assert!(
            order.windows(2).all(|pair| at(pair[0]) < at(pair[1])),
            "{text}"
        );
    }

    fn record_path(root: &tempfile::TempDir) -> std::path::PathBuf {
        root.path()
            .join(PROJECT_D2)
            .join(LAUNCHER_FOLDER)
            .join(PROFILE_RECORD)
    }

    #[test]
    fn the_first_launch_takes_the_profile_and_records_it() {
        let (temporary, root) = managed_root(None);
        assert_eq!(
            configure_config(&root, Some(GraphicsProfile::Performance)).unwrap(),
            D2glSetup::Written
        );
        assert_eq!(
            read_config(&temporary),
            first_launch_config(GraphicsProfile::Performance)
        );
        assert_eq!(
            fs::read_to_string(record_path(&temporary)).unwrap(),
            "performance\n"
        );
    }

    #[test]
    fn a_profile_is_written_only_when_it_changes_and_the_players_values_stay_until_then() {
        let (temporary, root) = managed_root(None);
        assert_eq!(
            configure_config(&root, Some(GraphicsProfile::Performance)).unwrap(),
            D2glSetup::Written
        );

        // The player turns bloom and FXAA on in the game (Ctrl+O): every
        // Play with the same profile keeps them.
        let mut played: Value = serde_json::from_slice(&read_config(&temporary)).unwrap();
        played["graphics"]["bloom"] = json!(true);
        played["graphics"]["fxaa"] = json!(true);
        let played_bytes = serde_json::to_vec_pretty(&played).unwrap();
        fs::write(
            temporary.path().join(PROJECT_D2).join(CONFIG),
            &played_bytes,
        )
        .unwrap();
        for _ in 0..2 {
            assert_eq!(
                configure_config(&root, Some(GraphicsProfile::Performance)).unwrap(),
                D2glSetup::Unchanged
            );
            assert_eq!(read_config(&temporary), played_bytes);
        }

        // Default picked: its three values, nothing else.
        assert_eq!(
            configure_config(&root, Some(GraphicsProfile::Default)).unwrap(),
            D2glSetup::Written
        );
        let written: Value = serde_json::from_slice(&read_config(&temporary)).unwrap();
        let mut expected = played.clone();
        expected["graphics"]["shader_preset"] = json!("anti-aliasing\\advanced-aa.slangp");
        expected["graphics"]["sharpen"] = json!(true);
        expected["graphics"]["bloom"] = json!(true);
        assert_eq!(written, expected);
        assert_eq!(written["graphics"]["fxaa"], true);
        assert_eq!(
            fs::read_to_string(record_path(&temporary)).unwrap(),
            "default\n"
        );
        let default_bytes = read_config(&temporary);
        assert_eq!(
            configure_config(&root, Some(GraphicsProfile::Default)).unwrap(),
            D2glSetup::Unchanged
        );
        assert_eq!(read_config(&temporary), default_bytes);

        // Back to Performance: its three values again.
        assert_eq!(
            configure_config(&root, Some(GraphicsProfile::Performance)).unwrap(),
            D2glSetup::Written
        );
        let written: Value = serde_json::from_slice(&read_config(&temporary)).unwrap();
        expected["graphics"]["shader_preset"] = json!("bilinear.slangp");
        expected["graphics"]["sharpen"] = json!(false);
        expected["graphics"]["bloom"] = json!(false);
        assert_eq!(written, expected);
    }

    #[test]
    fn a_configuration_without_a_readable_record_gets_the_profile() {
        // An imported game folder has its own d2gl.json and no record; a
        // record the Launcher cannot read counts as none.
        for record in [None, Some(&b"ultra\n"[..])] {
            let (temporary, root) = managed_root(Some(DEFAULT_CONFIG.as_bytes()));
            if let Some(record) = record {
                let folder = temporary.path().join(PROJECT_D2).join(LAUNCHER_FOLDER);
                fs::create_dir_all(&folder).unwrap();
                fs::write(folder.join(PROFILE_RECORD), record).unwrap();
            }
            assert_eq!(
                configure_config(&root, Some(GraphicsProfile::Performance)).unwrap(),
                D2glSetup::Written
            );
            let written = String::from_utf8(read_config(&temporary)).unwrap();
            assert_eq!(
                written,
                DEFAULT_CONFIG
                    .replacen(
                        "\"screen\": {",
                        "\"screen\": {\"unlock_cursor\": true, \"auto_minimize\": false, ",
                        1
                    )
                    .replacen(
                        "\"graphics\": {",
                        "\"graphics\": {\"sharpen\": false, \"bloom\": false, ",
                        1
                    )
                    .replacen("anti-aliasing\\\\advanced-aa.slangp", "bilinear.slangp", 1)
                    .replacen(
                        "\"other\": {",
                        "\"other\": {\"gl_ver_major\": 3, \"gl_ver_minor\": 3, ",
                        1
                    )
            );
            assert_eq!(
                fs::read_to_string(record_path(&temporary)).unwrap(),
                "performance\n"
            );
        }
    }

    #[test]
    fn the_screen_values_and_the_profile_are_written_without_each_other() {
        // Graphics that are not an object: the screen values are still
        // written, and nothing is recorded, so the next Play tries again.
        let (temporary, root) = managed_root(Some(
            br#"{"screen": {"window_size_width": 1604, "window_size_height": 720}, "graphics": null, "other": {"gl_ver_major": 3, "gl_ver_minor": 3}}"#,
        ));
        assert_eq!(
            configure_config(&root, Some(GraphicsProfile::Performance)).unwrap(),
            D2glSetup::Written
        );
        assert_eq!(
            read_config(&temporary),
            br#"{"screen": {"unlock_cursor": true, "window_fullscreen": true, "auto_minimize": false, "window_size_width": 1604, "window_size_height": 720}, "graphics": null, "other": {"gl_ver_major": 3, "gl_ver_minor": 3}}"#
        );
        assert!(!record_path(&temporary).exists());

        // Screen values that are not addressable (a key twice): nothing can
        // be set without a profile, and the profile is still written.
        let twice = r#"{"screen": {"unlock_cursor": false, "unlock_cursor": false}, "graphics": {}, "other": {"gl_ver_major": 3, "gl_ver_minor": 3}}"#;
        let (temporary, root) = managed_root(Some(twice.as_bytes()));
        assert!(configure_config(&root, None).is_err());
        assert_eq!(read_config(&temporary), twice.as_bytes());
        assert_eq!(
            configure_config(&root, Some(GraphicsProfile::Performance)).unwrap(),
            D2glSetup::Written
        );
        assert_eq!(
            String::from_utf8(read_config(&temporary)).unwrap(),
            twice.replacen(
                "\"graphics\": {}",
                "\"graphics\": {\"shader_preset\": \"bilinear.slangp\", \"sharpen\": false, \"bloom\": false}",
                1
            )
        );
        assert_eq!(
            fs::read_to_string(record_path(&temporary)).unwrap(),
            "performance\n"
        );
    }

    #[test]
    fn a_first_launch_records_what_it_wrote_without_a_profile_too() {
        // An older record never stands for a configuration written anew.
        let (temporary, root) = managed_root(None);
        let folder = temporary.path().join(PROJECT_D2).join(LAUNCHER_FOLDER);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(PROFILE_RECORD), "performance\n").unwrap();
        assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Written);
        assert_eq!(
            read_config(&temporary),
            first_launch_config(GraphicsProfile::Default)
        );
        assert_eq!(
            fs::read_to_string(record_path(&temporary)).unwrap(),
            "default\n"
        );
        assert_eq!(
            configure_config(&root, Some(GraphicsProfile::Performance)).unwrap(),
            D2glSetup::Written
        );
        let written: Value = serde_json::from_slice(&read_config(&temporary)).unwrap();
        assert_eq!(written["graphics"]["shader_preset"], "bilinear.slangp");
    }

    #[test]
    fn a_profile_changes_only_its_values_byte_for_byte() {
        // Present values in place, missing ones at the start of graphics.
        let profiled = profiled_config(DEFAULT_CONFIG.as_bytes(), GraphicsProfile::Default)
            .unwrap()
            .expect("sharpen and bloom are missing");
        assert_eq!(
            String::from_utf8(profiled.clone()).unwrap(),
            DEFAULT_CONFIG.replacen(
                "\"graphics\": {",
                "\"graphics\": {\"sharpen\": true, \"bloom\": true, ",
                1
            )
        );
        assert_eq!(
            profiled_config(&profiled, GraphicsProfile::Default).unwrap(),
            None
        );

        // A byte order mark and compact formatting stay; no graphics yet.
        let compact = b"\xEF\xBB\xBF{\"screen\":{\"window_size_width\":1068}}";
        assert_eq!(
            profiled_config(compact, GraphicsProfile::Performance)
                .unwrap()
                .unwrap(),
            b"\xEF\xBB\xBF{\"graphics\": {\"shader_preset\": \"bilinear.slangp\", \"sharpen\": false, \"bloom\": false}, \"screen\":{\"window_size_width\":1068}}"
        );
        let empty: Value = serde_json::from_slice(
            &profiled_config(b"{}", GraphicsProfile::Default)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            empty,
            json!({"graphics": {"shader_preset": "anti-aliasing\\advanced-aa.slangp", "sharpen": true, "bloom": true}})
        );

        // Graphics that are not one object are left alone.
        for invalid in [
            &b"{\"graphics\": []}"[..],
            b"{\"graphics\": {\"bloom\": true, \"bloom\": false}}",
            b"[]",
        ] {
            assert!(profiled_config(invalid, GraphicsProfile::Performance).is_err());
        }
    }

    /// A desktop player's configuration, as an import from a PC brings it:
    /// windowed, with D2GL's locked cursor, their own fps limit, FXAA, corner
    /// mini map and fps counter.
    const IMPORTED_CONFIG: &str = r#"{
    "screen": {
        "window_size_width": 3840,
        "window_size_height": 2160,
        "window_centered": true,
        "unlock_cursor": false,
        "window_fullscreen": false,
        "foreground_fps_value": 123,
        "auto_minimize": false,
        "window_dark_mode": true
    },
    "graphics": {
        "shader_preset": "anti-aliasing\\advanced-aa.slangp",
        "fxaa": true
    },
    "features": {
        "mini_map": true,
        "show_fps": true
    }
}"#;

    #[test]
    fn the_android_host_makes_an_imported_configuration_full_screen_with_the_cursor_unlocked() {
        // With the locked cursor the first click after each start is lost on
        // the X screen, and the window is placed partly off it.
        let (temporary, root) = managed_root(Some(IMPORTED_CONFIG.as_bytes()));
        assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Written);
        let expected = IMPORTED_CONFIG
            .replacen("\"unlock_cursor\": false", "\"unlock_cursor\": true", 1)
            .replacen(
                "\"window_fullscreen\": false",
                "\"window_fullscreen\": true",
                1,
            )
            // D2GL writes the version when it saves; this one has none.
            .replacen(
                '{',
                "{\"other\": {\"gl_ver_major\": 3, \"gl_ver_minor\": 3}, ",
                1,
            );
        assert_eq!(
            String::from_utf8(read_config(&temporary)).unwrap(),
            expected
        );

        // Every other key stays as the player had it.
        let written: Value = serde_json::from_slice(&read_config(&temporary)).unwrap();
        let original: Value = serde_json::from_str(IMPORTED_CONFIG).unwrap();
        assert_eq!(written["graphics"], original["graphics"]);
        assert_eq!(written["features"], original["features"]);
        assert_eq!(written["screen"]["foreground_fps_value"], 123);
        assert_eq!(written["screen"]["window_dark_mode"], true);
        assert!(!record_path(&temporary).exists());

        // The next Play finds nothing to change.
        assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Unchanged);
        assert_eq!(
            String::from_utf8(read_config(&temporary)).unwrap(),
            expected
        );
    }

    #[test]
    fn the_android_screen_values_are_inserted_where_missing_and_stand_alone() {
        // Missing values at the start of screen, present ones in place: a
        // player's auto_minimize goes off with the game full screen.
        let (temporary, root) = managed_root(Some(
            br#"{"screen": {"window_size_width": 1280, "window_size_height": 960, "auto_minimize": true}, "other": {"gl_ver_major": 3, "gl_ver_minor": 3}}"#,
        ));
        assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Written);
        assert_eq!(
            read_config(&temporary),
            br#"{"screen": {"unlock_cursor": true, "window_fullscreen": true, "window_size_width": 1280, "window_size_height": 960, "auto_minimize": false}, "other": {"gl_ver_major": 3, "gl_ver_minor": 3}}"#
        );

        // No screen at all: a new one with just these values.
        let (temporary, root) = managed_root(Some(
            br#"{"other": {"gl_ver_major": 3, "gl_ver_minor": 3}}"#,
        ));
        assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Written);
        assert_eq!(
            read_config(&temporary),
            br#"{"screen": {"unlock_cursor": true, "window_fullscreen": true, "auto_minimize": false}, "other": {"gl_ver_major": 3, "gl_ver_minor": 3}}"#
        );

        // A screen that is not an object: it stays, and the profile is still
        // written.
        let (temporary, root) = managed_root(Some(
            br#"{"screen": null, "graphics": {}, "other": {"gl_ver_major": 3, "gl_ver_minor": 3}}"#,
        ));
        assert!(configure_config(&root, None).is_err());
        assert_eq!(
            configure_config(&root, Some(GraphicsProfile::Performance)).unwrap(),
            D2glSetup::Written
        );
        assert_eq!(
            read_config(&temporary),
            br#"{"screen": null, "graphics": {"shader_preset": "bilinear.slangp", "sharpen": false, "bloom": false}, "other": {"gl_ver_major": 3, "gl_ver_minor": 3}}"#
        );
    }

    #[test]
    fn the_first_launch_has_the_android_screen_values() {
        let first: Value =
            serde_json::from_slice(&first_launch_config(GraphicsProfile::Default)).unwrap();
        for (key, value) in ANDROID_SCREEN {
            assert_eq!(first["screen"][key], value, "{key}");
        }
    }

    #[test]
    fn graphics_the_player_chose_stay_without_a_profile() {
        let (temporary, root) = managed_root(None);
        let mut chosen: Value =
            serde_json::from_slice(&first_launch_config(GraphicsProfile::Default)).unwrap();
        chosen["graphics"]["shader_preset"] = json!("bilinear.slangp");
        chosen["graphics"]["bloom"] = json!(false);
        let chosen = serde_json::to_vec_pretty(&chosen).unwrap();
        fs::write(temporary.path().join(PROJECT_D2).join(CONFIG), &chosen).unwrap();
        for _ in 0..2 {
            assert_eq!(configure_config(&root, None).unwrap(), D2glSetup::Unchanged);
            assert_eq!(read_config(&temporary), chosen);
        }
        assert!(!record_path(&temporary).exists());
    }

    #[test]
    fn the_android_host_never_follows_a_dangling_configuration_link() {
        let (temporary, root) = managed_root(None);
        let outside = temporary.path().join("outside.json");
        symlink(&outside, temporary.path().join(PROJECT_D2).join(CONFIG)).unwrap();
        assert_eq!(configure(&root, None), D2glSetup::Skipped);
        assert!(!outside.exists());
    }

    #[test]
    fn a_missing_configuration_is_reported_absent() {
        let (_temporary, root) = managed_root(None);
        assert!(configuration_absent(&root));
    }

    #[test]
    fn present_or_unreadable_configuration_is_not_reported_absent() {
        let (_temporary, root) = managed_root(Some(DEFAULT_CONFIG.as_bytes()));
        assert!(!configuration_absent(&root));

        let (temporary, root) = managed_root(None);
        let outside = temporary.path().join("outside.json");
        fs::write(&outside, DEFAULT_CONFIG).unwrap();
        symlink(&outside, temporary.path().join(PROJECT_D2).join(CONFIG)).unwrap();
        assert!(!configuration_absent(&root));
    }

    #[test]
    fn invalid_or_oversized_configuration_is_skipped_untouched() {
        let mut oversized = DEFAULT_CONFIG.as_bytes().to_vec();
        oversized.resize(MAX_CONFIG_BYTES as usize + 1, b' ');
        for config in [b"{ not json".to_vec(), oversized] {
            for profile in [None, Some(GraphicsProfile::Performance)] {
                let (temporary, root) = managed_root(Some(&config));
                assert_eq!(configure(&root, profile), D2glSetup::Skipped);
                assert_eq!(read_config(&temporary), config);
                assert!(!record_path(&temporary).exists());
            }
        }
    }

    #[test]
    fn symlinked_configuration_is_skipped_without_following() {
        let (temporary, root) = managed_root(None);
        let outside = temporary.path().join("outside.json");
        fs::write(&outside, DEFAULT_CONFIG).unwrap();
        symlink(&outside, temporary.path().join(PROJECT_D2).join(CONFIG)).unwrap();

        assert_eq!(configure(&root, None), D2glSetup::Skipped);
        assert_eq!(fs::read_to_string(outside).unwrap(), DEFAULT_CONFIG);
    }

    /// The `other` section as PD2's D2GL writes it.
    const PD2_OTHER: &str = r#""other": {
        "gl_ver_major": 4,
        "gl_ver_minor": 6,
        "use_compute_shader": false,
        "frame_latency": 1,
        "load_dlls_early": "",
        "load_dlls_late": "SGD2FreeRes.dll"
    }"#;

    fn pd2_config(other: &str) -> String {
        format!(
            "{{\n    \"screen\": {{\n        \"window_size_width\": 1068,\n        \"unlock_cursor\": true\n    }},\n    {other}\n}}"
        )
    }

    #[test]
    fn the_dlls_are_set_to_pd2s_own_and_every_other_byte_stays() {
        // Another app pointed both at files it dropped in the game folder;
        // D2GL loads a PE file whatever its name.
        let planted = PD2_OTHER
            .replacen(
                "\"load_dlls_early\": \"\"",
                "\"load_dlls_early\": \"Save/notes.txt\"",
                1,
            )
            .replacen(
                "\"SGD2FreeRes.dll\"",
                "\"SGD2FreeRes.dll, filters/x.png:stdcall:_Init@0\"",
                1,
            );
        let (temporary, root) = managed_root(Some(pd2_config(&planted).as_bytes()));
        pin_dlls(&root).unwrap();
        assert_eq!(
            String::from_utf8(read_config(&temporary)).unwrap(),
            pd2_config(PD2_OTHER)
        );

        // Missing values go at the start of `other`, a missing `other` at
        // the start of the configuration.
        for (config, pinned) in [
            (
                r#"{"other": {"frame_latency": 2}}"#,
                r#"{"other": {"load_dlls_early": "", "load_dlls_late": "SGD2FreeRes.dll", "frame_latency": 2}}"#,
            ),
            (
                r#"{"screen": {}}"#,
                r#"{"other": {"load_dlls_early": "", "load_dlls_late": "SGD2FreeRes.dll"}, "screen": {}}"#,
            ),
        ] {
            let (temporary, root) = managed_root(Some(config.as_bytes()));
            pin_dlls(&root).unwrap();
            assert_eq!(read_config(&temporary), pinned.as_bytes());
        }

        // A byte order mark stays.
        let (temporary, root) = managed_root(Some(b"\xEF\xBB\xBF{\"other\": {}}"));
        pin_dlls(&root).unwrap();
        assert_eq!(
            read_config(&temporary),
            b"\xEF\xBB\xBF{\"other\": {\"load_dlls_early\": \"\", \"load_dlls_late\": \"SGD2FreeRes.dll\"}}"
        );
    }

    #[test]
    fn pinned_dlls_are_written_anew_so_an_earlier_descriptor_misses_them() {
        let (temporary, root) = managed_root(Some(pd2_config(PD2_OTHER).as_bytes()));
        let path = temporary.path().join(PROJECT_D2).join(CONFIG);
        let held = fs::OpenOptions::new().write(true).open(&path).unwrap();
        let before = fs::metadata(&path).unwrap().ino();

        pin_dlls(&root).unwrap();
        assert_ne!(fs::metadata(&path).unwrap().ino(), before);
        (&held)
            .write_all(br#"{"other": {"load_dlls_late": "Save/notes.txt"}}"#)
            .unwrap();
        assert_eq!(read_config(&temporary), pd2_config(PD2_OTHER).as_bytes());
        assert!(!temporary.path().join(PROJECT_D2).join(STAGED).exists());
    }

    #[test]
    fn a_configuration_whose_dlls_cannot_be_set_becomes_its_backup_with_pd2s_dlls() {
        // D2GL loads its backup in place of a configuration it cannot parse.
        let mut oversized = pd2_config(PD2_OTHER).into_bytes();
        oversized.resize(MAX_CONFIG_BYTES as usize + 1, b' ');
        let unpinnable = [
            b"{ not json".to_vec(),
            oversized,
            br#"{"other": {"load_dlls_late": "SGD2FreeRes.dll", "load_dlls_late": "Save/notes.txt"}}"#.to_vec(),
            br#"{"other": {"load_dlls_lat\u0065": "Save/notes.txt"}}"#.to_vec(),
            br#"{"other": {}, "\u006fther": {"load_dlls_early": "Save/notes.txt"}}"#.to_vec(),
            br#"{"other": "load_dlls_late"}"#.to_vec(),
            br#"{"other": null}"#.to_vec(),
            b"[]".to_vec(),
        ];
        let planted_backup =
            pd2_config(&PD2_OTHER.replacen("\"SGD2FreeRes.dll\"", "\"Save/notes.txt\"", 1));
        for config in &unpinnable {
            let (temporary, root) = managed_root(Some(config));
            fs::write(
                temporary.path().join(PROJECT_D2).join(BACKUP),
                &planted_backup,
            )
            .unwrap();
            pin_dlls(&root).unwrap();
            assert_eq!(
                String::from_utf8(read_config(&temporary)).unwrap(),
                pd2_config(PD2_OTHER),
                "{}",
                String::from_utf8_lossy(config)
            );
        }

        // Without a backup that can be edited it is removed, and D2GL starts
        // from its defaults (PD2's DLLs).
        for backup in [None, Some(&b"{ not json"[..])] {
            let (temporary, root) = managed_root(Some(b"{ not json"));
            if let Some(backup) = backup {
                fs::write(temporary.path().join(PROJECT_D2).join(BACKUP), backup).unwrap();
            }
            pin_dlls(&root).unwrap();
            assert!(!temporary.path().join(PROJECT_D2).join(CONFIG).exists());
        }

        // Repeated keys whose last is PD2's: what D2GL reads is already
        // right, and the file stays as it is.
        let repeated = br#"{"other": {"load_dlls_late": "Save/notes.txt", "load_dlls_late": "SGD2FreeRes.dll", "load_dlls_early": ""}}"#;
        let (temporary, root) = managed_root(Some(repeated));
        pin_dlls(&root).unwrap();
        assert_eq!(read_config(&temporary), repeated);
    }

    #[test]
    fn a_missing_configuration_stays_missing_and_one_that_is_no_file_stops_the_pin() {
        let (temporary, root) = managed_root(None);
        pin_dlls(&root).unwrap();
        assert!(!temporary.path().join(PROJECT_D2).join(CONFIG).exists());

        // D2GL cannot write a configuration over a folder and loads its
        // backup instead.
        let (temporary, root) = managed_root(None);
        fs::create_dir(temporary.path().join(PROJECT_D2).join(CONFIG)).unwrap();
        assert!(pin_dlls(&root).is_err());

        let (temporary, root) = managed_root(None);
        let outside = temporary.path().join("outside.json");
        fs::write(&outside, DEFAULT_CONFIG).unwrap();
        symlink(&outside, temporary.path().join(PROJECT_D2).join(CONFIG)).unwrap();
        assert!(pin_dlls(&root).is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), DEFAULT_CONFIG);
    }

    #[test]
    fn other_spellings_of_the_configuration_are_removed() {
        // Wine opens a name that differs only in case when d2gl.json itself
        // is missing (`ſ` is a lowercase s).
        let (temporary, root) = managed_root(None);
        let project = temporary.path().join(PROJECT_D2);
        for name in ["D2GL.json", "d2gl.JSON", "d2gl.jſon"] {
            fs::write(
                project.join(name),
                br#"{"other": {"load_dlls_late": "Save/notes.txt"}}"#,
            )
            .unwrap();
        }
        for name in [BACKUP, "d2gl.jsonx", "d2gl.jßon"] {
            fs::write(project.join(name), b"{}").unwrap();
        }
        pin_dlls(&root).unwrap();
        let mut names = fs::read_dir(&project)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, [BACKUP, "d2gl.jsonx", "d2gl.jßon"]);

        // A folder among them stops the pin.
        fs::create_dir(project.join("D2GL.JSON")).unwrap();
        assert!(pin_dlls(&root).is_err());
    }

    #[test]
    fn the_first_launch_has_the_dlls_pd2_ships() {
        let first: Value =
            serde_json::from_slice(&first_launch_config(GraphicsProfile::Default)).unwrap();
        assert_eq!(first["other"]["load_dlls_early"], "");
        assert_eq!(first["other"]["load_dlls_late"], "SGD2FreeRes.dll");
        let pinned = first_launch_config(GraphicsProfile::Default);
        assert_eq!(set_values(&pinned, "other", &shipped_dlls()).unwrap(), None);
    }
}
