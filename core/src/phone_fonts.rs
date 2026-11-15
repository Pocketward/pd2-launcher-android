//! The phone's own fonts for Wine on the Android host (issue 19: a
//! launcher's Chinese text showed as boxes, because Wine had no font with
//! Chinese letters). The app lists the phone's font of each language it
//! links (Android's `SystemFonts`; today Simplified Chinese) in a file that `PD2_SYSTEM_FONTS` names; this
//! module turns that list into the prefix's registry values:
//!
//! - `Fonts`: one entry per font file, so that Wine loads it from where the
//!   phone keeps it (through drive `Z:`, which is `/`). Nothing is copied.
//! - `FontLink\SystemLink`: the Windows fonts that programs draw text with
//!   ([`LINKED_FONTS`]) take a letter they do not have from these fonts, in
//!   the list's order.
//! - `FontSubstitutes`: the Windows names of a language's fonts
//!   ([`WINDOWS_NAMES`]) give the phone's font of that language.
//! - `Software\PD2 Launcher` `PhoneFonts`: the SHA-256 of these values, so
//!   that `reg import` runs only when they changed.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The app's variable with the path of the list.
pub(crate) const LIST_VARIABLE: &str = "PD2_SYSTEM_FONTS";

/// The key of the marker value, as `reg import` names it and as Wine
/// writes it in `system.reg` (with doubled backslashes).
const MARKER_KEY: &str = r"Software\PD2 Launcher";
const MARKER_VALUE: &str = "PhoneFonts";

/// The Windows fonts that take missing letters from the phone's fonts: the
/// fonts of Windows dialogs and of most programs' text.
const LINKED_FONTS: [&str; 5] = [
    "Tahoma",
    "Segoe UI",
    "Microsoft Sans Serif",
    "Lucida Sans Unicode",
    "Arial",
];

/// The Windows names of the fonts of a language, by the language of the
/// phone font that replaces them (owner, 2026-11-09: Simplified Chinese
/// until a player reports boxes for another language).
const WINDOWS_NAMES: [(&str, &[&str]); 1] = [(
    "zh-Hans",
    &[
        "SimSun",
        "NSimSun",
        "SimHei",
        "Microsoft YaHei",
        "Microsoft YaHei UI",
        "\u{5b8b}\u{4f53}",
        "\u{65b0}\u{5b8b}\u{4f53}",
        "\u{9ed1}\u{4f53}",
        "\u{5fae}\u{8f6f}\u{96c5}\u{9ed1}",
    ],
)];

/// The languages whose font the log names, or says is missing: the ones
/// the app lists (its `SystemFontList.LANGUAGES`).
const MAIN_LANGUAGES: [(&str, &str); 1] = [("zh-Hans", "Chinese (Simplified)")];

/// One phone font of the app's list, with the family name Wine gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PhoneFont {
    /// Android's language tags, comma-separated (`zh-Hant,zh-Bopo`).
    pub(crate) languages: String,
    pub(crate) path: PathBuf,
    pub(crate) family: String,
}

impl PhoneFont {
    /// The language this font is for, as the tables here name it: Android's
    /// first tag without its region (`zh-Hans-CN` is `zh-Hans`,
    /// `ja-Jpan-JP` is `ja`, `und-Thai` stays).
    fn first_language(&self) -> String {
        language_key(self.languages.split(',').next().unwrap_or(""))
    }

    fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// The key of an Android language tag: `zh-Hans` or `zh-Hant` for Chinese
/// (by its script, or Traditional for Taiwan, Hong Kong and Macao), `ja`
/// and `ko` for Japanese and Korean, else the language and script subtags
/// (`und-Thai`).
fn language_key(tag: &str) -> String {
    let parts: Vec<&str> = tag.split(['-', '_']).collect();
    let script = parts.iter().skip(1).find(|part| part.len() == 4).copied();
    match parts.first().copied().unwrap_or("") {
        "zh" => {
            let traditional = script == Some("Hant")
                || (script.is_none()
                    && parts.iter().any(|part| matches!(*part, "TW" | "HK" | "MO")));
            if traditional { "zh-Hant" } else { "zh-Hans" }.to_owned()
        }
        "ja" => "ja".to_owned(),
        "ko" => "ko".to_owned(),
        language => match script {
            Some(script) => format!("{language}-{script}"),
            None => language.to_owned(),
        },
    }
}

/// The fonts of the app's list text (`languages<TAB>index<TAB>path` lines),
/// in its order, each with its family name from `family` (the file and the
/// index in a collection); a line without a readable family name is left
/// out and named in the second list.
pub(crate) fn from_list(
    text: &str,
    family: impl Fn(&Path, u32) -> Option<String>,
) -> (Vec<PhoneFont>, Vec<String>) {
    let mut fonts = Vec::new();
    let mut unreadable = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(languages), Some(index), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let (Ok(index), true) = (index.parse::<u32>(), path.starts_with('/')) else {
            continue;
        };
        let path = PathBuf::from(path);
        match family(&path, index) {
            Some(name) => fonts.push(PhoneFont {
                languages: languages.to_owned(),
                path,
                family: name,
            }),
            None => unreadable.push(format!("{languages} {}", path.display())),
        }
    }
    (fonts, unreadable)
}

/// The family name Wine gives the font at `index` of `path` (a font file or
/// a collection): name 1 of its `name` table, Windows platform, US English
/// (Wine's own fallback language), else the first Windows name 1.
pub(crate) fn family_name(path: &Path, index: u32) -> Option<String> {
    let file = File::open(path).ok()?;
    family_name_in(&file, index).ok().flatten()
}

fn read_at(file: &File, offset: u64, length: usize) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0; length];
    file.read_exact_at(&mut buffer, offset)?;
    Ok(buffer)
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn family_name_in(file: &File, index: u32) -> io::Result<Option<String>> {
    let header = read_at(file, 0, 12)?;
    let font = if &header[..4] == b"ttcf" {
        if index >= u32_at(&header, 8) {
            return Ok(None);
        }
        u32_at(&read_at(file, 12 + 4 * u64::from(index), 4)?, 0)
    } else if index == 0 {
        0
    } else {
        return Ok(None);
    };
    let directory = read_at(file, u64::from(font), 12)?;
    let tables = usize::from(u16_at(&directory, 4));
    let records = read_at(file, u64::from(font) + 12, 16 * tables)?;
    let Some(name) = (0..tables)
        .map(|table| &records[16 * table..16 * table + 16])
        .find(|record| &record[..4] == b"name")
    else {
        return Ok(None);
    };
    let (offset, length) = (u32_at(name, 8), u32_at(name, 12) as usize);
    if !(6..=1 << 20).contains(&length) {
        return Ok(None);
    }
    let table = read_at(file, u64::from(offset), length)?;
    let (count, strings) = (
        usize::from(u16_at(&table, 2)),
        usize::from(u16_at(&table, 4)),
    );
    let mut best: Option<(bool, String)> = None;
    for record in 0..count {
        let at = 6 + 12 * record;
        if at + 12 > table.len() {
            break;
        }
        let (platform, language, name_id) = (
            u16_at(&table, at),
            u16_at(&table, at + 4),
            u16_at(&table, at + 6),
        );
        if platform != 3 || name_id != 1 {
            continue;
        }
        let (size, start) = (
            usize::from(u16_at(&table, at + 8)),
            strings + usize::from(u16_at(&table, at + 10)),
        );
        let Some(bytes) = table.get(start..start + size) else {
            continue;
        };
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
            .collect();
        let Ok(text) = String::from_utf16(&units) else {
            continue;
        };
        let english = language == 0x0409;
        if best.as_ref().is_none_or(|(found, _)| english && !found) {
            best = Some((english, text));
        }
    }
    Ok(best.map(|(_, text)| text).filter(|text| !text.is_empty()))
}

