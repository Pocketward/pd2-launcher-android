//! Redaction for text the Launcher keeps for, or hands to, maintainers.
//!
//! Every text that enters the failure activity log or a saved report passes
//! through [`Redactor::text`]:
//!
//! - CD keys: the in-memory keys (when the process holds them, for example
//!   `D2_KEY`/`D2_LOD_KEY` of an unattended run) are replaced wherever they
//!   appear, in any case and grouping. Any key-shaped run — 16 or 26 of the
//!   characters CD keys use, in any case, as one word or in groups that
//!   group separators part (a dash, a space, a tab, U+00A0, U+202F, U+2010,
//!   U+2011, U+2012, U+2013, U+2014 or U+2212) — is replaced too, as is the
//!   value after a key name (`D2_KEY=…`, `CD key: …`; its first word, and
//!   the groups after it when they make a key's 16 or 26 characters). What
//!   stays: a word with another letter or digit (a 0, 1, 3, 5, A, I, L, O, Q,
//!   S or U, as in most Wine addresses and hashes and in names such as
//!   `WINEDLLOVERRIDES`), a whole UUID, and in the game's own logs a key
//!   shape wholly inside the address and hex words of a crash report's
//!   memory dump row ([`Redactor::game_log_text`]). Other words of key
//!   characters only that add up to 16 or 26 (a 16-digit hash such as
//!   `fdfb2b868b66f942`, the words `Extended renderer`) are hidden.
//! - `$HOME` becomes `~` (also in its Wine `Z:\…` form) and the user name
//!   becomes `<user>`.
//! - URLs lose their credentials, query string and fragment.
//!
//! [`mask_game_identities`] additionally hides account, character and game
//! names in Battle.net and game logs (best effort, see docs/DIAGNOSTICS.md).

use std::{env, path::Path};

pub(crate) const KEY_MASK: &str = "<cd-key>";
const USER_MASK: &str = "<user>";
const NAME_MASK: &str = "<name>";

/// Key names whose value is always a CD key.
const KEY_CONTEXTS: [&str; 10] = [
    "key1",
    "key2",
    "d2_lod_key",
    "d2_key",
    "lod_key",
    "cdkey",
    "cd-key",
    "cd key",
    "product key",
    "product_key",
];

#[derive(Clone, Debug, Default)]
pub(crate) struct Redactor {
    home: Option<String>,
    user: Option<String>,
    /// Upper-case CD keys known to this process, never logged.
    secrets: Vec<String>,
}

impl Redactor {
    pub(crate) fn new(
        home: Option<&Path>,
        user: Option<&str>,
        secrets: impl IntoIterator<Item = String>,
    ) -> Self {
        let home = home
            .and_then(Path::to_str)
            .map(|home| home.trim_end_matches('/').to_owned())
            .filter(|home| home.len() > 1);
        let user = user
            .map(str::to_owned)
            .or_else(|| {
                home.as_deref()
                    .and_then(|home| home.rsplit('/').next())
                    .map(str::to_owned)
            })
            .filter(|user| user.len() >= 2);
        let secrets = secrets
            .into_iter()
            .map(|secret| {
                secret
                    .chars()
                    .filter(char::is_ascii_alphanumeric)
                    .collect::<String>()
                    .to_ascii_uppercase()
            })
            .filter(|secret| secret.len() >= 6)
            .collect();
        Self {
            home,
            user,
            secrets,
        }
    }

    /// This process's home, user name, and any CD keys in its environment.
    /// `.env.local` is never read.
    pub(crate) fn from_environment() -> Self {
        let home = env::var_os("HOME").map(std::path::PathBuf::from);
        let user = env::var("USER")
            .ok()
            .or_else(|| env::var("LOGNAME").ok())
            .filter(|user| !user.is_empty());
        let secrets = ["D2_KEY", "D2_LOD_KEY"]
            .into_iter()
            .filter_map(|name| env::var(name).ok())
            // A dev build's compiled-in keys.
            .chain(
                crate::dev_build::product_keys()
                    .into_iter()
                    .flat_map(|(base, expansion)| [base, expansion])
                    .map(str::to_owned),
            );
        Self::new(home.as_deref(), user.as_deref(), secrets)
    }

    pub(crate) fn text(&self, input: &str) -> String {
        self.text_with(input, false)
    }

    /// [`text`](Self::text) for the game's own logs: in a row of a memory
    /// dump in a crash report (`0201FF18: 54F37800 FC052502 …  TM.x…`), a
    /// key shape wholly inside its address and hex words stays (those words
    /// are no key); every other key shape of the row (one in its ASCII
    /// column, or one across the two), the keys this process holds, and
    /// every other line are masked as [`text`](Self::text) masks them.
    pub(crate) fn game_log_text(&self, input: &str) -> String {
        input
            .split_inclusive('\n')
            .map(|line| self.text_with(line, true))
            .collect()
    }

