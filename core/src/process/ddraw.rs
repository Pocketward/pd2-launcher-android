//! Chooses how PD2's DirectDraw wrapper (cnc-ddraw) draws, before a
//! DirectDraw launch.
//!
//! PD2 ships `ProjectD2/ddraw.ini` with `renderer=opengl`. Where OpenGL
//! inside Wine shows nothing (DroidDeck, whose Windows games draw through
//! Vulkan), `direct3d9` (DXVK, Vulkan) or `gdi` (software) work instead. The
//! file is the player's (updates preserve it); only its `renderer=` line in
//! the `[ddraw]` section is changed, every failure skips the change, and the
//! launch continues.
//!
//! With `gdi`, cnc-ddraw stretches the 800 × 600 menus to the full window,
//! and PD2's `adjmouse=false` reads the pointer unscaled: a tap lands up to
//! 1.335 × too far from the left edge. So a `gdi` write also sets
//! `adjmouse=true`. Before it writes, it records the two lines as they were
//! ([`SOFTWARE_RECORD`]); the first start after it with another renderer, or
//! with none chosen, puts both lines back as they were: a line that was not
//! there goes again, and a record that cannot be read gives PD2's shipped
//! values ([`SHIPPED_RENDERER`] and `adjmouse=false`). The owner approved this one more
//! value (2026-11-03); no other line changes.

use std::{
    ffi::OsStr,
    io::{self, Read as _},
    path::Path,
};

use crate::safe_fs::Directory;

const PROJECT_D2: &str = "ProjectD2";
const CONFIG: &str = "ddraw.ini";
const MAX_CONFIG_BYTES: u64 = 256 * 1024;
/// The Launcher's own folder in ProjectD2 (as for `d2gl.json`'s profile
/// record); the audit never counts it as a game file.
const LAUNCHER_FOLDER: &str = ".pd2launcher";
/// In [`LAUNCHER_FOLDER`]: `ddraw.ini` holds the `gdi` write's
/// `renderer=gdi` and `adjmouse=true`; the record holds the two lines from
/// before it (`renderer=<value>` and `adjmouse=<value>`, one a line, an empty
/// value for a line that was not there), which the next start without `gdi`
/// puts back ([`leave_software`]).
const SOFTWARE_RECORD: &str = "ddraw-software";
const MAX_RECORD_BYTES: u64 = 256;
/// The renderer PD2 ships in `ddraw.ini`.
const SHIPPED_RENDERER: &str = "opengl";
/// The `adjmouse` value PD2 ships in `ddraw.ini`.
const SHIPPED_ADJMOUSE: &str = "false";
/// The software renderer, which also needs `adjmouse=true`.
const SOFTWARE: &str = "gdi";

/// Set cnc-ddraw's renderer to `renderer` (`opengl`, `direct3d9`, `gdi`).
/// `gdi` also sets `adjmouse=true`; it records the two earlier lines first,
/// so a failed record never leaves `adjmouse=true` behind (and a record whose
/// `ddraw.ini` write then failed puts back the same values). A second `gdi`
/// start keeps the first record. Another renderer after such a write also
/// puts back the earlier `adjmouse` line. Whether the file changed.
pub(crate) fn set_renderer(folder: &Directory, renderer: &str) -> io::Result<bool> {
    let project = folder.open_directory(Path::new(PROJECT_D2))?;
    let earlier = recorded(&project);
    if renderer.eq_ignore_ascii_case(SOFTWARE) {
        if earlier.is_none() {
            let text = read_config(&project)?;
            let record = Earlier::of(&text);
            project
                .ensure_directory(Path::new(LAUNCHER_FOLDER))?
                .atomic_write(OsStr::new(SOFTWARE_RECORD), record.to_text().as_bytes())?;
        }
        return write_values(&project, Some(renderer), Some(Some("true")));
    }
    let changed = write_values(
        &project,
        Some(renderer),
        earlier.as_ref().map(Earlier::adjmouse),
    )?;
    if earlier.is_some() {
        forget_software(&project)?;
    }
    Ok(changed)
}

/// A start that draws without `gdi` (Glide, or DirectDraw with no renderer
/// chosen) after a `gdi` write ([`SOFTWARE_RECORD`]): `renderer=` and
/// `adjmouse=` go back to their values from before it. Whether there was
/// such a write to put back.
pub(crate) fn leave_software(folder: &Directory) -> io::Result<bool> {
    let project = folder.open_directory(Path::new(PROJECT_D2))?;
    let Some(earlier) = recorded(&project) else {
        return Ok(false);
    };
    write_values(&project, earlier.renderer(), Some(earlier.adjmouse()))?;
    forget_software(&project)?;
    Ok(true)
}