/// Log lines: which phone font each main language uses, how many languages
/// have a font, and the main languages the phone has no font for.
pub(crate) fn log_lines(fonts: &[PhoneFont]) -> Vec<String> {
    let mut lines = vec![format!(
        "pd2-launcher: the phone's fonts for Wine: {} language{}",
        fonts.len(),
        if fonts.len() == 1 { "" } else { "s" }
    )];
    let mut missing = Vec::new();
    for (tag, name) in MAIN_LANGUAGES {
        match fonts.iter().find(|font| font.first_language() == tag) {
            Some(font) => lines.push(format!(
                "pd2-launcher: {name}: {} ({})",
                font.path.display(),
                font.family
            )),
            None => missing.push(name),
        }
    }
    if !missing.is_empty() {
        lines.push(format!(
            "pd2-launcher: the phone has no font for: {}; Wine shows boxes for these letters",
            missing.join(", ")
        ));
    }
    lines
}

fn reg_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', r"\\").replace('"', "\\\""))
}

fn reg_multi_string(entries: &[String]) -> String {
    let mut units: Vec<u16> = Vec::new();
    for entry in entries {
        units.extend(entry.encode_utf16());
        units.push(0);
    }
    units.push(0);
    let bytes: Vec<String> = units
        .iter()
        .flat_map(|unit| unit.to_le_bytes())
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("hex(7):{}", bytes.join(","))
}

/// The `reg import` text (version 5) of the fonts' registry values, without
/// the marker, and the marker value: the SHA-256 of that text.
fn values_text(fonts: &[PhoneFont]) -> String {
    let machine = r"HKEY_LOCAL_MACHINE\Software\Microsoft\Windows NT\CurrentVersion";
    let mut text = String::new();
    text.push_str(&format!("[{machine}\\Fonts]\r\n"));
    let mut files: Vec<&PathBuf> = Vec::new();
    for font in fonts {
        if files.contains(&&font.path) {
            continue;
        }
        files.push(&font.path);
        let windows = format!("Z:{}", font.path.display().to_string().replace('/', "\\"));
        text.push_str(&format!(
            "{}={}\r\n",
            reg_string(&format!("{} (TrueType)", font.family)),
            reg_string(&windows)
        ));
    }
    let mut links: Vec<String> = Vec::new();
    for font in fonts {
        let link = format!("{},{}", font.file_name(), font.family);
        if !links.contains(&link) {
            links.push(link);
        }
    }
    text.push_str(&format!("\r\n[{machine}\\FontLink\\SystemLink]\r\n"));
    for linked in LINKED_FONTS {
        text.push_str(&format!(
            "{}={}\r\n",
            reg_string(linked),
            reg_multi_string(&links)
        ));
    }
    text.push_str(&format!("\r\n[{machine}\\FontSubstitutes]\r\n"));
    for (tag, names) in WINDOWS_NAMES {
        let Some(font) = fonts.iter().find(|font| font.first_language() == tag) else {
            continue;
        };
        for name in names {
            text.push_str(&format!(
                "{}={}\r\n",
                reg_string(name),
                reg_string(&font.family)
            ));
        }
    }
    text
}

/// The `reg import` file (UTF-16 with its byte order mark, as Windows'
/// version 5 files are) and the marker it sets: the hex SHA-256 of the
/// values. Nothing when the list has no font.
pub(crate) fn import_file(fonts: &[PhoneFont]) -> Option<(Vec<u8>, String)> {
    if fonts.is_empty() {
        return None;
    }
    let values = values_text(fonts);
    let marker = format!("{:x}", Sha256::digest(values.as_bytes()));
    let text = format!(
        "Windows Registry Editor Version 5.00\r\n\r\n{values}\r\n[HKEY_LOCAL_MACHINE\\{MARKER_KEY}]\r\n{}={}\r\n",
        reg_string(MARKER_VALUE),
        reg_string(&marker)
    );
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    Some((bytes, marker))
}

