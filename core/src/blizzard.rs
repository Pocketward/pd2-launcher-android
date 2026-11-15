//! Blizzard's Diablo II and Lord of Destruction installers fetched without
//! Blizzard's downloader (the "direct download" install source).
//!
//! Blizzard's legacy downloader wrapper carries a torrent and a signed CDN
//! access cookie (valid for about a week). The torrent's `direct download`
//! host serves the payload as one numbered file per piece, which any order
//! and any number of connections can fetch; each piece is checked against
//! its SHA-1 from the torrent. The finished files are the same complete
//! installer Blizzard's downloader saves, and the same identity check
//! (sizes and SHA-256) accepts them.
//!
//! A translation of the Diablo II parts of blizzard-legacy-dl (MIT,
//! Copyright (c) 2026 jaenster; `src/blizzard/LICENSE`). Nothing of
//! Blizzard's is embedded: the torrent always comes from the wrapper.
//!
//! The piece host is plain HTTP: it presents no certificate for its name
//! (Akamai's own, for `a248.e.akamai.net`). It is the one built-in source
//! with that exception, and only for the pinned host and paths below: every
//! piece must match its SHA-1 in a torrent whose infohash is pinned, and the
//! finished files their SHA-256 pins.

mod connections;
mod fetch;
mod keys;
mod keystore;
mod mpq;
mod native;
mod script;
mod torrent;

pub(crate) use fetch::{Cancelled, Download, HttpTransport, Limits, Outcome, PayloadFile, fetch};
pub(crate) use keys::{KeyField, KeyProblem, NormalizedKey, check as check_key};
#[cfg(test)]
pub(crate) use keys::{TEST_BASE_KEY, TEST_EXPANSION_KEY, check_without_test_keys};
pub(crate) use mpq::Archive;
#[cfg(test)]
pub(crate) use mpq::tests::build as build_test_archive;
pub(crate) use native::{
    BASE_GAME as BASE_GAME_PLAN, EXPANSION as EXPANSION_PLAN, NativeError, SCRIPT_MEMBER,
    check_plan, run as run_native,
};
pub(crate) use script::plan as read_script;
pub(crate) use torrent::{AccessCookie, Torrent};

/// The one host pieces come from, on port 80, under this path.
pub(crate) const PIECE_HOST: &str = "rogue.blizzard.com.edgesuite.net";
pub(crate) const PIECE_PATH_PREFIX: &str = "/applications/Diablo2/1.14B/";

/// What the direct download accepts for one product.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProductPin {
    /// The SHA-1 of the torrent's `info` dictionary: its name, piece size,
    /// file list and every piece's SHA-1.
    pub(crate) infohash: &'static str,
    /// The payload's folder name, the same as Blizzard's downloader saves.
    pub(crate) name: &'static str,
    pub(crate) piece_length: u64,
    pub(crate) pieces: usize,
    pub(crate) total: u64,
    /// The only piece server: `<base>/<n>` is piece `n`.
    pub(crate) base: &'static str,
}

/// Diablo II 1.14b, Windows, English (`D2DV`).
pub(crate) const BASE_GAME: ProductPin = ProductPin {
    infohash: "84ce44e25d34d9a292d7aad5d31e54e866c10c9c",
    name: "D2-1.14b-Installer-enUS",
    piece_length: 262_144,
    pieces: 6_088,
    total: 1_595_721_859,
    base: "http://rogue.blizzard.com.edgesuite.net/applications/Diablo2/1.14B/D2/enUS",
};

/// Lord of Destruction 1.14b, Windows, English (`D2XP`).
pub(crate) const EXPANSION: ProductPin = ProductPin {
    infohash: "82c051339f174d5cf9c383bbf752b9b6b1db2b54",
    name: "D2LOD-1.14b-Installer-enUS",
    piece_length: 262_144,
    pieces: 2_039,
    total: 534_264_111,
    base: "http://rogue.blizzard.com.edgesuite.net/applications/Diablo2/1.14B/LOD/enUS",
};

/// The files of a payload that are not kept: the Mac installer.
pub(crate) const SKIPPED_PREFIX: &str = "Installer.app/";

/// Which part of a torrent differs from its pin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mismatch {
    Infohash,
    Name,
    Pieces,
    Server,
    Files,
}

