//! How Diablo II keeps the CD key and the owner name: each is an MPQ member
//! (named like an ordinary asset) holding the text in Blizzard's own
//! wrapper. The installer writes them; the game reads them.
//!
//! The wrapper: 64-byte blocks and an 8-byte trailer, every eighth block
//! through IDEA, a running digest (a SHA-1 variant) subtracted across each
//! block backwards, and every sixteenth plaintext block folded back into the
//! digest. The trailer holds four bytes of the final digest. The password and
//! the schedule's start come from the C runtime's generator with fixed
//! seeds, so every install gives the same bytes for the same text.
//!
//! Translated from libd2's `packages/bnet/src/keystore.zig` and `idea.zig`
//! (MIT, Copyright (c) 2026 jaenster; `LICENSE-libd2` beside this file).

/// The member that holds the Diablo II key in `d2data.mpq`.
pub(crate) const BASE_KEY_MEMBER: &str = r"data\global\sfx\cursor\wavindx.wav";
/// The member that holds the Lord of Destruction key in `d2char.mpq`.
pub(crate) const EXPANSION_KEY_MEMBER: &str = r"data\global\chars\am\cof\amblxbow.cof";
/// The member that holds the owner name, in both archives.
pub(crate) const OWNER_MEMBER: &str = r"data\global\sfx\cursor\curindx.wav";
/// The owner name the engine installs with.
pub(crate) const OWNER: &str = "Player";

const TRAILER: usize = 8;
const BLOCK: usize = 64;

/// The Microsoft C runtime's `rand`: its values are part of the format.
struct Rand(u32);

impl Rand {
    fn next_byte(&mut self) -> u8 {
        self.0 = self.0.wrapping_mul(214_013).wrapping_add(2_531_011);
        ((self.0 >> 16) & 0x7fff) as u8
    }
}

/// The 19-byte password, from `srand(0x150B)`; a zero is drawn again.
fn password() -> [u8; 19] {
    let mut rand = Rand(0x150b);
    let mut out = [0_u8; 19];
    let mut filled = 0;
    while filled < out.len() {
        let byte = rand.next_byte();
        if byte != 0 {
            out[filled] = byte;
            filled += 1;
        }
    }
    out
}

/// The 112 bytes the schedule starts from, from `srand(0x4FA7)`.
fn schedule_seed() -> [u8; 112] {
    let mut rand = Rand(0x4fa7);
    std::array::from_fn(|_| rand.next_byte())
}

/// The password tiled across 64 bytes, restarting at the first NUL.
fn tile(password: &[u8]) -> [u8; 64] {
    let mut at = 0;
    std::array::from_fn(|_| {
        if at >= password.len() || password[at] == 0 {
            at = 0;
        }
        let byte = password[at];
        at += 1;
        byte
    })
}

/// The wrapper's running hash: SHA-1's rounds, but the message expansion
/// has no rotation and the two round rotations shift right arithmetically.
/// No padding: whole blocks only, read out between them.
#[derive(Clone)]
struct Hash([u32; 5]);

impl Hash {
    fn new() -> Self {
        Self([
            0x6745_2301,
            0xefcd_ab89,
            0x98ba_dcfe,
            0x1032_5476,
            0xc3d2_e1f0,
        ])
    }

    /// A left shift ORed with an arithmetic right shift, as the game does.
    fn smear(value: u32, left: u32) -> u32 {
        (value << left) | ((value as i32) >> (32 - left)) as u32
    }