    fn text_with(&self, input: &str, dump_rows: bool) -> String {
        let text = strip_url_secrets(input);
        let text = self.mask_secrets(&text);
        // A dump row as it is after the held keys: a key there can make it
        // none, and then every key shape of it goes.
        let exempt = if dump_rows {
            dump_row_ascii(&text).unwrap_or(0)
        } else {
            0
        };
        // Shapes first: a key name before a key in groups (`CD key: 2468
        // BCDE …`) would else hide only the first group.
        let text = mask_key_shaped(&text, exempt);
        let text = mask_key_contexts(&text);
        let text = self.mask_home(&text);
        let text = self.mask_user(&text);
        mask_windows_users(&text)
    }

    /// A path for a report: under the home it starts with `~`.
    pub(crate) fn path(&self, path: &Path) -> String {
        self.text(&path.to_string_lossy())
    }

    /// The keys this process holds, in any case and in any grouping: group
    /// separators ([`group_separator`]) between any of its characters.
    fn mask_secrets(&self, text: &str) -> String {
        let bytes = text.as_bytes();
        let mut hidden = Vec::new();
        for secret in &self.secrets {
            let secret = secret.as_bytes();
            for start in 0..bytes.len() {
                if !bytes[start].eq_ignore_ascii_case(&secret[0]) {
                    continue;
                }
                let mut index = start + 1;
                let mut matched = 1;
                while matched < secret.len() {
                    while let Some(length) = group_separator(text, index) {
                        index += length;
                    }
                    if index == bytes.len() || !bytes[index].eq_ignore_ascii_case(&secret[matched])
                    {
                        break;
                    }
                    index += 1;
                    matched += 1;
                }
                if matched == secret.len() {
                    hidden.push((start, index));
                }
            }
        }
        replace_ranges(text, hidden, KEY_MASK)
    }

    fn mask_home(&self, text: &str) -> String {
        let Some(home) = &self.home else {
            return text.to_owned();
        };
        let text = text.replace(home.as_str(), "~");
        // Wine names the same folder `Z:\home\user`.
        let windows = format!("Z:{}", home.replace('/', "\\"));
        let text = replace_ignore_ascii_case(&text, &windows, "~");
        replace_ignore_ascii_case(&text, &home.replace('/', "\\"), "~")
    }

    fn mask_user(&self, text: &str) -> String {
        match &self.user {
            Some(user) => replace_ignore_ascii_case(text, user, USER_MASK),
            None => text.to_owned(),
        }
    }
}

fn replace_ignore_ascii_case(text: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() || needle.len() > text.len() {
        return text.to_owned();
    }
    let lower = text.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    let mut output = String::with_capacity(text.len());
    let mut position = 0;
    while let Some(found) = lower[position..].find(&needle) {
        let start = position + found;
        output.push_str(&text[position..start]);
        output.push_str(replacement);
        position = start + needle.len();
    }
    output.push_str(&text[position..]);
    output
}

/// Windows profile folders that name no one.
const SHARED_WINDOWS_USERS: [&str; 4] = ["public", "default", "default user", "all users"];

/// Hide the user name of a Windows profile folder (`C:\Users\<name>`, also
/// with `/` or JSON's `\\`): the official launcher's logs from a folder
/// copied from a PC name the PC's user.
fn mask_windows_users(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let separator = |byte: u8| byte == b'\\' || byte == b'/';
    let mut hidden = Vec::new();
    let mut position = 0;
    while let Some(found) = lower[position..].find("users") {
        let at = position + found;
        position = at + "users".len();
        // A drive before it: `C:\`, `c:/`, `C:\\`.
        let mut before = at;
        while before > 0 && separator(bytes[before - 1]) {
            before -= 1;
        }
        if before == at
            || before < 2
            || bytes[before - 1] != b':'
            || !bytes[before - 2].is_ascii_alphabetic()
        {
            continue;
        }
        let mut start = position;
        while start < bytes.len() && separator(bytes[start]) {
            start += 1;
        }
        if start == position {
            continue;
        }
        let end = windows_user_end(bytes, start);
        let name = &lower[start..end];
        if !name.is_empty() && !SHARED_WINDOWS_USERS.contains(&name) {
            hidden.push((start, end));
        }
    }
    replace_ranges(text, hidden, USER_MASK)
}