/// One `ddraw.ini` line from before a `gdi` write: its value, or no line.
#[derive(Debug, PartialEq)]
enum Was {
    Value(String),
    NoLine,
}

/// The two `ddraw.ini` lines from before a `gdi` write ([`SOFTWARE_RECORD`]);
/// `None`: not known (a record that cannot be read), so PD2's shipped value
/// goes back.
#[derive(Debug, PartialEq)]
struct Earlier {
    renderer: Option<Was>,
    adjmouse: Option<Was>,
}

impl Earlier {
    /// The two lines of `ddraw.ini`'s `text` (review LOW-2: a missing line is
    /// recorded as missing).
    fn of(text: &str) -> Self {
        let was = |key| Some(value_of(text, key).map_or(Was::NoLine, Was::Value));
        Self {
            renderer: was("renderer"),
            adjmouse: was("adjmouse"),
        }
    }

    fn to_text(&self) -> String {
        fn line(was: &Option<Was>) -> &str {
            match was {
                Some(Was::Value(value)) => value,
                _ => "",
            }
        }
        format!(
            "renderer={}\nadjmouse={}\n",
            line(&self.renderer),
            line(&self.adjmouse)
        )
    }

    /// The record's lines: an empty value is a line that was not there; a
    /// value that is not a plain word (or `gdi` itself, as in an earlier form
    /// of the record), or no such key, is not known.
    fn parse(text: &str) -> Self {
        let was = |key: &str| {
            let value = text
                .lines()
                .filter_map(|line| line.split_once('='))
                .find(|(name, _)| name.trim() == key)?
                .1
                .trim();
            if value.is_empty() {
                return Some(Was::NoLine);
            }
            (value.len() <= 32
                && value.chars().all(|c| c.is_ascii_alphanumeric())
                && !value.eq_ignore_ascii_case(SOFTWARE))
            .then(|| Was::Value(value.to_owned()))
        };
        Self {
            renderer: was("renderer"),
            adjmouse: was("adjmouse"),
        }
    }

    /// The `renderer=` line to put back; `None`: remove it.
    fn renderer(&self) -> Option<&str> {
        Self::target(&self.renderer, SHIPPED_RENDERER)
    }

    /// The `adjmouse=` line to put back; `None`: remove it.
    fn adjmouse(&self) -> Option<&str> {
        Self::target(&self.adjmouse, SHIPPED_ADJMOUSE)
    }

    fn target<'a>(was: &'a Option<Was>, shipped: &'a str) -> Option<&'a str> {
        match was {
            Some(Was::Value(value)) => Some(value),
            Some(Was::NoLine) => None,
            None => Some(shipped),
        }
    }
}

/// The line the Launcher prints for a DirectDraw start with a chosen
/// renderer, after `ddraw.ini` has it. The Android app reads it
/// (`PhoneDriverFailure.drawsWithGdi`: "pd2-launcher: DirectDraw draws with
/// gdi "), so its words are a contract, which a test pins.
pub(crate) fn draws_with_line(renderer: &str, changed: bool) -> String {
    format!(
        "pd2-launcher: DirectDraw draws with {renderer} (ddraw.ini {})",
        if changed { "updated" } else { "already set" }
    )
}