    fn block(&mut self, data: &[u8; 64]) {
        let mut w = [0_u32; 80];
        for (index, word) in data.as_chunks::<4>().0.iter().enumerate() {
            w[index] = u32::from_le_bytes(*word);
        }
        for index in 16..80 {
            w[index] = w[index - 3] ^ w[index - 8] ^ w[index - 14] ^ w[index - 16];
        }
        let [mut a, mut b, mut c, mut d, mut e] = self.0;
        for (index, word) in w.iter().enumerate() {
            let (f, k) = match index / 20 {
                0 => ((b & c) | (!b & d), 0x5a82_7999),
                1 => (b ^ c ^ d, 0x6ed9_eba1),
                2 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let t = Self::smear(a, 5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = Self::smear(b, 30);
            b = a;
            a = t;
        }
        for (state, value) in self.0.iter_mut().zip([a, b, c, d, e]) {
            *state = state.wrapping_add(value);
        }
        // The schedule holds the text's words.
        zeroize::Zeroize::zeroize(&mut w);
    }

    fn digest(&self) -> [u8; 20] {
        let mut out = [0_u8; 20];
        for (chunk, value) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.0) {
            *chunk = value.to_le_bytes();
        }
        out
    }
}

type Subkeys = [u16; 52];

/// Multiplication modulo 2^16+1, a stored zero meaning 2^16.
fn mul(a: u16, b: u16) -> u16 {
    if a == 0 {
        return 1_u32.wrapping_sub(u32::from(b)) as u16;
    }
    if b == 0 {
        return 1_u32.wrapping_sub(u32::from(a)) as u16;
    }
    let product = u32::from(a) * u32::from(b);
    let (low, high) = (product & 0xffff, product >> 16);
    low.wrapping_sub(high).wrapping_add(u32::from(low < high)) as u16
}

/// IDEA's schedule, every word little-endian: the key, then each group of
/// eight the one before rotated left 25 bits.
fn encrypt_schedule(key: &[u8; 16]) -> Subkeys {
    let mut subkeys = [0_u16; 52];
    for (index, word) in key.as_chunks::<2>().0.iter().enumerate() {
        subkeys[index] = u16::from_le_bytes(*word);
    }
    for index in 8..52 {
        let base = (index / 8 - 1) * 8;
        let a = subkeys[base + (index + 1) % 8];
        let b = subkeys[base + (index + 2) % 8];
        subkeys[index] = (a << 9) | (b >> 7);
    }
    subkeys
}

/// One IDEA block, words little-endian.
fn idea(subkeys: &Subkeys, input: [u8; 8]) -> [u8; 8] {
    let word = |index: usize| u16::from_le_bytes([input[index * 2], input[index * 2 + 1]]);
    let (mut x0, mut x1, mut x2, mut x3) = (word(0), word(1), word(2), word(3));
    for round in subkeys[..48].as_chunks::<6>().0 {
        x0 = mul(x0, round[0]);
        x1 = x1.wrapping_add(round[1]);
        x2 = x2.wrapping_add(round[2]);
        x3 = mul(x3, round[3]);
        let t0 = mul(x0 ^ x2, round[4]);
        let t1 = mul(t0.wrapping_add(x1 ^ x3), round[5]);
        let t2 = t0.wrapping_add(t1);
        x0 ^= t1;
        x3 ^= t2;
        let swap = x1 ^ t2;
        x1 = x2 ^ t1;
        x2 = swap;
    }
    let mut out = [0_u8; 8];
    for (chunk, value) in out.as_chunks_mut::<2>().0.iter_mut().zip([
        mul(x0, subkeys[48]),
        x2.wrapping_add(subkeys[49]),
        x1.wrapping_add(subkeys[50]),
        mul(x3, subkeys[51]),
    ]) {
        *chunk = value.to_le_bytes();
    }
    out
}

/// The first IDEA schedule and the opening hash, from the password.
fn schedule(password: &[u8]) -> (Subkeys, Hash) {
    let mut stream = schedule_seed();
    let mut seed = Hash::new();
    seed.block(&tile(password));
    let digest = seed.digest();
    for (index, byte) in stream.iter_mut().enumerate() {
        *byte ^= digest[index % 20];
    }
    let key: [u8; 16] = stream[..16].try_into().unwrap_or([0; 16]);
    let mut hash = Hash::new();
    let opening: [u8; 64] = stream[48..112].try_into().unwrap_or([0; 64]);
    hash.block(&opening);
    (encrypt_schedule(&key), hash)
}

/// The size of the wrapper for `text`: the text and its NUL in whole
/// blocks, and the trailer.
pub(crate) fn wrapped_len(text_len: usize) -> usize {
    let length = text_len + 1;
    if length.is_multiple_of(BLOCK) {
        length + TRAILER
    } else {
        length + (BLOCK + TRAILER - length % BLOCK)
    }
}

/// Wrap `text` (a key or the owner name) as the installer does.
pub(crate) fn wrap(text: &[u8]) -> Vec<u8> {
    let mut out = vec![0_u8; wrapped_len(text.len())];
    out[..text.len()].copy_from_slice(text);
    let body = out.len() - TRAILER;
    let (subkeys, mut hash) = schedule(&password());
    for (number, block) in out[..body]
        .as_chunks_mut::<BLOCK>()
        .0
        .iter_mut()
        .enumerate()
    {
        let digest = hash.digest();
        let mut plain: [u8; BLOCK] = *block;
        for (index, byte) in block.iter_mut().enumerate() {
            *byte = byte.wrapping_add(digest[(63 - index) % 20]);
        }
        if number & 7 == 0 {
            for chunk in block.as_chunks_mut::<8>().0 {
                *chunk = idea(&subkeys, *chunk);
            }
        }
        if number & 0xf == 0 {
            hash.block(&plain);
        }
        zeroize::Zeroize::zeroize(&mut plain);
    }
    let mac = hash.digest();
    out[body..body + 4].copy_from_slice(&mac[..4]);
    let consumed = text.len() + 1 - (body / BLOCK - 1) * BLOCK;
    out[body + 4] = 0;
    out[body + 5] = consumed as u8;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// IDEA's decryption subkeys, for the round trip only.
    fn decrypt_schedule(key: &[u8; 16]) -> Subkeys {
        fn inverse(x: u16) -> u16 {
            if x <= 1 {
                return x;
            }
            let (mut t0, mut t1) = (1_u32, 0x10001 / u32::from(x));
            let (mut v, mut y) = (u32::from(x), 0x10001 % u32::from(x));
            if y == 1 {
                return 1_u32.wrapping_sub(t1) as u16;
            }
            while y != 1 {
                let q = v / y;
                v %= y;
                t0 = t0.wrapping_add(q * t1);
                if v == 1 {
                    return t0 as u16;
                }
                let q = y / v;
                y %= v;
                t1 = t1.wrapping_add(q * t0);
            }
            1_u32.wrapping_sub(t1) as u16
        }
        let e = encrypt_schedule(key);
        let mut d = [0_u16; 52];
        let (mut p, mut q) = (0, 52);
        d[p] = inverse(e[q - 4]);
        d[p + 1] = e[q - 3].wrapping_neg();
        d[p + 2] = e[q - 2].wrapping_neg();
        d[p + 3] = inverse(e[q - 1]);
        p += 4;
        q -= 4;
        for _ in 0..7 {
            d[p] = e[q - 2];
            d[p + 1] = e[q - 1];
            p += 2;
            q -= 2;
            d[p] = inverse(e[q - 4]);
            d[p + 1] = e[q - 2].wrapping_neg();
            d[p + 2] = e[q - 3].wrapping_neg();
            d[p + 3] = inverse(e[q - 1]);
            p += 4;
            q -= 4;
        }
        d[p] = e[q - 2];
        d[p + 1] = e[q - 1];
        p += 2;
        q -= 2;
        d[p] = inverse(e[q - 4]);
        d[p + 1] = e[q - 3].wrapping_neg();
        d[p + 2] = e[q - 2].wrapping_neg();
        d[p + 3] = inverse(e[q - 1]);
        d
    }

    /// The game's read path, to prove [`wrap`] can be read back.
    fn unwrap(blob: &[u8]) -> Option<Vec<u8>> {
        let body = blob.len().checked_sub(TRAILER)?;
        if body == 0 || body % BLOCK != 0 {
            return None;
        }
        let mut stream = schedule_seed();
        let mut seed = Hash::new();
        seed.block(&tile(&password()));
        let digest = seed.digest();
        for (index, byte) in stream.iter_mut().enumerate() {
            *byte ^= digest[index % 20];
        }
        let subkeys = decrypt_schedule(&stream[..16].try_into().ok()?);
        let mut hash = Hash::new();
        hash.block(&stream[48..112].try_into().ok()?);
        let mut out = blob[..body].to_vec();
        for (number, block) in out.as_chunks_mut::<BLOCK>().0.iter_mut().enumerate() {
            let digest = hash.digest();
            if number & 7 == 0 {
                for chunk in block.as_chunks_mut::<8>().0 {
                    *chunk = idea(&subkeys, *chunk);
                }
            }
            for (index, byte) in block.iter_mut().enumerate() {
                *byte = byte.wrapping_sub(digest[(63 - index) % 20]);
            }
            if number & 0xf == 0 {
                hash.block(block);
            }
        }
        let trailer = &blob[body..];
        if trailer[4] != 0 || trailer[..4] != hash.digest()[..4] {
            return None;
        }
        out.truncate(body - BLOCK + usize::from(trailer[5]));
        Some(out)
    }

    #[test]
    fn the_password_and_seed_are_the_same_on_every_install() {
        assert_eq!(
            password(),
            [
                0xde, 0x88, 0x2a, 0x14, 0xdb, 0x23, 0xe3, 0x8f, 0xb3, 0xfb, 0x7b, 0xa4, 0x22, 0xeb,
                0x34, 0x18, 0x22, 0x15, 0x7a
            ]
        );
        let seed = schedule_seed();
        assert_eq!(seed[..8], [0x43, 0xea, 0x16, 0xdb, 0x9e, 0x52, 0xb2, 0xcf]);
        assert_eq!(
            seed[104..],
            [0x75, 0x5d, 0x20, 0x13, 0x71, 0xe3, 0x5c, 0x39]
        );
    }

    #[test]
    fn the_hash_matches_the_game() {
        let mut hash = Hash::new();
        hash.block(&[0; 64]);
        assert_eq!(
            hash.digest(),
            [
                0xd4, 0x0b, 0xe5, 0x96, 0x94, 0xf9, 0xf4, 0x09, 0x96, 0xc1, 0xa6, 0x7c, 0x60, 0x32,
                0x41, 0xf3, 0xe3, 0x83, 0x9d, 0x0f
            ]
        );
        let mut hash = Hash::new();
        hash.block(&tile(&password()));
        assert_eq!(
            hash.digest(),
            [
                0x4e, 0x3a, 0x79, 0xda, 0xd2, 0x97, 0x28, 0x30, 0xe2, 0x00, 0x3d, 0x92, 0xf3, 0x54,
                0xd2, 0xf0, 0xe4, 0x8e, 0xa3, 0x9e
            ]
        );
        assert_eq!(Hash::smear(0x8000_0000, 5), 0xffff_fff0);
    }

    #[test]
    fn the_schedule_matches_the_game() {
        let (_, hash) = schedule(&password());
        let digest = hash.digest();
        let words: Vec<u32> = digest
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| u32::from_le_bytes(*chunk))
            .collect();
        assert_eq!(
            words,
            [
                0x846e_6d34,
                0x658a_d5c7,
                0xef9c_f2b5,
                0x2f88_02c6,
                0xa45b_7ed4
            ]
        );
        let mut stream = schedule_seed();
        let mut seed = Hash::new();
        seed.block(&tile(&password()));
        let digest = seed.digest();
        for (index, byte) in stream.iter_mut().enumerate() {
            *byte ^= digest[index % 20];
        }
        let subkeys = decrypt_schedule(&stream[..16].try_into().unwrap());
        assert_eq!(
            subkeys[..8],
            [
                0xa775, 0xacc1, 0x195c, 0x8293, 0xf287, 0xe665, 0x1a48, 0xad96
            ]
        );
        assert_eq!(subkeys[50..], [0x3ab4, 0x4f89]);
    }

