//! Diablo II and Lord of Destruction CD keys, checked as Blizzard's 1.14b
//! installers check them.
//!
//! Each installer's script names one key type and one product:
//! `<encrypt object="cdkey26" ... product_id="24"/>` for Diablo II and
//! `product_id="25"` for Lord of Destruction. So a key is 26 characters of
//! the key alphabet, and it is valid in a field when its decoded product is
//! that field's product: 24 or 25. The installers have no other check (no
//! checksum for these keys), and they do not take 16-character keys at all.
//! The installers do not remove dashes or spaces, and they accept lower case;
//! [`normalize`] removes the first two and makes upper case, so the key a
//! player types and the key an installer gets are the same.
//!
//! The decode is translated from libd2's `packages/bnet/src/cdkey.zig` (MIT,
//! Copyright (c) 2026 jaenster; `LICENSE-libd2` beside this file), and checked
//! against the installer's own code.

use zeroize::Zeroize;

/// The length of a key the installers take.
pub(crate) const KEY_LENGTH: usize = 26;
/// The length of a boxed key from before 2010, which these installers refuse
/// (a Blizzard account shows the 26-character key for it).
const OLD_KEY_LENGTH: usize = 16;

/// The field a key is typed into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeyField {
    BaseGame,
    Expansion,
}

impl KeyField {
    /// The product the installer's script requires in this field.
    const fn product(self) -> u32 {
        match self {
            Self::BaseGame => 24,
            Self::Expansion => 25,
        }
    }
}

/// Why a key cannot be installed. Each has its own stable code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeyProblem {
    /// Not 26 characters once dashes and spaces are removed.
    WrongLength,
    /// A 16-character key: the installers take only 26-character keys.
    OldKeyLength,
    /// A character that keys do not use.
    InvalidCharacter,
    /// A Lord of Destruction key in the Diablo II field.
    ExpansionKeyInBaseField,
    /// A Diablo II key in the Lord of Destruction field.
    BaseKeyInExpansionField,
    /// The key decodes to no product the field accepts.
    WrongProduct,
}

// The decode's two tables. Their values are libd2's `CHARTBL` (its 128
// entries, then 0xFF for every byte above 0x7F) and `SBOX` (MIT, the notice
// above), and they are the same bytes as the tables in the pinned
// Installer.exe of Diablo II (file offsets 0x1ee458 and 0x1ee278, VA 0x5ef458
// and 0x5ef278) and of Lord of Destruction (file offsets 0x1ee658 and
// 0x1ee478). SHA-256 of the 256 bytes of `DIGITS`:
// 67adc57cc18be9508c589762f962d82aaf8961e52996a8b7965b894c24b2b329; of the
// 480 bytes of `SBOX`:
// 538d10873a47425e36d6002996dad2e63eed77b1523b9cc6803d20eb700cc5c0.

/// Each byte's base-25 digit, for upper and lower case; 0xFF is not a key
/// character.
#[rustfmt::skip]
const DIGITS: [u8; 256] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0x00, 0xff, 0x01, 0xff, 0x02, 0x03, 0x04, 0x05, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0xff, 0x0d, 0x0e, 0xff, 0x0f, 0x10, 0xff,
    0x11, 0xff, 0x12, 0xff, 0x13, 0xff, 0x14, 0x15, 0x16, 0x17, 0x18, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0xff, 0x0d, 0x0e, 0xff, 0x0f, 0x10, 0xff,
    0x11, 0xff, 0x12, 0xff, 0x13, 0xff, 0x14, 0x15, 0x16, 0x17, 0x18, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
];