/// Check a wrapper's torrent against its pin. `kept` names each file that
/// is written, with its pinned size; every other file must be part of the
/// Mac installer, which is not.
pub(crate) fn check_pin(
    torrent: &Torrent,
    pin: &ProductPin,
    kept: &[(&str, u64)],
) -> Result<(), Mismatch> {
    if torrent.infohash_hex() != pin.infohash {
        return Err(Mismatch::Infohash);
    }
    if torrent.name != pin.name {
        return Err(Mismatch::Name);
    }
    if torrent.piece_length != pin.piece_length
        || torrent.piece_count() != pin.pieces
        || torrent.total != pin.total
    {
        return Err(Mismatch::Pieces);
    }
    if !matches!(
        torrent.servers.as_slice(),
        [server] if server.url == pin.base && server.first == 0 && server.last == u64::MAX
    ) {
        return Err(Mismatch::Server);
    }
    for (path, length) in kept {
        let mut found = torrent.files.iter().filter(|file| file.path == *path);
        if !matches!((found.next(), found.next()), (Some(file), None) if file.length == *length) {
            return Err(Mismatch::Files);
        }
    }
    if !torrent.files.iter().all(|file| {
        kept.iter().any(|(path, _)| *path == file.path) || file.path.starts_with(SKIPPED_PREFIX)
    }) {
        return Err(Mismatch::Files);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use torrent::tests::{FAKE_COOKIE, stub_bytes, torrent_bytes};

    const BASE: &str = "http://pieces.invalid/applications/x/enUS";

    fn synthetic(files: &[(&str, usize)], base: &str) -> Torrent {
        let length = files.iter().map(|(_, length)| length).sum::<usize>();
        let payload = vec![7_u8; length];
        Torrent::from_stub(&stub_bytes(
            &torrent_bytes("Synthetic-Installer", base, 64, files, &payload),
            FAKE_COOKIE,
        ))
        .unwrap()
    }

    fn pin_of(torrent: &Torrent) -> ProductPin {
        ProductPin {
            infohash: Box::leak(torrent.infohash_hex().into_boxed_str()),
            name: "Synthetic-Installer",
            piece_length: 64,
            pieces: torrent.piece_count(),
            total: torrent.total,
            base: BASE,
        }
    }

    const FILES: [(&str, usize); 4] = [
        ("DirectX/a.cab", 100),
        ("Installer Tome.mpq", 300),
        ("Installer.app/Contents/x", 50),
        ("Installer.exe", 70),
    ];
    const KEPT: [(&str, u64); 3] = [
        ("DirectX/a.cab", 100),
        ("Installer Tome.mpq", 300),
        ("Installer.exe", 70),
    ];

    #[test]
    fn a_torrent_that_matches_its_pin_passes() {
        let torrent = synthetic(&FILES, BASE);
        assert_eq!(check_pin(&torrent, &pin_of(&torrent), &KEPT), Ok(()));
    }

    #[test]
    fn each_difference_from_the_pin_fails_closed() {
        let torrent = synthetic(&FILES, BASE);
        let pin = pin_of(&torrent);
        let other = ProductPin {
            infohash: "0000000000000000000000000000000000000000",
            ..pin
        };
        assert_eq!(check_pin(&torrent, &other, &KEPT), Err(Mismatch::Infohash));
        let other = ProductPin {
            name: "Other",
            ..pin
        };
        assert_eq!(check_pin(&torrent, &other, &KEPT), Err(Mismatch::Name));
        let other = ProductPin { pieces: 1, ..pin };
        assert_eq!(check_pin(&torrent, &other, &KEPT), Err(Mismatch::Pieces));
        let other = ProductPin {
            base: "http://elsewhere.invalid/x",
            ..pin
        };
        assert_eq!(check_pin(&torrent, &other, &KEPT), Err(Mismatch::Server));
        // A kept file of another size, or one missing.
        let mut kept = KEPT;
        kept[2].1 = 71;
        assert_eq!(check_pin(&torrent, &pin, &kept), Err(Mismatch::Files));
        assert_eq!(
            check_pin(
                &torrent,
                &pin,
                &[("Missing.exe", 1), KEPT[0], KEPT[1], KEPT[2]]
            ),
            Err(Mismatch::Files)
        );
        // A file that is neither kept nor the Mac installer's.
        assert_eq!(check_pin(&torrent, &pin, &KEPT[..2]), Err(Mismatch::Files));
        // Two servers.
        let torrent = synthetic(&FILES, &format!("{BASE}|{BASE}"));
        assert_eq!(
            check_pin(&torrent, &pin_of(&torrent), &KEPT),
            Err(Mismatch::Server)
        );
    }

    #[test]
    fn the_pins_name_the_one_piece_host() {
        for pin in [BASE_GAME, EXPANSION] {
            assert_eq!(pin.infohash.len(), 40);
            assert!(
                pin.base
                    .starts_with(&format!("http://{PIECE_HOST}{PIECE_PATH_PREFIX}"))
            );
            assert_eq!(pin.total.div_ceil(pin.piece_length), pin.pieces as u64);
        }
    }
}