    #[test]
    fn wrapped_text_comes_back_and_is_not_in_the_clear() {
        for text in [
            &b"Player"[..],
            b"FAKE-NOT-A-KEY-FAKE-NOT-A",
            &[b'x'; 63],
            &[b'y'; 64],
            &[b'z'; 1100],
        ] {
            let blob = wrap(text);
            assert_eq!(blob.len(), wrapped_len(text.len()));
            assert_eq!((blob.len() - TRAILER) % BLOCK, 0);
            if text.len() > 4 {
                assert!(!blob.windows(text.len()).any(|window| window == text));
            }
            let back = unwrap(&blob).unwrap();
            assert_eq!(&back[..text.len()], text);
            assert_eq!(back[text.len()], 0);
        }
        assert_eq!(wrapped_len(6), 72);
        assert_eq!(wrapped_len(26), 72);
        assert_eq!(wrapped_len(63), 72);
        assert_eq!(wrapped_len(64), 136);
        // The same text gives the same bytes every time.
        assert_eq!(wrap(OWNER.as_bytes()), wrap(OWNER.as_bytes()));
    }

    /// Owner members that Blizzard's installer wrote (the stored bytes of
    /// `OWNER_MEMBER`, one file each, separated by `,` in
    /// `PD2_DIRECT_DOWNLOAD_WRAPPED_OWNERS`) read back, and [`wrap`] of their
    /// text gives the same bytes. Only for owner members: never give it a
    /// key member. It prints only the text's length and whether it is
    /// [`OWNER`].
    #[test]
    #[ignore = "needs owner members from an official install"]
    fn owner_members_of_an_official_install_wrap_the_same() {
        let files = std::env::var("PD2_DIRECT_DOWNLOAD_WRAPPED_OWNERS").expect("owner members");
        for file in files.split(',') {
            let blob = std::fs::read(file).unwrap();
            let mut text = unwrap(&blob).expect("the installer's wrapper reads back");
            assert_eq!(text.pop(), Some(0));
            assert!(wrap(&text) == blob, "wrap gives the installer's bytes");
            eprintln!(
                "owner member: reads back, wraps to the same bytes; its text has {} characters; it is {OWNER:?}: {}",
                text.len(),
                text == OWNER.as_bytes()
            );
            zeroize::Zeroize::zeroize(&mut text);
        }
    }
}