/// The substitution box: 30 rows of 16 nibbles.
#[rustfmt::skip]
const SBOX: [u8; 480] = [
    0x09, 0x04, 0x07, 0x0f, 0x0d, 0x0a, 0x03, 0x0b, 0x01, 0x02, 0x0c, 0x08, 0x06, 0x0e, 0x05, 0x00,
    0x09, 0x0b, 0x05, 0x04, 0x08, 0x0f, 0x01, 0x0e, 0x07, 0x00, 0x03, 0x02, 0x0a, 0x06, 0x0d, 0x0c,
    0x0c, 0x0e, 0x01, 0x04, 0x09, 0x0f, 0x0a, 0x0b, 0x0d, 0x06, 0x00, 0x08, 0x07, 0x02, 0x05, 0x03,
    0x0b, 0x02, 0x05, 0x0e, 0x0d, 0x03, 0x09, 0x00, 0x01, 0x0f, 0x07, 0x0c, 0x0a, 0x06, 0x04, 0x08,
    0x06, 0x02, 0x04, 0x05, 0x0b, 0x08, 0x0c, 0x0e, 0x0d, 0x0f, 0x07, 0x01, 0x0a, 0x00, 0x03, 0x09,
    0x05, 0x04, 0x0e, 0x0c, 0x07, 0x06, 0x0d, 0x0a, 0x0f, 0x02, 0x09, 0x01, 0x00, 0x0b, 0x08, 0x03,
    0x0c, 0x07, 0x08, 0x0f, 0x0b, 0x00, 0x05, 0x09, 0x0d, 0x0a, 0x06, 0x0e, 0x02, 0x04, 0x03, 0x01,
    0x03, 0x0a, 0x0e, 0x08, 0x01, 0x0b, 0x05, 0x04, 0x02, 0x0f, 0x0d, 0x0c, 0x06, 0x07, 0x09, 0x00,
    0x0c, 0x0d, 0x01, 0x0f, 0x08, 0x0e, 0x05, 0x0b, 0x03, 0x0a, 0x09, 0x00, 0x07, 0x02, 0x04, 0x06,
    0x0d, 0x0a, 0x07, 0x0e, 0x01, 0x06, 0x0b, 0x08, 0x0f, 0x0c, 0x05, 0x02, 0x03, 0x00, 0x04, 0x09,
    0x03, 0x0e, 0x07, 0x05, 0x0b, 0x0f, 0x08, 0x0c, 0x01, 0x0a, 0x04, 0x0d, 0x00, 0x06, 0x09, 0x02,
    0x0b, 0x06, 0x09, 0x04, 0x01, 0x08, 0x0a, 0x0d, 0x07, 0x0e, 0x00, 0x0c, 0x0f, 0x02, 0x03, 0x05,
    0x0c, 0x07, 0x08, 0x0d, 0x03, 0x0b, 0x00, 0x0e, 0x06, 0x0f, 0x09, 0x04, 0x0a, 0x01, 0x05, 0x02,
    0x0c, 0x06, 0x0d, 0x09, 0x0b, 0x00, 0x01, 0x02, 0x0f, 0x07, 0x03, 0x04, 0x0a, 0x0e, 0x08, 0x05,
    0x03, 0x06, 0x01, 0x05, 0x0b, 0x0c, 0x08, 0x00, 0x0f, 0x0e, 0x09, 0x04, 0x07, 0x0a, 0x0d, 0x02,
    0x0a, 0x07, 0x0b, 0x0f, 0x02, 0x08, 0x00, 0x0d, 0x0e, 0x0c, 0x01, 0x06, 0x09, 0x03, 0x05, 0x04,
    0x0a, 0x0b, 0x0d, 0x04, 0x03, 0x08, 0x05, 0x09, 0x01, 0x00, 0x0f, 0x0c, 0x07, 0x0e, 0x02, 0x06,
    0x0b, 0x04, 0x0d, 0x0f, 0x01, 0x06, 0x03, 0x0e, 0x07, 0x0a, 0x0c, 0x08, 0x09, 0x02, 0x05, 0x00,
    0x09, 0x06, 0x07, 0x00, 0x01, 0x0a, 0x0d, 0x02, 0x03, 0x0e, 0x0f, 0x0c, 0x05, 0x0b, 0x04, 0x08,
    0x0d, 0x0e, 0x05, 0x06, 0x01, 0x09, 0x08, 0x0c, 0x02, 0x0f, 0x03, 0x07, 0x0b, 0x04, 0x00, 0x0a,
    0x09, 0x0f, 0x04, 0x00, 0x01, 0x06, 0x0a, 0x0e, 0x02, 0x03, 0x07, 0x0d, 0x05, 0x0b, 0x08, 0x0c,
    0x03, 0x0e, 0x01, 0x0a, 0x02, 0x0c, 0x08, 0x04, 0x0b, 0x07, 0x0d, 0x00, 0x0f, 0x06, 0x09, 0x05,
    0x07, 0x02, 0x0c, 0x06, 0x0a, 0x08, 0x0b, 0x00, 0x0f, 0x04, 0x03, 0x0e, 0x09, 0x01, 0x0d, 0x05,
    0x0c, 0x04, 0x05, 0x09, 0x0a, 0x02, 0x08, 0x0d, 0x03, 0x0f, 0x01, 0x0e, 0x06, 0x07, 0x0b, 0x00,
    0x0a, 0x08, 0x0e, 0x0d, 0x09, 0x0f, 0x03, 0x00, 0x04, 0x06, 0x01, 0x0c, 0x07, 0x0b, 0x02, 0x05,
    0x03, 0x0c, 0x04, 0x0a, 0x02, 0x0f, 0x0d, 0x0e, 0x07, 0x00, 0x05, 0x08, 0x01, 0x06, 0x0b, 0x09,
    0x0a, 0x0c, 0x01, 0x00, 0x09, 0x0e, 0x0d, 0x0b, 0x03, 0x07, 0x0f, 0x08, 0x05, 0x02, 0x04, 0x06,
    0x0e, 0x0a, 0x01, 0x08, 0x07, 0x06, 0x05, 0x0c, 0x02, 0x0f, 0x00, 0x0d, 0x03, 0x0b, 0x04, 0x09,
    0x03, 0x08, 0x0e, 0x00, 0x07, 0x09, 0x0f, 0x0c, 0x01, 0x06, 0x0d, 0x02, 0x05, 0x0a, 0x0b, 0x04,
    0x03, 0x0a, 0x0c, 0x04, 0x0d, 0x0b, 0x09, 0x0e, 0x0f, 0x06, 0x01, 0x07, 0x02, 0x00, 0x05, 0x08,
];

