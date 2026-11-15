//! What a dev build adds for testing, and a release build leaves out.
//!
//! Every build is a dev build unless it is made with the `release-build`
//! feature, which the build scripts turn on for `PD2_RELEASE_BUILD=1`. A dev
//! build prefills the CD keys it was built with and has Wine say more in its
//! logs.

/// Whether this build carries the testing additions.
pub(crate) const DEV_BUILD: bool = !cfg!(feature = "release-build");

/// Wine's debug channels for the game: in a dev build, which DLLs load, when,
/// in which process, and the exceptions they raise.
pub(crate) const GAME_WINEDEBUG: &str = if DEV_BUILD {
    "fixme-all,+loaddll,+timestamp,+pid,warn+seh"
} else {
    "fixme-all"
};

/// Wine's debug channels for setup steps; the same in a dev build.
pub(crate) const SETUP_WINEDEBUG: &str = if DEV_BUILD {
    "fixme-all,+pid,+timestamp,+loaddll,warn+seh"
} else {
    "fixme-all"
};

/// The CD keys a dev build was compiled with (`PD2_DEV_D2_KEY` and
/// `PD2_DEV_D2_LOD_KEY`, which the container build takes from the build
/// machine's `.env.local`); a release build has none.
pub(crate) fn product_keys() -> Option<(&'static str, &'static str)> {
    #[cfg(not(feature = "release-build"))]
    {
        option_env!("PD2_DEV_D2_KEY").zip(option_env!("PD2_DEV_D2_LOD_KEY"))
    }
    #[cfg(feature = "release-build")]
    {
        None
    }
}

/// The file in the Managed root that names a locale (for example
/// `zh_CN.UTF-8`) for the game's `LC_CTYPE`, with `LOCPATH` set to the
/// Managed root's `devlocale` folder, which holds that compiled locale: a dev
/// build's test of a game that needs another Windows code page (issue 19:
/// the Chinese server's client needs 936). A release build never reads it.
pub(crate) const GAME_LOCALE_FILE: &str = ".pd2launcher-dev-game-locale";

/// The game's `LC_CTYPE` and `LOCPATH` from [`GAME_LOCALE_FILE`] in the
/// Managed `root` at `root_path`; none in a release build or without a plain
/// locale name (letters, digits, `_`, `.`, `-`, `@`).
pub(crate) fn game_locale(
    root: &crate::safe_fs::Directory,
    root_path: &std::path::Path,
) -> Vec<(String, String)> {
    if !DEV_BUILD {
        return Vec::new();
    }
    let mut text = String::new();
    let Ok(file) = root.open_file(std::path::Path::new(GAME_LOCALE_FILE)) else {
        return Vec::new();
    };
    if std::io::Read::read_to_string(&mut std::io::Read::take(file, 64), &mut text).is_err() {
        return Vec::new();
    }
    let name = text.trim();
    if name.is_empty()
        || !name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-' | '@')
        })
    {
        return Vec::new();
    }
    vec![
        ("LC_CTYPE".to_owned(), name.to_owned()),
        (
            "LOCPATH".to_owned(),
            root_path.join("devlocale").display().to_string(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_build_logs_plainly_and_has_no_keys() {
        if DEV_BUILD {
            assert!(GAME_WINEDEBUG.contains("+loaddll"));
            assert!(SETUP_WINEDEBUG.contains("+loaddll"));
        } else {
            assert_eq!(GAME_WINEDEBUG, "fixme-all");
            assert_eq!(SETUP_WINEDEBUG, "fixme-all");
            assert_eq!(product_keys(), None);
        }
    }
}