/// Where a Windows user name that starts at `start` of `bytes` ends: at a
/// character no Windows user name holds (`" / \ [ ] : ; | = , + * ? < >`), a
/// line's end, or a quote (an apostrophe with a letter on each side, as in
/// `O'Brien`, is part of the name), and after at most
/// [`WINDOWS_USER_LIMIT`] characters. A space does not end it: a Windows user
/// name can hold spaces.
fn windows_user_end(bytes: &[u8], start: usize) -> usize {
    let letter = |byte: u8| byte.is_ascii_alphabetic() || byte >= 0x80;
    let mut end = start;
    let mut characters = 0;
    while end < bytes.len() && characters < WINDOWS_USER_LIMIT {
        let byte = bytes[end];
        if matches!(
            byte,
            b'"' | b'/'
                | b'\\'
                | b'['
                | b']'
                | b':'
                | b';'
                | b'|'
                | b'='
                | b','
                | b'+'
                | b'*'
                | b'?'
                | b'<'
                | b'>'
                | b'\n'
                | b'\r'
        ) || (byte == b'\''
            && !(end > start
                && letter(bytes[end - 1])
                && bytes.get(end + 1).is_some_and(|next| letter(*next))))
        {
            break;
        }
        end += 1;
        // A UTF-8 character counts once, at its first byte.
        if byte & 0xC0 != 0x80 {
            characters += 1;
        }
    }
    while end < bytes.len() && bytes[end] & 0xC0 == 0x80 {
        end += 1;
    }
    end
}

/// The longest Windows user name hidden as one.
const WINDOWS_USER_LIMIT: usize = 64;

/// Remove `user:password@`, `?query` and `#fragment` from every URL.
fn strip_url_secrets(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(found) = rest.find("://") {
        let scheme_start = rest[..found]
            .rfind(|character: char| {
                !(character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.'))
            })
            .map_or(0, |index| index + 1);
        if scheme_start == found {
            output.push_str(&rest[..found + 3]);
            rest = &rest[found + 3..];
            continue;
        }
        output.push_str(&rest[..found + 3]);
        let after = &rest[found + 3..];
        let end = after
            .find(|character: char| {
                character.is_whitespace()
                    || matches!(character, '"' | '\'' | '<' | '>' | '`' | '\\')
            })
            .unwrap_or(after.len());
        let url = &after[..end];
        let authority_end = url.find(['/', '?', '#']).unwrap_or(url.len());
        let (authority, path) = url.split_at(authority_end);
        let authority = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let path = path.split(['?', '#']).next().unwrap_or("");
        output.push_str(authority);
        output.push_str(path);
        rest = &after[end..];
    }
    output.push_str(rest);
    output
}

/// Hide the value after a CD-key name (`D2_KEY=…`, `CD key: …`).
fn mask_key_contexts(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut hidden: Vec<(usize, usize)> = Vec::new();
    for context in KEY_CONTEXTS {
        let mut position = 0;
        while let Some(found) = lower[position..].find(context) {
            let start = position + found;
            let mut index = start + context.len();
            position = index;
            // `key1` and `key2` at a word's start: `Hotkey2 = F4` names no
            // key.
            if context.starts_with("key") && start > 0 && bytes[start - 1].is_ascii_alphanumeric() {
                continue;
            }
            // Separator: `=`, `:`, quotes, spaces, tabs, a no-break space, an
            // en dash, or a dash between spaces (` - `), at least one of them.
            let separator_start = index;
            while let Some(character) = text[index..].chars().next() {
                let between_spaces = character == '-'
                    && index > separator_start
                    && bytes[index - 1] == b' '
                    && bytes.get(index + 1) == Some(&b' ');
                if !(matches!(
                    character,
                    '=' | ':' | ' ' | '\t' | '"' | '\'' | '\u{00A0}' | '\u{2013}'
                ) || between_spaces)
                {
                    break;
                }
                index += character.len_utf8();
            }
            if index == separator_start {
                continue;
            }
            // The value: its first word, and the groups after it (group
            // separators between) only when with them it has exactly 16 or
            // 26 letters and digits, a key's length.
            let value_start = index;
            let first_end = word_end(bytes, value_start);
            if first_end == value_start {
                continue;
            }
            let mut value_end = first_end;
            let mut length = first_end - value_start;
            let mut cursor = first_end;
            loop {
                let gap = cursor;
                while let Some(separator) = group_separator(text, cursor) {
                    cursor += separator;
                }
                let next_end = word_end(bytes, cursor);
                if cursor == gap || next_end == cursor {
                    break;
                }
                length += next_end - cursor;
                if length > 26 {
                    break;
                }
                if matches!(length, 16 | 26) {
                    value_end = next_end;
                }
                cursor = next_end;
            }
            hidden.push((value_start, value_end));
        }
    }
    replace_ranges(text, hidden, KEY_MASK)
}

/// Where the run of ASCII letters and digits that starts at `index` ends.
fn word_end(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
        index += 1;
    }
    index
}