/// A key as the installers get it: dashes and spaces removed, upper case.
/// It is wiped when dropped.
pub(crate) struct NormalizedKey(Vec<u8>);

impl NormalizedKey {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for NormalizedKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for NormalizedKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NormalizedKey([REDACTED])")
    }
}

/// Remove dashes and spaces and make upper case.
pub(crate) fn normalize(raw: &[u8]) -> NormalizedKey {
    // Room for all of it at once: no growth leaves key bytes in freed memory.
    let mut key = Vec::with_capacity(raw.len());
    key.extend(
        raw.iter()
            .filter(|byte| !matches!(byte, b'-' | b' '))
            .map(u8::to_ascii_uppercase),
    );
    NormalizedKey(key)
}

/// Check a key for `field` as the installer does: its form (length and
/// characters), then its decoded product.
pub(crate) fn check(field: KeyField, raw: &[u8]) -> Result<NormalizedKey, KeyProblem> {
    let key = check_form(raw)?;
    #[cfg(test)]
    if key.0 == test_key(field).as_bytes() {
        return Ok(key);
    }
    check_product(field, key)
}

/// Test builds only: one clearly fake string per field passes the product
/// step, so that tests of what comes after the check can run. The real
/// decode refuses both (`the_test_keys_fail_the_real_check`). A release build
/// has neither.
#[cfg(test)]
pub(crate) const TEST_BASE_KEY: &str = "BCDFGHJKMNPRTVWXYZ24678922";
#[cfg(test)]
pub(crate) const TEST_EXPANSION_KEY: &str = "CDFGHJKMNPRTVWXYZ246789BCD";

#[cfg(test)]
fn test_key(field: KeyField) -> &'static str {
    match field {
        KeyField::BaseGame => TEST_BASE_KEY,
        KeyField::Expansion => TEST_EXPANSION_KEY,
    }
}