/// The record of a `gdi` write, if there is one.
fn recorded(project: &Directory) -> Option<Earlier> {
    let mut text = String::new();
    project
        .open_directory(Path::new(LAUNCHER_FOLDER))
        .and_then(|folder| folder.open_file(Path::new(SOFTWARE_RECORD)))
        .ok()?
        .take(MAX_RECORD_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    Some(Earlier::parse(&text))
}

fn forget_software(project: &Directory) -> io::Result<()> {
    let folder = project.open_directory(Path::new(LAUNCHER_FOLDER))?;
    let record = folder.open_file(Path::new(SOFTWARE_RECORD))?;
    folder.remove_open_file_bounded(OsStr::new(SOFTWARE_RECORD), &record)
}

/// `ddraw.ini` with `renderer` (`None`: no such line) and, when given,
/// `adjmouse` (`Some(None)`: no such line); written only when a line changes.
/// Whether it changed.
fn write_values(
    project: &Directory,
    renderer: Option<&str>,
    adjmouse: Option<Option<&str>>,
) -> io::Result<bool> {
    let text = read_config(project)?;
    let set = |text: &str, key: &str, value: Option<&str>| match value {
        Some(value) => with_value(text, key, value),
        None => without_key(text, key),
    };
    let renderer_set = set(&text, "renderer", renderer);
    let both = match adjmouse {
        Some(value) => {
            set(renderer_set.as_deref().unwrap_or(&text), "adjmouse", value).or(renderer_set)
        }
        None => renderer_set,
    };
    let Some(updated) = both else {
        return Ok(false);
    };
    project.atomic_write(OsStr::new(CONFIG), updated.as_bytes())?;
    Ok(true)
}

/// `ddraw.ini`'s text, at most [`MAX_CONFIG_BYTES`].
fn read_config(project: &Directory) -> io::Result<String> {
    let mut text = String::new();
    project
        .open_file(Path::new(CONFIG))?
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > MAX_CONFIG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ddraw.ini is too large",
        ));
    }
    Ok(text)
}

/// The value of the `[ddraw]` section's `key=` line; `None` without one.
fn value_of(text: &str, key: &str) -> Option<String> {
    let mut section = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed[1..trimmed.len() - 1].trim().to_ascii_lowercase();
        } else if section == "ddraw"
            && let Some((name, value)) = trimmed.split_once('=')
            && name.trim().eq_ignore_ascii_case(key)
        {
            return Some(value.trim().to_owned());
        }
    }
    None
}

/// `text` without the `[ddraw]` section's first `key=` line, or `None` when
/// it has none.
fn without_key(text: &str, key: &str) -> Option<String> {
    let mut section = String::new();
    let mut removed = false;
    let mut lines = Vec::new();
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed[1..trimmed.len() - 1].trim().to_ascii_lowercase();
        } else if section == "ddraw"
            && !removed
            && let Some((name, _)) = trimmed.split_once('=')
            && name.trim().eq_ignore_ascii_case(key)
        {
            removed = true;
            continue;
        }
        lines.push(line);
    }
    removed.then(|| lines.concat())
}