/// Hide every run shaped like a Diablo II (16) or Lord of Destruction (26) CD
/// key: words only of the characters CD keys use ([`key_character`]), in any
/// case, with 16 or 26 of them in all, as one word or as groups that group
/// separators part ([`group_separator`], in any mix: `2468-BCDE 9F7E-6D4B`,
/// `2468 - bcde`). A word with another letter or digit (a 0, 1, 3, 5, A, I,
/// L, O, Q, S or U) is never part of one, nor is a whole UUID
/// (`8-4-4-4-12` hex digits, with or without braces): a key is never one.
/// Other hex data whose words have only key characters (an address, a hash,
/// a version) can be hidden.
fn mask_key_shaped(text: &str, exempt: usize) -> String {
    let bytes = text.as_bytes();
    // Each word (a run of ASCII letters and digits) and whether it is only of
    // key characters.
    let mut words = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_alphanumeric() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
            index += 1;
        }
        let keyed = bytes[start..index].iter().all(|byte| key_character(*byte));
        words.push((start, index, keyed));
    }
    // A whole UUID's five words: no key.
    for first in 0..words.len().saturating_sub(4) {
        let uuid = words[first..first + 5].iter().zip([8, 4, 4, 4, 12]).all(
            |(&(start, end, _), length)| {
                end - start == length && bytes[start..end].iter().all(u8::is_ascii_hexdigit)
            },
        ) && words[first..first + 5]
            .windows(2)
            .all(|pair| &text[pair[0].1..pair[1].0] == "-");
        if uuid {
            for word in &mut words[first..first + 5] {
                word.2 = false;
            }
        }
    }
    let parted = |from: usize, to: usize| {
        let mut index = from;
        while index < to {
            match group_separator(text, index) {
                Some(length) => index += length,
                None => return false,
            }
        }
        to > from
    };
    let mut hidden = Vec::new();
    for first in 0..words.len() {
        let mut length = 0;
        for next in first..words.len() {
            let (start, end, keyed) = words[next];
            if !keyed || (next > first && !parted(words[next - 1].1, start)) {
                break;
            }
            length += end - start;
            if length > 26 {
                break;
            }
            // A shape wholly inside the first `exempt` bytes (a dump row's
            // address and hex words) stays.
            if matches!(length, 16 | 26) && end > exempt {
                hidden.push((words[first].0, end));
            }
        }
    }
    replace_ranges(text, hidden, KEY_MASK)
}

/// What parts a key's groups when someone types or pastes it: a dash, a
/// space, a tab, a no-break space (U+00A0), a narrow no-break space
/// (U+202F), a hyphen (U+2010), a no-break hyphen (U+2011), a figure dash
/// (U+2012), an en dash (U+2013), an em dash (U+2014) or a minus sign
/// (U+2212).
const GROUP_SEPARATORS: [char; 11] = [
    '-', ' ', '\t', '\u{00A0}', '\u{202F}', '\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}',
    '\u{2014}', '\u{2212}',
];

/// The byte length of the group separator ([`GROUP_SEPARATORS`]) at `index`
/// of `text`, if one is there.
fn group_separator(text: &str, index: usize) -> Option<usize> {
    let character = text.get(index..)?.chars().next()?;
    GROUP_SEPARATORS
        .contains(&character)
        .then(|| character.len_utf8())
}

/// Where the ASCII column of `line` starts, when `line` is a row of a memory
/// dump in a crash report of the game: an optional time, an address and a
/// colon, one to four 32-bit words in hex, and the ASCII column after two
/// spaces (or nothing). None for any other line.
fn dump_row_ascii(line: &str) -> Option<usize> {
    let hex8 = |text: &str| text.len() == 8 && text.bytes().all(|byte| byte.is_ascii_hexdigit());
    let body = line.trim_start();
    let mut position = line.len() - body.len();
    // `21:13:04.951  ` before the first row.
    let mut rest = body;
    if let Some((time, after)) = rest.split_once(' ')
        && time.len() == 12
        && time.bytes().enumerate().all(|(index, byte)| match index {
            2 | 5 => byte == b':',
            8 => byte == b'.',
            _ => byte.is_ascii_digit(),
        })
    {
        let after_time = after.trim_start();
        position += time.len() + (after.len() - after_time.len()) + 1;
        rest = after_time;
    }
    let (address, after) = rest.split_once(": ")?;
    if !hex8(address) {
        return None;
    }
    position += address.len() + 2;
    let mut rest = after;
    let mut words = 0;
    // `get`: the text after a word can be any UTF-8.
    while words < 4 && rest.get(..8).is_some_and(hex8) {
        words += 1;
        position += 8;
        rest = &rest[8..];
        if let Some(next) = rest.strip_prefix(' ')
            && next.get(..8).is_some_and(hex8)
        {
            position += 1;
            rest = next;
        } else {
            break;
        }
    }
    if words == 0 {
        return None;
    }
    let rest = rest.trim_end_matches(['\r', '\n']);
    if rest.is_empty() || rest.starts_with("  ") {
        Some(position)
    } else {
        None
    }
}