/// The check without the test builds' fake keys.
#[cfg(test)]
pub(crate) fn check_without_test_keys(
    field: KeyField,
    raw: &[u8],
) -> Result<NormalizedKey, KeyProblem> {
    check_product(field, check_form(raw)?)
}

fn check_form(raw: &[u8]) -> Result<NormalizedKey, KeyProblem> {
    let key = normalize(raw);
    match key.0.len() {
        KEY_LENGTH => {}
        OLD_KEY_LENGTH => return Err(KeyProblem::OldKeyLength),
        _ => return Err(KeyProblem::WrongLength),
    }
    if !key.0.iter().all(|byte| ALPHABET.contains(byte)) {
        return Err(KeyProblem::InvalidCharacter);
    }
    Ok(key)
}

fn check_product(field: KeyField, key: NormalizedKey) -> Result<NormalizedKey, KeyProblem> {
    let mut digits = [0_u8; KEY_LENGTH];
    for (digit, byte) in digits.iter_mut().zip(&key.0) {
        *digit = DIGITS[usize::from(*byte)];
        if *digit > 24 {
            digits.zeroize();
            return Err(KeyProblem::InvalidCharacter);
        }
    }
    let product = decode(&digits).product;
    digits.zeroize();
    match problem(field, product) {
        None => Ok(key),
        Some(problem) => Err(problem),
    }
}

/// What is wrong with a key of `product` in `field`, if anything.
fn problem(field: KeyField, product: u32) -> Option<KeyProblem> {
    match (field, product) {
        (_, product) if product == field.product() => None,
        (KeyField::BaseGame, 25) => Some(KeyProblem::ExpansionKeyInBaseField),
        (KeyField::Expansion, 24) => Some(KeyProblem::BaseKeyInExpansionField),
        _ => Some(KeyProblem::WrongProduct),
    }
}

/// The characters keys use. The installer's map also takes lower case,
/// which [`normalize`] makes upper case first.
const ALPHABET: &[u8; 25] = b"246789BCDEFGHJKMNPRTVWXYZ";

/// What a 26-character key decodes to.
#[derive(Debug)]
struct Decoded {
    product: u32,
    #[cfg(test)]
    public: u32,
    #[cfg(test)]
    value: [u8; 10],
}

/// Decode the digits of a 26-character key: as base-5 pairs at a stride,
/// into a 120-bit number, through the substitution box, then a bit
/// permutation.
fn decode(digits: &[u8; KEY_LENGTH]) -> Decoded {
    let mut base5 = [0_u8; 52];
    let mut second = 0x21_usize;
    for digit in digits {
        let first = (second + 0x7b5) % 52;
        second = (first + 0x7b5) % 52;
        base5[first] = digit / 5;
        base5[second] = digit % 5;
    }
    // token[0] is the most significant word.
    let mut token = [0_u32; 4];
    for digit in base5.iter().rev() {
        let mut carry = u64::from(*digit);
        for word in token.iter_mut().rev() {
            let product = u64::from(*word) * 5 + carry;
            *word = product as u32;
            carry = product >> 32;
        }
    }
    for nibble in (0..30).rev() {
        let row = &SBOX[nibble * 16..nibble * 16 + 16];
        let mut acc = get_nibble(&token, nibble);
        for other in (nibble + 1..30).rev().chain((0..nibble).rev()) {
            acc = row[usize::from(get_nibble(&token, other) ^ row[usize::from(acc)])];
        }
        set_nibble(&mut token, nibble, row[usize::from(acc)]);
    }
    let mut source = token;
    let mut from = 0;
    for bit in 0..120 {
        set_bit(&mut token, bit, get_bit(&source, from));
        from = (from + 11) % 120;
    }
    source.zeroize();
    base5.zeroize();
    let decoded = Decoded {
        product: token[0] >> 10,
        #[cfg(test)]
        public: (token[0] & 0x3ff) << 16 | token[1] >> 16,
        #[cfg(test)]
        value: {
            let mut value = [0_u8; 10];
            value[..2].copy_from_slice(&(token[1] as u16).to_le_bytes());
            value[2..6].copy_from_slice(&token[2].to_le_bytes());
            value[6..].copy_from_slice(&token[3].to_le_bytes());
            value
        },
    };
    token.zeroize();
    decoded
}

