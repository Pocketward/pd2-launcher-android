//! The language of a Diablo II copy, which Project Diablo 2 needs to be
//! English.
//!
//! Diablo II reads its language from the one-byte member `data\local\use` of
//! its archives (D2Lang's language ID: 0 English, 9 Traditional Chinese, and
//! so on). The installer of each language writes it into `d2data.mpq`; a
//! patch archive can carry it too, and Storm opens the patch first.
//!
//! PD2 runs on an English copy only (its Support FAQ: "Make sure you have an
//! English version of Diablo 2 + LoD installed" for the Halt error); on
//! another language the game can halt. The Launcher does not change the
//! game's files, so it only says so (issue 19). D2GL's HD text has no glyphs
//! for Chinese, Japanese or Korean in PD2's `d2gl.mpq` (its atlas lists name
//! Noto CJK glyph sets that the archive does not hold), so on these
//! languages every letter shows as an empty box; the game's own fonts show
//! them ([`GameLanguage::needs_game_fonts`]).

use std::path::Path;

use crate::{blizzard::Archive, safe_fs::Directory};

/// The member that holds the language ID.
const MEMBER: &str = "data\\local\\use";

/// The archives that can hold [`MEMBER`], the one Storm opens first first:
/// PD2's own patch, the game's patch, the expansion, the base game.
const ARCHIVES: [&str; 4] = [
    "ProjectD2/patch_d2.mpq",
    "patch_d2.mpq",
    "d2exp.mpq",
    "d2data.mpq",
];

/// A language ID of D2Lang, as `data\local\use` holds it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GameLanguage(u8);

impl GameLanguage {
    pub(crate) const ENGLISH: Self = Self(0);
    #[cfg(test)]
    pub(crate) const CHINESE: Self = Self(9);

    /// The language of the copy in the game `folder`: the first archive of
    /// [`ARCHIVES`] that holds [`MEMBER`] names it. `None` when no archive
    /// holds a readable one.
    pub(crate) fn of(folder: &Directory) -> Option<Self> {
        ARCHIVES.iter().find_map(|name| {
            let archive = Archive::open(folder.open_file(Path::new(name)).ok()?).ok()?;
            match archive.member(MEMBER).ok()?.as_slice() {
                [id] => Some(Self(*id)),
                _ => None,
            }
        })
    }

    /// The language of the installed copy below the Managed `root` (the
    /// Managed game folder, or the one used in place). `None` when there is
    /// none or it cannot be read.
    pub(crate) fn of_installation(root: &Directory) -> Option<Self> {
        let folder = crate::game_folder::GameFolder::resolve(root).ok()?;
        Self::of(&folder.open(root).ok()?)
    }

    pub(crate) fn is_english(self) -> bool {
        self == Self::ENGLISH
    }

    /// Chinese, Japanese or Korean: D2GL's HD text cannot show it with PD2's
    /// `d2gl.mpq`, so the game's own fonts must.
    pub(crate) fn needs_game_fonts(self) -> bool {
        matches!(self.0, 6..=9)
    }

    /// D2Lang's three-letter code (`chi`, `kor`, ...), or `unknown` for an ID
    /// it does not name. This is the control channel's value.
    pub(crate) fn code(self) -> &'static str {
        match self.0 {
            0 => "eng",
            1 => "esp",
            2 => "deu",
            3 => "fra",
            4 => "por",
            5 => "ita",
            6 => "jpn",
            7 => "kor",
            8 => "sin",
            9 => "chi",
            10 => "pol",
            11 => "rus",
            _ => "unknown",
        }
    }

    /// The language's English name, for the log and the desktop's notice.
    pub(crate) fn name(self) -> &'static str {
        match self.0 {
            0 => "English",
            1 => "Spanish",
            2 => "German",
            3 => "French",
            4 => "Portuguese",
            5 => "Italian",
            6 => "Japanese",
            7 => "Korean",
            8 => "Simplified Chinese",
            9 => "Traditional Chinese",
            10 => "Polish",
            11 => "Russian",
            _ => "an unknown language",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blizzard::build_test_archive;

    /// An archive's members: name, data, compressed.
    type Members<'a> = &'a [(&'a str, &'a [u8], bool)];

    fn folder(archives: &[(&str, Members<'_>)]) -> (tempfile::TempDir, Directory) {
        let temporary = tempfile::tempdir().expect("temporary folder");
        std::fs::create_dir(temporary.path().join("ProjectD2")).expect("ProjectD2");
        for (name, members) in archives {
            std::fs::write(
                temporary.path().join(name),
                build_test_archive(members, 16, 3),
            )
            .expect("archive");
        }
        let directory = Directory::open(temporary.path()).expect("folder");
        (temporary, directory)
    }

    #[test]
    fn the_base_archive_names_the_language() {
        let (_temporary, english) = folder(&[("d2data.mpq", &[(MEMBER, &[0], false)])]);
        assert_eq!(GameLanguage::of(&english), Some(GameLanguage::ENGLISH));

        let (_temporary, chinese) = folder(&[("d2data.mpq", &[(MEMBER, &[9], false)])]);
        let language = GameLanguage::of(&chinese).expect("language");
        assert!(!language.is_english());
        assert!(language.needs_game_fonts());
        assert_eq!(language.code(), "chi");
    }

    #[test]
    fn a_patch_archive_comes_before_the_base_archive() {
        let (_temporary, folder) = folder(&[
            ("patch_d2.mpq", &[(MEMBER, &[7], true)]),
            ("d2data.mpq", &[(MEMBER, &[0], false)]),
        ]);
        assert_eq!(
            GameLanguage::of(&folder).map(GameLanguage::code),
            Some("kor")
        );
    }

    #[test]
    fn archives_without_the_member_are_passed_over() {
        let (_temporary, folder) = folder(&[
            (
                "ProjectD2/patch_d2.mpq",
                &[("data\\global\\x.txt", b"x", false)],
            ),
            (
                "d2exp.mpq",
                &[("data\\local\\lng\\eng\\patchstring.tbl", b"t", false)],
            ),
            ("d2data.mpq", &[(MEMBER, &[2], false)]),
        ]);
        let language = GameLanguage::of(&folder).expect("language");
        assert_eq!(language.code(), "deu");
        assert!(!language.needs_game_fonts());
    }

    #[test]
    fn no_readable_member_is_no_language() {
        let (_temporary, empty) = folder(&[]);
        assert_eq!(GameLanguage::of(&empty), None);
        let (_temporary, long) = folder(&[("d2data.mpq", &[(MEMBER, &[0, 0], false)])]);
        assert_eq!(GameLanguage::of(&long), None);
    }
}