/// The characters of Diablo II's CD keys (2 4 6 7 8 9 B C D E F G H J K M N
/// P R T V W X Y Z), in either case.
fn key_character(byte: u8) -> bool {
    matches!(
        byte.to_ascii_uppercase(),
        b'2' | b'4'
            | b'6'..=b'9'
            | b'B'..=b'H'
            | b'J'
            | b'K'
            | b'M'
            | b'N'
            | b'P'
            | b'R'
            | b'T'
            | b'V'..=b'Z'
    )
}

fn replace_ranges(text: &str, mut ranges: Vec<(usize, usize)>, replacement: &str) -> String {
    if ranges.is_empty() {
        return text.to_owned();
    }
    ranges.sort_unstable();
    let mut output = String::with_capacity(text.len());
    let mut position = 0;
    for (start, end) in ranges {
        if start < position {
            position = position.max(end);
            continue;
        }
        output.push_str(&text[position..start]);
        output.push_str(replacement);
        position = end;
    }
    output.push_str(&text[position..]);
    output
}

/// Words after which Battle.net and game logs name an account, a character,
/// or a game.
const IDENTITY_CONTEXTS: [&str; 12] = [
    "account",
    "acct",
    "character",
    "charname",
    "char",
    "username",
    "user",
    "player",
    "realm",
    "name",
    "game",
    "password",
];

/// Best effort: hide quoted strings and the word after an identity context
/// (`Account: Foo`, `char=Bar`, `Joined game Baz`) in Battle.net and game
/// logs.
pub(crate) fn mask_game_identities(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        output.push_str(&mask_line_identities(line));
    }
    output
}