fn get_nibble(token: &[u32; 4], nibble: usize) -> u8 {
    ((token[3 - (nibble >> 3)] >> ((nibble & 7) * 4)) & 0xf) as u8
}

fn set_nibble(token: &mut [u32; 4], nibble: usize, value: u8) {
    let shift = (nibble & 7) * 4;
    let word = &mut token[3 - (nibble >> 3)];
    *word = (*word & !(0xf << shift)) | (u32::from(value & 0xf) << shift);
}

fn get_bit(token: &[u32; 4], bit: usize) -> u32 {
    (token[3 - (bit >> 5)] >> (bit & 31)) & 1
}

fn set_bit(token: &mut [u32; 4], bit: usize, value: u32) {
    let shift = bit & 31;
    let word = &mut token[3 - (bit >> 5)];
    *word = (*word & !(1 << shift)) | ((value & 1) << shift);
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    const DIGITS_SHA256: &str = "67adc57cc18be9508c589762f962d82aaf8961e52996a8b7965b894c24b2b329";
    const SBOX_SHA256: &str = "538d10873a47425e36d6002996dad2e63eed77b1523b9cc6803d20eb700cc5c0";

    fn sha256(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Fake strings and what the decode code of both pinned Installer.exe
    /// files gives for them: product, public value, the rest.
    const DECODE_VECTORS: [(&[u8; KEY_LENGTH], u32, u32, &str); 3] = [
        (
            b"22222222222222222222222222",
            3293,
            0x00c5_8620,
            "969c435fa6c175b8adfb",
        ),
        (
            b"ZZZZZZZZZZZZZZZZZZZZZZZZZZ",
            21039,
            0x01f7_6232,
            "c3634a7f034504c14d40",
        ),
        (
            b"246789BCDEFGHJKMNPRTVWXYZ2",
            5775,
            0x0011_46db,
            "5ab9c310857095acf08d",
        ),
    ];

    #[test]
    fn the_tables_are_the_installers_tables() {
        assert_eq!(sha256(&DIGITS), DIGITS_SHA256);
        assert_eq!(sha256(&SBOX), SBOX_SHA256);
        // The map takes the alphabet in either case, in order, and nothing
        // else.
        for (digit, byte) in ALPHABET.iter().enumerate() {
            assert_eq!(usize::from(DIGITS[usize::from(*byte)]), digit);
            assert_eq!(
                usize::from(DIGITS[usize::from(byte.to_ascii_lowercase())]),
                digit
            );
        }
        let characters = DIGITS.iter().filter(|digit| **digit != 0xff).count();
        assert_eq!(characters, 25 + 19);
    }

    #[test]
    fn dashes_spaces_and_case_are_normalized() {
        let key = normalize(b"2222-2222 2222-2222-2222-22bc");
        assert_eq!(key.as_bytes(), b"2222222222222222222222BC");
        assert_eq!(format!("{key:?}"), "NormalizedKey([REDACTED])");
    }

    #[test]
    fn the_form_is_checked_first() {
        let field = KeyField::BaseGame;
        assert_eq!(check(field, b"").unwrap_err(), KeyProblem::WrongLength);
        assert_eq!(
            check(field, b"2222222222222222222222222").unwrap_err(),
            KeyProblem::WrongLength
        );
        assert_eq!(
            check(field, b"2222-2222-2222-2222").unwrap_err(),
            KeyProblem::OldKeyLength
        );
        // A, I, L, O, Q, S, U, 0, 1, 3 and 5 are not key characters.
        for bad in *b"AIO015S#" {
            let mut key = [b'2'; KEY_LENGTH];
            key[7] = bad;
            assert_eq!(
                check(field, &key).unwrap_err(),
                KeyProblem::InvalidCharacter,
                "{}",
                bad as char
            );
        }
    }

    #[test]
    fn the_decode_gives_the_installers_results() {
        for (fake, product, public, value) in DECODE_VECTORS {
            let digits: [u8; KEY_LENGTH] =
                std::array::from_fn(|index| DIGITS[usize::from(fake[index])]);
            let decoded = decode(&digits);
            assert_eq!(decoded.product, product);
            assert_eq!(decoded.public, public);
            let shown: String = decoded.value.iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(shown, value);
        }
    }

    #[test]
    fn a_key_whose_product_is_not_the_fields_fails() {
        // Fake strings: whatever they decode to, it is checked against the
        // field, never searched for.
        for (fake, ..) in DECODE_VECTORS {
            for field in [KeyField::BaseGame, KeyField::Expansion] {
                assert_eq!(check(field, fake).unwrap_err(), KeyProblem::WrongProduct);
            }
        }
        // Lower case and dashes are the same key.
        assert_eq!(
            check(KeyField::BaseGame, b"zzzz-zzzz-zzzz-zzzz-zzzz-zzzz-zz").unwrap_err(),
            KeyProblem::WrongProduct
        );
        // The product of the other field has its own reason.
        assert_eq!(problem(KeyField::BaseGame, 24), None);
        assert_eq!(problem(KeyField::Expansion, 25), None);
        assert_eq!(
            problem(KeyField::BaseGame, 25),
            Some(KeyProblem::ExpansionKeyInBaseField)
        );
        assert_eq!(
            problem(KeyField::Expansion, 24),
            Some(KeyProblem::BaseKeyInExpansionField)
        );
        assert_eq!(
            problem(KeyField::BaseGame, 3293),
            Some(KeyProblem::WrongProduct)
        );
    }

    #[test]
    fn the_test_keys_fail_the_real_check() {
        for field in [KeyField::BaseGame, KeyField::Expansion] {
            for key in [TEST_BASE_KEY, TEST_EXPANSION_KEY] {
                assert!(check_without_test_keys(field, key.as_bytes()).is_err());
            }
        }
        // Each passes only in its own field, and only in test builds.
        assert!(check(KeyField::BaseGame, TEST_BASE_KEY.as_bytes()).is_ok());
        assert!(check(KeyField::Expansion, TEST_EXPANSION_KEY.as_bytes()).is_ok());
        assert!(check(KeyField::Expansion, TEST_BASE_KEY.as_bytes()).is_err());
        assert!(check(KeyField::BaseGame, TEST_EXPANSION_KEY.as_bytes()).is_err());
    }

    #[test]
    fn nibbles_and_bits_address_the_words_from_the_least_significant() {
        let mut token = [0_u32; 4];
        set_nibble(&mut token, 0, 0xa);
        set_nibble(&mut token, 9, 0x5);
        assert_eq!(token, [0, 0, 0x50, 0xa]);
        assert_eq!(get_nibble(&token, 9), 5);
        set_bit(&mut token, 33, 1);
        assert_eq!(token[2], 0x52);
        assert_eq!(get_bit(&token, 33), 1);
        set_bit(&mut token, 33, 0);
        assert_eq!(token[2], 0x50);
    }

    /// The tables against the bytes at the pinned offsets of a pinned
    /// Installer.exe (Diablo II's or Lord of Destruction's) from a kept
    /// payload:
    /// `PD2_DIRECT_DOWNLOAD_INSTALLER=<Installer.exe> cargo test --lib keys -- --ignored`.
    #[test]
    #[ignore = "needs a pinned Installer.exe"]
    fn the_decode_matches_the_installer() {
        let path = std::env::var_os("PD2_DIRECT_DOWNLOAD_INSTALLER").expect("an installer");
        let installer = std::fs::read(path).unwrap();
        // Diablo II's offsets, then Lord of Destruction's.
        let found = [(0x1e_e458, 0x1e_e278), (0x1e_e658, 0x1e_e478)]
            .iter()
            .filter(|(digits, sbox)| {
                installer.get(*digits..digits + 256) == Some(&DIGITS[..])
                    && installer.get(*sbox..sbox + 480) == Some(&SBOX[..])
            })
            .count();
        assert_eq!(found, 1, "the tables are at the installer's pinned offsets");
    }
}