/// Whether the text of the prefix's `system.reg` has the `marker` already.
pub(crate) fn applied(system_reg: &str, marker: &str) -> bool {
    let section = format!("[{}]", MARKER_KEY.replace('\\', r"\\")).to_ascii_lowercase();
    let wanted = format!("\"{MARKER_VALUE}\"=\"{marker}\"").to_ascii_lowercase();
    let mut in_section = false;
    for line in system_reg.lines().map(str::trim_start) {
        if line.starts_with('[') {
            in_section = line.to_ascii_lowercase().starts_with(&section);
        } else if in_section && line.to_ascii_lowercase() == wanted {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font(languages: &str, path: &str, family: &str) -> PhoneFont {
        PhoneFont {
            languages: languages.to_owned(),
            path: PathBuf::from(path),
            family: family.to_owned(),
        }
    }

    fn fonts() -> Vec<PhoneFont> {
        vec![
            font(
                "zh-Hans",
                "/system/fonts/NotoSansCJK-Regular.ttc",
                "Noto Sans CJK SC",
            ),
            font(
                "zh-Hant,zh-Bopo",
                "/system/fonts/NotoSansCJK-Regular.ttc",
                "Noto Sans CJK TC",
            ),
            font(
                "und-Thai",
                "/system/fonts/NotoSansThai-Regular.ttf",
                "Noto Sans Thai",
            ),
        ]
    }

    fn import_text(fonts: &[PhoneFont]) -> String {
        let (bytes, _) = import_file(fonts).unwrap();
        assert_eq!(&bytes[..2], &[0xff, 0xfe]);
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn the_list_keeps_its_order_and_names_unreadable_fonts() {
        let text = "zh-Hans\t2\t/f/cjk.ttc\nund-Thai\t0\t/f/thai.ttf\nbad line\nko\tx\t/f/cjk.ttc\nund-Arab\t0\trelative.ttf\n";
        let (fonts, unreadable) = from_list(text, |path, index| {
            (path == Path::new("/f/cjk.ttc") && index == 2).then(|| "Noto Sans CJK SC".to_owned())
        });
        assert_eq!(
            fonts,
            vec![font("zh-Hans", "/f/cjk.ttc", "Noto Sans CJK SC")]
        );
        assert_eq!(unreadable, vec!["und-Thai /f/thai.ttf".to_owned()]);
    }

    #[test]
    fn each_file_is_loaded_once_and_every_font_is_linked_in_order() {
        let text = import_text(&fonts());
        assert!(text.starts_with("Windows Registry Editor Version 5.00\r\n"));
        assert_eq!(text.matches("NotoSansCJK-Regular.ttc\"").count(), 1);
        assert!(text.contains(
            r#""Noto Sans CJK SC (TrueType)"="Z:\\system\\fonts\\NotoSansCJK-Regular.ttc""#
        ));
        let links = reg_multi_string(&[
            "NotoSansCJK-Regular.ttc,Noto Sans CJK SC".to_owned(),
            "NotoSansCJK-Regular.ttc,Noto Sans CJK TC".to_owned(),
            "NotoSansThai-Regular.ttf,Noto Sans Thai".to_owned(),
        ]);
        for linked in LINKED_FONTS {
            assert!(text.contains(&format!("\"{linked}\"={links}\r\n")));
        }
    }

    #[test]
    fn windows_language_fonts_give_the_phone_font_of_their_language() {
        let text = import_text(&fonts());
        assert!(text.contains("\"SimSun\"=\"Noto Sans CJK SC\""));
        assert!(text.contains("\"\u{5fae}\u{8f6f}\u{96c5}\u{9ed1}\"=\"Noto Sans CJK SC\""));
        // Traditional Chinese has no names here: PMingLiU stays as it is.
        assert!(!text.contains("\"PMingLiU\""));
    }

    #[test]
    fn the_marker_changes_with_the_fonts_and_is_found_as_wine_writes_it() {
        let (_, marker) = import_file(&fonts()).unwrap();
        let (_, other) = import_file(&fonts()[..1]).unwrap();
        assert_ne!(marker, other);
        let system_reg = format!(
            "[Software\\\\Microsoft] 1\n\"x\"=\"y\"\n\n[Software\\\\PD2 Launcher] 1794202008\n#time=1dd\n\"PhoneFonts\"=\"{marker}\"\n"
        );
        assert!(applied(&system_reg, &marker));
        assert!(!applied(&system_reg, &other));
        assert!(!applied("", &marker));
        assert!(import_file(&[]).is_none());
    }

    #[test]
    fn androids_full_tags_match_without_their_region() {
        assert_eq!(language_key("zh-Hans-CN"), "zh-Hans");
        assert_eq!(language_key("zh-Hant-TW"), "zh-Hant");
        assert_eq!(language_key("zh-TW"), "zh-Hant");
        assert_eq!(language_key("ja-Jpan-JP"), "ja");
        assert_eq!(language_key("ko-Kore-KR"), "ko");
        assert_eq!(language_key("und-Thai"), "und-Thai");
        let fonts = [font(
            "zh-Hans-CN",
            "/system/fonts/SysSans-Hans-Regular.ttf",
            "OPlusSans SC",
        )];
        let text = import_text(&fonts);
        assert!(text.contains("\"SimSun\"=\"OPlusSans SC\""));
        assert!(
            !log_lines(&fonts)
                .iter()
                .any(|line| line.contains("no font for"))
        );
    }

    #[test]
    fn the_log_names_each_main_language_or_says_it_is_missing() {
        let lines = log_lines(&fonts());
        assert_eq!(
            lines[0],
            "pd2-launcher: the phone's fonts for Wine: 3 languages"
        );
        assert!(lines.contains(
            &"pd2-launcher: Chinese (Simplified): /system/fonts/NotoSansCJK-Regular.ttc (Noto Sans CJK SC)"
                .to_owned()
        ));
        assert_eq!(lines.len(), 2);
        assert_eq!(
            log_lines(&fonts()[2..]).last().unwrap(),
            "pd2-launcher: the phone has no font for: Chinese (Simplified); Wine shows boxes for these letters"
        );
    }

    /// A one-font file with only a `name` table, as the parser reads it.
    fn font_file(names: &[(u16, u16, &str)]) -> Vec<u8> {
        let mut strings = Vec::new();
        let mut records = Vec::new();
        for (language, name_id, text) in names {
            let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
            for value in [
                3,
                1,
                *language,
                *name_id,
                bytes.len() as u16,
                strings.len() as u16,
            ] {
                records.extend(value.to_be_bytes());
            }
            strings.extend(bytes);
        }
        let mut name = Vec::new();
        for value in [0, names.len() as u16, (6 + records.len()) as u16] {
            name.extend(value.to_be_bytes());
        }
        name.extend(records);
        name.extend(strings);
        let mut file = vec![0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        file.extend(b"name");
        file.extend(0_u32.to_be_bytes());
        file.extend(28_u32.to_be_bytes());
        file.extend((name.len() as u32).to_be_bytes());
        file.extend(name);
        file
    }

    #[test]
    fn the_family_name_is_windows_name_1_in_us_english_first() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("font.ttf");
        std::fs::write(
            &path,
            font_file(&[
                (0x0804, 1, "\u{601d}\u{6e90}\u{9ed1}\u{4f53}"),
                (0x0409, 4, "Noto Sans CJK SC Regular"),
                (0x0409, 1, "Noto Sans CJK SC"),
            ]),
        )
        .unwrap();
        assert_eq!(family_name(&path, 0).as_deref(), Some("Noto Sans CJK SC"));
        assert_eq!(family_name(&path, 1), None);
        std::fs::write(&path, font_file(&[(0x0804, 1, "\u{601d}\u{6e90}")])).unwrap();
        assert_eq!(family_name(&path, 0).as_deref(), Some("\u{601d}\u{6e90}"));
    }
}