fn mask_line_identities(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut hidden = Vec::new();
    // Quoted strings.
    let mut index = 0;
    while index < bytes.len() {
        if matches!(bytes[index], b'"' | b'\'')
            && let Some(close) = line[index + 1..].find(bytes[index] as char)
        {
            let end = index + 1 + close;
            if end > index + 1 {
                hidden.push((index + 1, end));
            }
            index = end + 1;
            continue;
        }
        index += 1;
    }
    let lower = line.to_ascii_lowercase();
    for context in IDENTITY_CONTEXTS {
        let mut position = 0;
        while let Some(found) = lower[position..].find(context) {
            let start = position + found;
            let mut index = start + context.len();
            position = index;
            // A whole word only.
            if start > 0 && bytes[start - 1].is_ascii_alphabetic() {
                continue;
            }
            while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
                index += 1;
            }
            let separator_start = index;
            while index < bytes.len() && matches!(bytes[index], b'=' | b':' | b' ' | b'\t') {
                index += 1;
            }
            if index == separator_start {
                continue;
            }
            let value_start = index;
            while index < bytes.len()
                && !bytes[index].is_ascii_whitespace()
                && !matches!(bytes[index], b',' | b';' | b')' | b']' | b'"' | b'\'')
            {
                index += 1;
            }
            if index > value_start {
                hidden.push((value_start, index));
            }
        }
    }
    replace_ranges(line, hidden, NAME_MASK)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor() -> Redactor {
        Redactor::new(
            Some(Path::new("/home/alice")),
            Some("alice"),
            ["ABCD2345EFGH6789JKLM234567".to_owned()],
        )
    }

    #[test]
    fn a_wine_address_stays_and_a_key_of_the_same_length_goes() {
        let text = Redactor::default().text(
            "trace:loaddll:build_module Loaded L\"C:\\windows\\system32\\wineboot.exe\" at \
             00006FFFFF5A0000: builtin, ntdll at 0000000140000000, low 00007fffa0001000 \
             key 2468BCDEFGHJKMNP hex-shaped key 2468BCDE9F7E6D4B",
        );
        assert!(text.contains("at 00006FFFFF5A0000: builtin"), "{text}");
        assert!(text.contains("0000000140000000"), "{text}");
        assert!(text.contains("00007fffa0001000"), "{text}");
        assert!(!text.contains("2468BCDEFGHJKMNP"), "{text}");
        assert!(!text.contains("2468BCDE9F7E6D4B"), "{text}");
        assert_eq!(text.matches(KEY_MASK).count(), 2, "{text}");
    }

    #[test]
    fn a_key_in_any_case_and_any_grouping_goes() {
        for probe in [
            "bcde2fghjkmn4prt",
            "bcde-2fgh-jkmn-4prt",
            "bcde 2fgh jkmn 4prt",
            "bcdefg-2hjk-mnprtv-4wxy-zbcdef",
            "BCDE FGHJ KMNP RTV2",
            "2468bcDE9f7e6D4B",
            "2468-bcde-9F7E-6D4B",
            "2468BCDE 9F7E6D4B",
            "2468-BCDE 9F7E-6D4B",
            "2468\tBCDE\t9F7E\t6D4B",
            "2468  BCDE  9F7E  6D4B",
            "2468 - BCDE - 9F7E - 6D4B",
            "bcdefg\t2hjk mnprtv-4wxy  zbcdef",
        ] {
            assert_eq!(
                Redactor::default().text(&format!("got {probe}.")),
                format!("got {KEY_MASK}."),
                "{probe}"
            );
        }
    }

    #[test]
    fn a_key_name_before_a_key_in_groups_hides_the_whole_key() {
        let redactor = Redactor::default();
        assert_eq!(
            redactor.text("CD key: 2468 BCDE 9F7E 6D4B"),
            format!("CD key: {KEY_MASK}")
        );
        assert_eq!(
            redactor.text("D2_KEY=2468 BCDE 9F7E 6D4B end"),
            format!("D2_KEY={KEY_MASK} end")
        );
        assert_eq!(
            redactor.text("key1: bcdefg 2hjk mnprtv 4wxy zbcdef"),
            format!("key1: {KEY_MASK}")
        );
        assert_eq!(
            redactor.text("D2_LOD_KEY=BCDEFG-2HJK MNPRTV-4WXY ZBCDEF"),
            format!("D2_LOD_KEY={KEY_MASK}")
        );
    }

    #[test]
    fn addresses_hashes_uuids_and_versions_with_other_characters_stay() {
        let sha = "0e8c0ea0a9bc50012fd9286d8941736f8a99d43af10413f9062db7773199efc2";
        let text = format!(
            "at 00006FFFFF5A0000: builtin, 0000000140000000 low 00007fffa0001000 {sha} \
             md5 49715a32c8e7b26f9d0e4b1f6c2a8d93 uuid 123e4567-e89b-12d3-a456-426614174000 \
             Mesa 27.0.0-devel f89f2592 wine-11.0 Box64 a46ad9df WINEDLLOVERRIDES \
             BOX64_DYNAREC_STRONGMEM=1 WINEDEBUG=+timestamp,+pid,warn+seh,+loaddll,+winsock \
             HKEY_CURRENT_USER the game will be here 2024 0x1f build 1234 5678 9abc def0 \
             pd2launcher-gecko-2468"
        );
        assert_eq!(Redactor::default().text(&text), text);
    }

    #[test]
    fn a_whole_uuid_stays_even_of_key_characters_only() {
        for uuid in [
            // Only key characters, with and without braces.
            "{E46C6E9D-E466-4484-BBFE-D4B8264B4E7F}",
            "e46c6e9d-e466-4484-bbfe-d4b8264b4e7f",
            // Its first 8-4-4 groups alone would make 16 key characters.
            "{EC64C6E9-E466-4484-BBFE-A4B8264B4371}",
        ] {
            let line = format!("Temp\\{uuid}\\.ba\\wixstdba.dll");
            assert_eq!(Redactor::default().text(&line), line, "{uuid}");
        }
        // Not a whole UUID: the key-shaped groups go.
        assert_eq!(
            Redactor::default().text("x EC64C6E9-E466-4484 y"),
            format!("x {KEY_MASK} y")
        );
    }

    #[test]
    fn a_game_logs_dump_row_keeps_its_hex_and_its_ascii_column_is_masked() {
        let redactor = Redactor::default();
        let rows = [
            "21:13:04.951  0201FF18: FFFFFFFF D87E244C AD144BBF F293C16F  TM.x..%.l......o",
            "              0201FF28: FFFFFFFF D87E244C  ....",
            "6FC16945: 8B086A00 6A006A00 8D54241C 528B1514",
        ];
        for row in rows {
            assert_eq!(redactor.game_log_text(row), row, "{row}");
        }
        // Any UTF-8 after the address or a word: no row, and no panic.
        for line in [
            "0201FF18: 54F3\u{fffd}800 x",
            "0201FF18: 54F37800 6\u{fffd}",
            "0201FF18:\u{fffd}\u{fffd}",
        ] {
            assert_eq!(redactor.game_log_text(line), redactor.text(line), "{line}");
        }
        // A key in the ASCII column of a row goes.
        assert_eq!(
            redactor.game_log_text("              0201FF38: 34363832 45444342  2468BCDE9F7E6D4B\n"),
            format!("              0201FF38: 34363832 45444342  {KEY_MASK}\n")
        );
        // Every other line is masked as text() masks it.
        assert_eq!(
            redactor.game_log_text("Stack bytes: FFFFFFFF D87E244C AD144BBF"),
            redactor.text("Stack bytes: FFFFFFFF D87E244C AD144BBF")
        );
        assert_eq!(
            redactor.game_log_text("12:00 got 2468-BCDE-9F7E-6D4B\nnext"),
            format!("12:00 got {KEY_MASK}\nnext")
        );
    }

    #[test]
    fn a_key_name_hides_the_groups_after_it_and_groups_part_by_unicode_dashes_and_spaces() {
        let redactor = Redactor::default();
        // A value that is no key shape: the groups after the name go too, up
        // to a key's 26 characters.
        assert_eq!(
            redactor.text("key1:\tABCD 2345-EFGH\t6789, end"),
            format!("key1:\t{KEY_MASK}, end")
        );
        assert_eq!(
            redactor.text("key2: ABCDEF 2345 EFGHJK 6789 MNPQRS then"),
            format!("key2: {KEY_MASK} then")
        );
        for separator in [
            "\u{00A0}", "\u{202F}", "\u{2010}", "\u{2011}", "\u{2012}", "\u{2013}", "\u{2014}",
            "\u{2212}",
        ] {
            let key = ["2468", "BCDE", "9F7E", "6D4B"].join(separator);
            assert_eq!(
                redactor.text(&format!("got {key}.")),
                format!("got {KEY_MASK}."),
                "{separator:?}"
            );
            let held = ["ABCD", "2345", "EFGH", "6789", "JKLM", "234567"].join(separator);
            assert_eq!(
                self::tests::redactor().text(&format!("a {held} b")),
                format!("a {KEY_MASK} b"),
                "{separator:?}"
            );
        }
    }

    #[test]
    fn a_key_name_hides_more_than_its_first_word_only_for_a_keys_length() {
        let redactor = Redactor::default();
        // Words after a key name that make no key's length: only the first goes.
        assert_eq!(
            redactor.text("The CD key is not valid for this realm today"),
            format!("The CD key {KEY_MASK} not valid for this realm today")
        );
        // `key2` inside a word names no key.
        let hotkey = "Hotkey2 = F4 Skill Two Left";
        assert_eq!(redactor.text(hotkey), hotkey);
        // A no-break space, an en dash, or a dash between spaces after the name.
        for separator in ["\u{00A0}", "\u{2013}", " - "] {
            assert_eq!(
                redactor.text(&format!("CD key{separator}QWERTY12 next")),
                format!("CD key{separator}{KEY_MASK} next"),
                "{separator:?}"
            );
        }
    }

    #[test]
    fn a_dump_row_hides_a_key_across_its_columns_and_a_held_key() {
        // A key that starts in the hex words and ends in the ASCII column.
        assert_eq!(
            Redactor::default().game_log_text("0201FF18: 2468BCDE  GHJKMNPR\r\n"),
            format!("0201FF18: {KEY_MASK}\r\n")
        );
        // A key this process holds, split over the two columns.
        let held = Redactor::new(None, None, ["BCDE2468FEDC9876GHJKMNPRTV".to_owned()]);
        assert_eq!(
            held.game_log_text("0201FF18: BCDE2468 FEDC9876  GHJKMNPRTV"),
            format!("0201FF18: {KEY_MASK}")
        );
        // A CRLF row keeps its hex.
        let row = "0201FF18: FFFFFFFF D87E244C AD144BBF  ....\r\n";
        assert_eq!(Redactor::default().game_log_text(row), row);
    }

    #[test]
    fn a_key_this_process_holds_goes_in_any_grouping() {
        let text =
            redactor().text("a abcd 2345-efgh\t6789 jklm 234567 b ABCD2345EFGH6789JKLM234567 c");
        assert_eq!(text, format!("a {KEY_MASK} b {KEY_MASK} c"), "{text}");
    }

    #[test]
    fn a_windows_profile_folder_hides_its_user_but_not_the_shared_ones() {
        let text = Redactor::default().text(
            "C:\\Users\\Kim Lee\\AppData\\Local\\pd2.log c:/users/kim/x \
             {\"path\":\"C:\\\\Users\\\\kim\\\\Desktop\"} C:\\Users\\Public\\Documents \
             at C:\\Users\\Bob now",
        );
        assert!(!text.contains("Kim") && !text.contains("kim"), "{text}");
        assert!(!text.contains("Bob"), "{text}");
        assert!(text.contains("C:\\Users\\<user>\\AppData"), "{text}");
        assert!(text.contains("C:\\Users\\Public\\Documents"), "{text}");
        assert!(text.ends_with("at C:\\Users\\<user>"), "{text}");
        // A name with a space and no folder after it, at a line's end and in
        // JSON, is hidden whole.
        assert_eq!(
            Redactor::default().text("Profile: C:\\Users\\Kim Lee"),
            "Profile: C:\\Users\\<user>"
        );
        assert_eq!(
            Redactor::default().text("{\"home\":\"C:\\\\Users\\\\Kim Lee\",\"x\":1}"),
            "{\"home\":\"C:\\\\Users\\\\<user>\",\"x\":1}"
        );
        assert_eq!(
            Redactor::default().text("C:\\Users\\Public"),
            "C:\\Users\\Public"
        );
        assert_eq!(
            Redactor::default().text("/home/users/notes and D:\\Games\\users.txt"),
            "/home/users/notes and D:\\Games\\users.txt"
        );
        // A name ends at a character no Windows user name holds, and an
        // apostrophe between letters is part of it.
        assert_eq!(
            Redactor::default().text("C:\\Users\\Bob, then D:\\Games\\pd2"),
            "C:\\Users\\<user>, then D:\\Games\\pd2"
        );
        assert_eq!(
            Redactor::default().text("C:\\Users\\Sean O'Brien\\AppData and 'C:\\Users\\Kim'"),
            "C:\\Users\\<user>\\AppData and 'C:\\Users\\<user>'"
        );
        assert_eq!(
            Redactor::default().text(&format!("C:\\Users\\{}", "x".repeat(70))),
            format!("C:\\Users\\<user>{}", "x".repeat(6))
        );
    }

    #[test]
    fn keys_home_user_and_url_secrets_are_hidden() {
        let redactor = redactor();
        let text = redactor.text(
            "key abcd2345efgh6789jklm234567 at /home/alice/x Z:\\home\\alice\\y C:\\users\\alice \
             https://bob:pw@mirror.example/a/b.tar?token=t0p#frag D2_KEY=QWERTY12 \
             classic 2468-BCDE-FGHJ-KMNP other 7R8W9E2T4K6Y8J9P7B8C9D2F4G typed 7r8w9e2t4k6y8j9p",
        );
        assert!(!text.to_ascii_lowercase().contains("abcd2345"), "{text}");
        assert!(!text.contains("alice"), "{text}");
        assert!(!text.contains("token") && !text.contains("bob") && !text.contains("frag"));
        assert!(text.contains("https://mirror.example/a/b.tar"), "{text}");
        assert!(!text.contains("QWERTY12"), "{text}");
        assert!(!text.contains("2468-BCDE"), "{text}");
        assert!(!text.contains("7R8W9E2T"), "{text}");
        assert!(!text.contains("7r8w9e2t"), "{text}");
        assert!(text.contains("~/x") && text.contains("~\\y"), "{text}");
        assert!(text.contains("C:\\users\\<user>"), "{text}");
    }

    #[test]
    fn ordinary_words_hashes_and_codes_survive() {
        let redactor = redactor();
        let sha = "0e8c0ea0a9bc50012fd9286d8941736f8a99d43af10413f9062db7773199efc2";
        let text = format!(
            "base-game-key-rejected Diablo II Install Log.html {sha} internationalism 2026-09-28 \
             prefix/.pd2launcher-gecko.sha256 wine-11.11-amd64-wow64 BaseGameWrapper"
        );
        assert_eq!(redactor.text(&text), text);
    }

    #[test]
    fn game_identities_are_masked() {
        let text = mask_game_identities(
            "12:00 Logon account: Hunter2Acct\nJoined game Baal-Run-12 as character=Sorc\nsaid \"hi there\"\n",
        );
        assert!(!text.contains("Hunter2Acct"), "{text}");
        assert!(!text.contains("Baal-Run-12"), "{text}");
        assert!(!text.contains("Sorc"), "{text}");
        assert!(!text.contains("hi there"), "{text}");
    }
}