/// `text` with the `[ddraw]` section's `key=` set to `value`, or `None`
/// when it already is (or there is no `[ddraw]` section). Only that line
/// changes; a missing line goes right under the section's header.
fn with_value(text: &str, key: &str, value: &str) -> Option<String> {
    let mut section = String::new();
    let mut lines = Vec::new();
    let mut changed = false;
    let mut found = false;
    let mut header = None;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed[1..trimmed.len() - 1].trim().to_ascii_lowercase();
            if section == "ddraw" {
                header = Some(index);
            }
        } else if section == "ddraw"
            && !found
            && let Some((name, current)) = trimmed.split_once('=')
            && name.trim().eq_ignore_ascii_case(key)
        {
            found = true;
            if !current.trim().eq_ignore_ascii_case(value) {
                let ending = &line[line.trim_end_matches(['\r', '\n']).len()..];
                lines.push(format!("{key}={value}{ending}"));
                changed = true;
                continue;
            }
        }
        lines.push(line.to_owned());
    }
    if !found {
        let header = header?;
        let ending = if text.contains("\r\n") { "\r\n" } else { "\n" };
        lines.insert(header + 1, format!("{key}={value}{ending}"));
        changed = true;
    }
    changed.then(|| lines.concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_ddraw_sections_renderer_line_changes() {
        let text = "; cnc-ddraw\r\n[ddraw]\r\nwidth=0\r\nrenderer=opengl\r\nvsync=false\r\n[Game.exe]\r\nrenderer=gdi\r\n";
        assert_eq!(
            with_value(text, "renderer", "direct3d9").unwrap(),
            "; cnc-ddraw\r\n[ddraw]\r\nwidth=0\r\nrenderer=direct3d9\r\nvsync=false\r\n[Game.exe]\r\nrenderer=gdi\r\n"
        );
        assert_eq!(with_value(text, "renderer", "OpenGL"), None, "already set");
        assert_eq!(
            with_value("[ddraw]\nwidth=0\n", "renderer", "gdi").unwrap(),
            "[ddraw]\nrenderer=gdi\nwidth=0\n"
        );
        assert_eq!(
            with_value("width=0\n", "renderer", "gdi"),
            None,
            "no [ddraw] section"
        );
    }

    #[test]
    fn the_file_in_projectd2_is_rewritten_in_place() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join(PROJECT_D2)).unwrap();
        let config = fixture.path().join(PROJECT_D2).join(CONFIG);
        std::fs::write(&config, "[ddraw]\nrenderer=opengl\n").unwrap();
        let folder = Directory::open(fixture.path()).unwrap();
        assert!(set_renderer(&folder, "direct3d9").unwrap());
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "[ddraw]\nrenderer=direct3d9\n"
        );
        assert!(!set_renderer(&folder, "direct3d9").unwrap());
    }

    /// PD2's `ddraw.ini` lines that the Software driver's write touches, in
    /// the file's own form (CRLF, other lines between them).
    const SHIPPED: &str = "; cnc-ddraw\r\n[ddraw]\r\nwidth=1068\r\nadjmouse=false\r\nhandlemouse=true\r\nrenderer=opengl\r\nsinglecpu=true\r\n[Game.exe]\r\nrenderer=gdi\r\n";

    fn fixture(text: &str) -> (tempfile::TempDir, std::path::PathBuf, Directory) {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join(PROJECT_D2)).unwrap();
        let config = fixture.path().join(PROJECT_D2).join(CONFIG);
        std::fs::write(&config, text).unwrap();
        let folder = Directory::open(fixture.path()).unwrap();
        (fixture, config, folder)
    }

    fn record_file(root: &tempfile::TempDir) -> std::path::PathBuf {
        root.path()
            .join(PROJECT_D2)
            .join(LAUNCHER_FOLDER)
            .join(SOFTWARE_RECORD)
    }

    fn recorded(root: &tempfile::TempDir) -> bool {
        root.path()
            .join(PROJECT_D2)
            .join(LAUNCHER_FOLDER)
            .join(SOFTWARE_RECORD)
            .is_file()
    }

    #[test]
    fn gdi_also_sets_adjmouse_and_a_start_without_it_puts_the_shipped_bytes_back() {
        let (root, config, folder) = fixture(SHIPPED);
        assert!(set_renderer(&folder, "gdi").unwrap());
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "; cnc-ddraw\r\n[ddraw]\r\nwidth=1068\r\nadjmouse=true\r\nhandlemouse=true\r\nrenderer=gdi\r\nsinglecpu=true\r\n[Game.exe]\r\nrenderer=gdi\r\n"
        );
        assert!(recorded(&root));
        assert!(!set_renderer(&folder, "gdi").unwrap(), "already set");
        // Glide, or DirectDraw with no renderer chosen: both lines go back.
        assert!(leave_software(&folder).unwrap());
        assert_eq!(std::fs::read(&config).unwrap(), SHIPPED.as_bytes());
        assert!(!recorded(&root));
        assert!(
            !leave_software(&folder).unwrap(),
            "nothing left to put back"
        );
        assert_eq!(std::fs::read(&config).unwrap(), SHIPPED.as_bytes());
    }

    #[test]
    fn another_renderer_after_gdi_also_puts_adjmouse_back() {
        let (root, config, folder) = fixture(SHIPPED);
        set_renderer(&folder, "gdi").unwrap();
        assert!(set_renderer(&folder, "direct3d9").unwrap());
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            SHIPPED.replace("renderer=opengl", "renderer=direct3d9")
        );
        assert!(!recorded(&root));
    }

    #[test]
    fn without_a_gdi_write_adjmouse_is_never_touched() {
        let own = SHIPPED.replace("adjmouse=false", "adjmouse=true");
        let (_root, config, folder) = fixture(&own);
        assert!(!leave_software(&folder).unwrap());
        assert!(set_renderer(&folder, "direct3d9").unwrap());
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            own.replace("renderer=opengl", "renderer=direct3d9")
        );
    }

    /// Review LOW-4: the player's own earlier lines come back, not PD2's.
    #[test]
    fn a_start_without_gdi_puts_the_players_own_earlier_lines_back() {
        let own = SHIPPED
            .replace("adjmouse=false", "adjmouse=true")
            .replace("renderer=opengl", "renderer=direct3d9");
        let (root, config, folder) = fixture(&own);
        assert!(set_renderer(&folder, "gdi").unwrap());
        assert_eq!(
            std::fs::read_to_string(record_file(&root)).unwrap(),
            "renderer=direct3d9\nadjmouse=true\n"
        );
        // A second gdi start keeps the first record.
        assert!(!set_renderer(&folder, "gdi").unwrap());
        assert_eq!(
            std::fs::read_to_string(record_file(&root)).unwrap(),
            "renderer=direct3d9\nadjmouse=true\n"
        );
        assert!(leave_software(&folder).unwrap());
        assert_eq!(std::fs::read(&config).unwrap(), own.as_bytes());
        assert!(!recorded(&root));
        // Another chosen renderer after gdi: the earlier adjmouse comes back.
        let (_root, config, folder) = fixture(&own);
        set_renderer(&folder, "gdi").unwrap();
        set_renderer(&folder, "opengl").unwrap();
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            own.replace("renderer=direct3d9", "renderer=opengl")
        );
    }

    /// Review LOW-3: the record goes first, so a failed `ddraw.ini` write
    /// leaves a record of the same values, and its restore changes nothing.
    #[test]
    fn the_record_comes_first_and_a_record_without_the_gdi_write_restores_the_same_bytes() {
        let (root, config, folder) = fixture(SHIPPED);
        // As after a record write whose ddraw.ini write failed.
        let project = folder.open_directory(Path::new(PROJECT_D2)).unwrap();
        let text = read_config(&project).unwrap();
        let record = Earlier::of(&text);
        std::fs::create_dir(root.path().join(PROJECT_D2).join(LAUNCHER_FOLDER)).unwrap();
        std::fs::write(record_file(&root), record.to_text()).unwrap();
        assert!(leave_software(&folder).unwrap());
        assert_eq!(std::fs::read(&config).unwrap(), SHIPPED.as_bytes());
        // A ddraw.ini with no [ddraw] section: the gdi write fails after the record.
        let (root, _config, folder) = fixture("width=0\n");
        assert!(!set_renderer(&folder, "gdi").unwrap());
        assert!(recorded(&root));
    }

    /// The record of the first engine with the adjmouse write (030b9e5e) has
    /// one line of the gdi values: PD2's shipped values go back.
    #[test]
    fn an_earlier_record_form_puts_the_shipped_values_back() {
        let (root, config, folder) = fixture(SHIPPED);
        set_renderer(&folder, "gdi").unwrap();
        std::fs::write(record_file(&root), "renderer=gdi adjmouse=true\n").unwrap();
        assert!(leave_software(&folder).unwrap());
        assert_eq!(std::fs::read(&config).unwrap(), SHIPPED.as_bytes());
        assert_eq!(
            Earlier::parse("renderer=gdi adjmouse=true\n"),
            Earlier {
                renderer: None,
                adjmouse: None
            }
        );
    }

    /// Review LOW-2: a line that was not there before the gdi write is not
    /// there after the restore.
    #[test]
    fn a_line_that_was_not_there_goes_again() {
        let without = SHIPPED.replace("adjmouse=false\r\n", "");
        let (root, config, folder) = fixture(&without);
        assert!(set_renderer(&folder, "gdi").unwrap());
        assert!(
            std::fs::read_to_string(&config)
                .unwrap()
                .contains("adjmouse=true\r\n")
        );
        assert_eq!(
            std::fs::read_to_string(record_file(&root)).unwrap(),
            "renderer=opengl\nadjmouse=\n"
        );
        assert!(leave_software(&folder).unwrap());
        assert_eq!(std::fs::read(&config).unwrap(), without.as_bytes());
        // Another chosen renderer after gdi: the adjmouse line goes too.
        let (_root, config, folder) = fixture(&without);
        set_renderer(&folder, "gdi").unwrap();
        set_renderer(&folder, "direct3d9").unwrap();
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            without.replace("renderer=opengl", "renderer=direct3d9")
        );
        // Both lines missing.
        let neither = without.replace("renderer=opengl\r\n", "");
        let (_root, config, folder) = fixture(&neither);
        set_renderer(&folder, "gdi").unwrap();
        assert!(leave_software(&folder).unwrap());
        assert_eq!(std::fs::read(&config).unwrap(), neither.as_bytes());
    }

    /// The Android app reads this line (`PhoneDriverFailure.drawsWithGdi`
    /// matches "pd2-launcher: DirectDraw draws with gdi "): its words stay.
    #[test]
    fn the_gdi_line_that_the_android_app_reads_keeps_its_words() {
        assert_eq!(
            draws_with_line("gdi", true),
            "pd2-launcher: DirectDraw draws with gdi (ddraw.ini updated)"
        );
        assert_eq!(
            draws_with_line("gdi", false),
            "pd2-launcher: DirectDraw draws with gdi (ddraw.ini already set)"
        );
    }
}
