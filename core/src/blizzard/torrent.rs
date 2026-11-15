//! The torrent inside Blizzard's legacy downloader wrapper: bencode read
//! with the byte span of every value, the infohash over the original bytes
//! of `info`, the CDN access cookie the wrapper carries after the torrent,
//! the piece servers, and which file bytes each piece covers.
//!
//! Translated from blizzard-legacy-dl's `src/legacy.zig` (MIT, Copyright (c)
//! 2026 jaenster; `LICENSE` beside this file).

use std::{fmt, ops::Range};

use sha1::{Digest, Sha1};
use zeroize::Zeroize;

/// Nesting deeper than this is refused; the wrapper's torrent nests four
/// levels.
const MAX_DEPTH: usize = 32;
/// Values one decode may read; the wrapper's torrent has some ten thousand
/// (its file list), so more is refused, never allocated.
const MAX_NODES: usize = 1 << 20;

/// Servers one `direct download` value may name once its host groups are
/// expanded; more is refused, never allocated.
const MAX_SERVERS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TorrentError {
    /// No `d8:announce` dictionary with an `info` dictionary in the bytes.
    NoTorrent,
    /// The torrent is not valid bencode, or a value has the wrong shape.
    Malformed,
}

/// One bencoded value and the bytes it was read from.
#[derive(Debug)]
pub(crate) struct Node<'a> {
    pub(crate) value: Value<'a>,
    pub(crate) span: Range<usize>,
}

#[derive(Debug)]
pub(crate) enum Value<'a> {
    Integer(i64),
    Bytes(&'a [u8]),
    List(Vec<Node<'a>>),
    Dictionary(Vec<(&'a [u8], Node<'a>)>),
}

impl<'a> Node<'a> {
    /// The first value of `key` in a dictionary.
    pub(crate) fn get(&self, key: &str) -> Option<&Node<'a>> {
        match &self.value {
            Value::Dictionary(entries) => entries
                .iter()
                .find(|(name, _)| *name == key.as_bytes())
                .map(|(_, node)| node),
            _ => None,
        }
    }

    pub(crate) fn bytes(&self) -> Option<&'a [u8]> {
        match self.value {
            Value::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }

    pub(crate) fn integer(&self) -> Option<i64> {
        match self.value {
            Value::Integer(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn list(&self) -> Option<&[Node<'a>]> {
        match &self.value {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    fn text(&self) -> Option<&'a str> {
        self.bytes()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
    }

    fn unsigned(&self) -> Option<u64> {
        self.integer().and_then(|value| u64::try_from(value).ok())
    }
}

/// Decode the value that starts at `at`. Its span ends just past it, so a
/// caller can hash the exact bytes a dictionary was read from.
pub(crate) fn decode(bytes: &[u8], at: usize) -> Result<Node<'_>, TorrentError> {
    decode_at(bytes, at, 0, &mut 0)
}

fn decode_at<'a>(
    bytes: &'a [u8],
    at: usize,
    depth: usize,
    nodes: &mut usize,
) -> Result<Node<'a>, TorrentError> {
    *nodes += 1;
    if depth > MAX_DEPTH || *nodes > MAX_NODES {
        return Err(TorrentError::Malformed);
    }
    match bytes.get(at) {
        Some(b'i') => {
            let end = find(bytes, at + 1, b"e").ok_or(TorrentError::Malformed)?;
            let value = std::str::from_utf8(&bytes[at + 1..end])
                .ok()
                .and_then(|text| text.parse::<i64>().ok())
                .ok_or(TorrentError::Malformed)?;
            Ok(Node {
                value: Value::Integer(value),
                span: at..end + 1,
            })
        }
        Some(b'l') => {
            let mut items = Vec::new();
            let mut next = at + 1;
            loop {
                match bytes.get(next) {
                    Some(b'e') => break,
                    Some(_) => {
                        let item = decode_at(bytes, next, depth + 1, nodes)?;
                        next = item.span.end;
                        items.push(item);
                    }
                    None => return Err(TorrentError::Malformed),
                }
            }
            Ok(Node {
                value: Value::List(items),
                span: at..next + 1,
            })
        }
        Some(b'd') => {
            let mut entries = Vec::new();
            let mut next = at + 1;
            loop {
                match bytes.get(next) {
                    Some(b'e') => break,
                    Some(_) => {
                        let key = decode_at(bytes, next, depth + 1, nodes)?;
                        let Value::Bytes(name) = key.value else {
                            return Err(TorrentError::Malformed);
                        };
                        let value = decode_at(bytes, key.span.end, depth + 1, nodes)?;
                        next = value.span.end;
                        entries.push((name, value));
                    }
                    None => return Err(TorrentError::Malformed),
                }
            }
            Ok(Node {
                value: Value::Dictionary(entries),
                span: at..next + 1,
            })
        }
        Some(b'0'..=b'9') => {
            let colon = find(bytes, at, b":").ok_or(TorrentError::Malformed)?;
            let length = std::str::from_utf8(&bytes[at..colon])
                .ok()
                .and_then(|text| text.parse::<usize>().ok())
                .ok_or(TorrentError::Malformed)?;
            let start = colon + 1;
            let end = start
                .checked_add(length)
                .filter(|end| *end <= bytes.len())
                .ok_or(TorrentError::Malformed)?;
            Ok(Node {
                value: Value::Bytes(&bytes[start..end]),
                span: at..end,
            })
        }
        _ => Err(TorrentError::Malformed),
    }
}

fn find(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|found| from + found)
}

/// One file of the payload, in torrent order; `path` joins its parts with `/`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TorrentFile {
    pub(crate) path: String,
    pub(crate) length: u64,
}

/// One HTTP piece source and the pieces it serves (inclusive).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Server {
    pub(crate) url: String,
    pub(crate) first: u64,
    pub(crate) last: u64,
}

impl Server {
    fn everything(url: String) -> Self {
        Self {
            url,
            first: 0,
            last: u64::MAX,
        }
    }
}

/// Where in which file a run of a piece's bytes belongs. Pieces do not
/// respect file borders: one often ends one file and starts the next.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Span {
    pub(crate) file: usize,
    pub(crate) offset: u64,
    pub(crate) length: u64,
}

/// The metainfo of one payload, as the wrapper carries it.
#[derive(Clone, Debug)]
pub(crate) struct Torrent {
    pub(crate) name: String,
    pub(crate) piece_length: u64,
    pieces: Vec<[u8; 20]>,
    pub(crate) files: Vec<TorrentFile>,
    pub(crate) total: u64,
    /// SHA-1 of the original bytes of `info`: re-encoding it would have to
    /// reproduce Blizzard's key order exactly.
    pub(crate) infohash: [u8; 20],
    /// Every piece source: the expansion of `direct download`, then each
    /// `server list` entry with its range.
    pub(crate) servers: Vec<Server>,
}

impl Torrent {
    /// Find and read the torrent a downloader wrapper carries. Every one
    /// starts its dictionary with the announce key, which is more robust to
    /// look for than a section layout.
    pub(crate) fn from_stub(stub: &[u8]) -> Result<Self, TorrentError> {
        let mut search = 0;
        while let Some(at) = find(stub, search, b"d8:announce") {
            search = at + 1;
            let Ok(root) = decode(stub, at) else {
                continue;
            };
            let Some(info) = root.get("info") else {
                continue;
            };
            if !matches!(info.value, Value::Dictionary(_)) {
                continue;
            }
            return Self::read(stub, &root, info);
        }
        Err(TorrentError::NoTorrent)
    }

    fn read(stub: &[u8], root: &Node<'_>, info: &Node<'_>) -> Result<Self, TorrentError> {
        let malformed = TorrentError::Malformed;
        let infohash: [u8; 20] = Sha1::digest(&stub[info.span.clone()]).into();
        let name = info
            .get("name")
            .and_then(Node::text)
            .ok_or(malformed)?
            .to_owned();
        let piece_length = info
            .get("piece length")
            .and_then(Node::unsigned)
            .filter(|length| *length > 0)
            .ok_or(malformed)?;
        let pieces = info.get("pieces").and_then(Node::bytes).ok_or(malformed)?;
        if pieces.is_empty() || pieces.len() % 20 != 0 {
            return Err(malformed);
        }
        let pieces = pieces.as_chunks::<20>().0.to_vec();
        let mut files = Vec::new();
        match info.get("files") {
            Some(list) => {
                for file in list.list().ok_or(malformed)? {
                    let length = file
                        .get("length")
                        .and_then(Node::unsigned)
                        .ok_or(malformed)?;
                    let mut parts = Vec::new();
                    for part in file.get("path").and_then(Node::list).ok_or(malformed)? {
                        let part = part.text().ok_or(malformed)?;
                        if part.is_empty()
                            || part == "."
                            || part == ".."
                            || part.contains(['/', '\\', '\0'])
                        {
                            return Err(malformed);
                        }
                        parts.push(part);
                    }
                    if parts.is_empty() {
                        return Err(malformed);
                    }
                    files.push(TorrentFile {
                        path: parts.join("/"),
                        length,
                    });
                }
            }
            None => files.push(TorrentFile {
                path: name.clone(),
                length: info
                    .get("length")
                    .and_then(Node::unsigned)
                    .ok_or(malformed)?,
            }),
        }
        let total = files
            .iter()
            .try_fold(0_u64, |total, file| total.checked_add(file.length))
            .ok_or(malformed)?;
        if total.div_ceil(piece_length) != pieces.len() as u64 {
            return Err(malformed);
        }
        let direct_download = match root.get("direct download") {
            Some(value) => value.text().ok_or(malformed)?,
            None => "",
        };
        let mut servers = expand_servers(direct_download).ok_or(malformed)?;
        if let Some(list) = root.get("server list") {
            // An entry missing its url, begin or end is skipped, as
            // Blizzard's downloader skips it.
            for entry in list.list().ok_or(malformed)? {
                let (Some(url), Some(first), Some(last)) = (
                    entry.get("url").and_then(Node::text),
                    entry.get("begin").and_then(Node::unsigned),
                    entry.get("end").and_then(Node::unsigned),
                ) else {
                    continue;
                };
                if servers.len() >= MAX_SERVERS {
                    return Err(malformed);
                }
                servers.push(Server {
                    url: url.to_owned(),
                    first,
                    last,
                });
            }
        }
        Ok(Self {
            name,
            piece_length,
            pieces,
            files,
            total,
            infohash,
            servers,
        })
    }

    pub(crate) fn piece_count(&self) -> usize {
        self.pieces.len()
    }

    /// The last piece is short when the payload is not a whole number of
    /// pieces.
    pub(crate) fn piece_size(&self, index: usize) -> u64 {
        let start = index as u64 * self.piece_length;
        self.total.saturating_sub(start).min(self.piece_length)
    }

    /// Whether `data` is piece `index`: its length, then its SHA-1.
    pub(crate) fn verify(&self, index: usize, data: &[u8]) -> bool {
        data.len() as u64 == self.piece_size(index)
            && self
                .pieces
                .get(index)
                .is_some_and(|hash| <[u8; 20]>::from(Sha1::digest(data)) == *hash)
    }

    /// The runs of file bytes piece `index` covers, in order.
    pub(crate) fn spans(&self, index: usize) -> Vec<Span> {
        let mut spans = Vec::new();
        let mut wanted = self.piece_size(index);
        let mut at = index as u64 * self.piece_length;
        let mut start = 0_u64;
        for (file, entry) in self.files.iter().enumerate() {
            if wanted == 0 {
                break;
            }
            let end = start + entry.length;
            if at < end {
                let length = wanted.min(end - at);
                spans.push(Span {
                    file,
                    offset: at - start,
                    length,
                });
                at += length;
                wanted -= length;
            }
            start = end;
        }
        spans
    }

    pub(crate) fn infohash_hex(&self) -> String {
        self.infohash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// Expand a `direct download` value into the servers it names, as
/// Blizzard's downloader does: split on `|`; a URL with a `[...]` group in
/// its host expands it, the group's body split on `,` and each item an
/// inclusive range on `-`, every number N giving `prefix + N + path`. The
/// path starts at the first `/` after the scheme, so host text after `]`
/// is dropped (the bracket is meant to end the host name). Anything else
/// passes as it is. `None` when it names more than [`MAX_SERVERS`].
pub(crate) fn expand_servers(value: &str) -> Option<Vec<Server>> {
    let mut servers = Vec::new();
    let mut push = |url: String| {
        if servers.len() >= MAX_SERVERS {
            return false;
        }
        servers.push(Server::everything(url));
        true
    };
    for url in value.split('|').filter(|url| !url.is_empty()) {
        let Some(group) = host_group(url) else {
            if !push(url.to_owned()) {
                return None;
            }
            continue;
        };
        let (prefix, body, path) = group;
        for item in body.split(',') {
            let mut ends = item.split('-');
            let Some(first) = ends.next().and_then(|text| text.parse::<u64>().ok()) else {
                continue;
            };
            let last = ends
                .next()
                .map_or(first, |text| text.parse::<u64>().unwrap_or(first));
            let mut number = first;
            while number <= last {
                if !push(format!("{prefix}{number}{path}")) {
                    return None;
                }
                number += 1;
            }
        }
    }
    Some(servers)
}

/// A `[...]` group in a plain-HTTP URL's host: the text before it, its
/// body, and the path.
fn host_group(url: &str) -> Option<(&str, &str, &str)> {
    if !url.starts_with("http://") {
        return None;
    }
    // Searched from the first byte of the host on, as Blizzard's downloader
    // does.
    let path = url.get(8..)?.find('/')? + 8;
    let open = url[8..].find('[')? + 8;
    let close = url[8..].find(']')? + 8;
    (open < path && close < path && open < close)
        .then(|| (&url[..open], &url[open + 1..close], &url[path..]))
}

/// The CDN access cookie a downloader wrapper carries after its torrent:
/// `<name>=expires=<unix time>~access=<path glob>~md5=<32 hex>`. The piece
/// host answers every request without it with 403. It is never printed:
/// `Debug` shows only its expiry.
pub(crate) struct AccessCookie {
    value: Vec<u8>,
    expires: u64,
}

impl AccessCookie {
    /// Find the cookie, anchored on `~md5=` and 32 hex digits, then walked
    /// back over the cookie's own characters (the bytes before it are the
    /// wrapper's tag). A real one names its expiry and the path it grants;
    /// anything else is a chance match.
    pub(crate) fn find(stub: &[u8]) -> Option<Self> {
        const ANCHOR: &[u8] = b"~md5=";
        let cookie_byte = |byte: u8| byte.is_ascii_alphanumeric() || b"=~/*._%-".contains(&byte);
        let mut search = 0;
        while let Some(anchor) = find(stub, search, ANCHOR) {
            search = anchor + 1;
            let digest = anchor + ANCHOR.len();
            let hex = stub.get(digest..digest + 32)?;
            if !hex.iter().all(u8::is_ascii_hexdigit) {
                continue;
            }
            let mut start = anchor;
            while start > 0 && cookie_byte(stub[start - 1]) {
                start -= 1;
            }
            let value = &stub[start..digest + 32];
            if find(value, 0, b"~access=").is_none() {
                continue;
            }
            let Some(expires) = expiry(value) else {
                continue;
            };
            return Some(Self {
                value: value.to_vec(),
                expires,
            });
        }
        None
    }

    /// The Unix time after which the piece host refuses it.
    pub(crate) fn expires(&self) -> u64 {
        self.expires
    }

    /// The `Cookie` header's value.
    pub(crate) fn header_value(&self) -> &[u8] {
        &self.value
    }

    #[cfg(test)]
    pub(crate) fn for_tests(value: &str, expires: u64) -> Self {
        Self {
            value: value.as_bytes().to_vec(),
            expires,
        }
    }
}

fn expiry(cookie: &[u8]) -> Option<u64> {
    let at = find(cookie, 0, b"expires=")? + b"expires=".len();
    let digits = cookie[at..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    std::str::from_utf8(&cookie[at..at + digits])
        .ok()?
        .parse()
        .ok()
}

impl Drop for AccessCookie {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

impl fmt::Debug for AccessCookie {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccessCookie")
            .field("value", &"[REDACTED]")
            .field("expires", &self.expires)
            .finish()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A bencoded byte string.
    pub(crate) fn text(value: &[u8]) -> Vec<u8> {
        let mut out = format!("{}:", value.len()).into_bytes();
        out.extend_from_slice(value);
        out
    }

    /// A synthetic torrent over `payload` cut into `files`, as Blizzard's
    /// wrappers lay it out: `direct download` names `base`.
    pub(crate) fn torrent_bytes(
        name: &str,
        base: &str,
        piece_length: usize,
        files: &[(&str, usize)],
        payload: &[u8],
    ) -> Vec<u8> {
        assert_eq!(
            files.iter().map(|(_, length)| length).sum::<usize>(),
            payload.len()
        );
        let mut pieces = Vec::new();
        for piece in payload.chunks(piece_length) {
            pieces.extend_from_slice(&Sha1::digest(piece));
        }
        let mut info = b"d5:filesl".to_vec();
        for (path, length) in files {
            info.extend_from_slice(format!("d6:lengthi{length}e4:pathl").as_bytes());
            for part in path.split('/') {
                info.extend(text(part.as_bytes()));
            }
            info.extend_from_slice(b"ee");
        }
        info.extend_from_slice(b"e4:name");
        info.extend(text(name.as_bytes()));
        info.extend_from_slice(format!("12:piece lengthi{piece_length}e6:pieces").as_bytes());
        info.extend(text(&pieces));
        info.push(b'e');
        let mut torrent = b"d8:announce".to_vec();
        torrent.extend(text(b"http://tracker.invalid:3724/announce"));
        torrent.extend_from_slice(b"15:direct download");
        torrent.extend(text(base.as_bytes()));
        torrent.extend_from_slice(b"11:disable p2pi1e4:info");
        torrent.extend(info);
        torrent.push(b'e');
        torrent
    }

    /// A synthetic wrapper: program bytes, a false start, the torrent, then
    /// the cookie between two tags, each four letters and four binary bytes
    /// as in Blizzard's wrappers.
    pub(crate) fn stub_bytes(torrent: &[u8], cookie: &str) -> Vec<u8> {
        let mut stub = b"MZ\x90\x00 not a torrent: d8:announce 3:abc".to_vec();
        stub.extend_from_slice(&[0_u8; 64]);
        stub.extend_from_slice(torrent);
        stub.extend_from_slice(b"TAGA\x13\x18\xfe\xff");
        stub.extend_from_slice(cookie.as_bytes());
        stub.extend_from_slice(b"TAGB\x97\xff\xff\xff");
        stub
    }

    pub(crate) const FAKE_COOKIE: &str = "fake_cookie=expires=1900000000~access=/applications/x/*~md5=0123456789abcdef0123456789ABCDEF";

    fn payload(length: usize) -> Vec<u8> {
        (0..length).map(|index| (index * 7 % 251) as u8).collect()
    }

    #[test]
    fn bencode_keeps_the_span_of_every_value() {
        let bytes = b"xxd3:cow3:moo4:spami-42e4:listli1ei2eee";
        let root = decode(bytes, 2).unwrap();
        assert_eq!(root.span, 2..bytes.len());
        assert_eq!(root.get("cow").unwrap().bytes(), Some(&b"moo"[..]));
        assert_eq!(root.get("spam").unwrap().integer(), Some(-42));
        let list = root.get("list").unwrap();
        assert_eq!(list.list().unwrap().len(), 2);
        assert_eq!(&bytes[list.span.clone()], b"li1ei2ee");
        for broken in [
            &b"d3:cow"[..],
            b"i12",
            b"5:abc",
            b"l",
            b"di1e1:ae",
            b"x",
            b"i1x2e",
        ] {
            assert_eq!(decode(broken, 0).unwrap_err(), TorrentError::Malformed);
        }
        let deep = format!("{}{}", "l".repeat(MAX_DEPTH + 2), "e".repeat(MAX_DEPTH + 2));
        assert!(decode(deep.as_bytes(), 0).is_err());
        // Too many values, flat: refused at the cap.
        let wide = format!("l{}e", "i0e".repeat(MAX_NODES));
        assert!(decode(wide.as_bytes(), 0).is_err());
        let fits = format!("l{}e", "i0e".repeat(1000));
        assert!(decode(fits.as_bytes(), 0).is_ok());
    }

    #[test]
    fn a_synthetic_wrapper_gives_its_torrent_infohash_and_cookie() {
        let data = payload(1000);
        let torrent = torrent_bytes(
            "Synthetic-Installer",
            "http://pieces.invalid/applications/x/enUS",
            256,
            &[("a/one.bin", 300), ("two.bin", 450), ("Mac.app/three", 250)],
            &data,
        );
        let stub = stub_bytes(&torrent, FAKE_COOKIE);
        let read = Torrent::from_stub(&stub).unwrap();
        assert_eq!(read.name, "Synthetic-Installer");
        assert_eq!(read.piece_length, 256);
        assert_eq!(read.piece_count(), 4);
        assert_eq!(read.total, 1000);
        assert_eq!(read.files[0].path, "a/one.bin");
        assert_eq!(
            read.servers,
            [Server::everything(
                "http://pieces.invalid/applications/x/enUS".to_owned()
            )]
        );
        // The infohash is the SHA-1 of the info dictionary's own bytes.
        let at = find(&torrent, 0, b"4:infod").unwrap() + 6;
        let info = decode(&torrent, at).unwrap();
        let expected: [u8; 20] = Sha1::digest(&torrent[info.span]).into();
        assert_eq!(read.infohash, expected);
        assert_eq!(read.infohash_hex().len(), 40);
        for (index, piece) in data.chunks(256).enumerate() {
            assert!(read.verify(index, piece));
        }
        assert!(!read.verify(1, &data[..256]));
        assert!(!read.verify(3, &data[768..999]));

        let cookie = AccessCookie::find(&stub).unwrap();
        assert_eq!(cookie.header_value(), FAKE_COOKIE.as_bytes());
        assert_eq!(cookie.expires(), 1_900_000_000);
        let shown = format!("{cookie:?}");
        assert!(
            !shown.contains("md5") && !shown.contains("fake_cookie"),
            "{shown}"
        );
    }

    #[test]
    fn a_cookie_must_name_its_expiry_and_its_path() {
        let digest = "0123456789abcdef0123456789abcdef";
        for chance in [
            format!("x=~md5={digest}"),
            format!("x=expires=5~md5={digest}"),
            format!("x=~access=/a~md5={digest}"),
            "x=expires=5~access=/a~md5=0123".to_owned(),
            format!("x=expires=5~access=/a~md5={}", "z".repeat(32)),
        ] {
            assert!(AccessCookie::find(chance.as_bytes()).is_none(), "{chance}");
        }
        // A chance match first, the real one later.
        let stub = format!(
            "\x00~md5={}\x00name=expires=7~access=/a/*~md5={digest}\x00",
            "g".repeat(32)
        );
        assert_eq!(AccessCookie::find(stub.as_bytes()).unwrap().expires(), 7);
    }

    #[test]
    fn no_torrent_and_a_broken_torrent_are_told_apart() {
        assert_eq!(
            Torrent::from_stub(b"MZ nothing here").unwrap_err(),
            TorrentError::NoTorrent
        );
        let data = payload(100);
        let mut torrent = torrent_bytes("n", "http://h/p", 64, &[("f", 100)], &data);
        // One piece hash too few: the file list and the pieces disagree.
        let at = find(&torrent, 0, b"6:pieces40:").unwrap();
        torrent.splice(at..at + 11 + 20, b"6:pieces20:".iter().copied());
        assert_eq!(
            Torrent::from_stub(&torrent).unwrap_err(),
            TorrentError::Malformed
        );
        let unsafe_path = torrent_bytes("n", "http://h/p", 64, &[("../f", 100)], &data);
        assert_eq!(
            Torrent::from_stub(&unsafe_path).unwrap_err(),
            TorrentError::Malformed
        );
    }

    #[test]
    fn direct_download_expands_as_blizzards_downloader_expands_it() {
        let urls = |value: &str| {
            expand_servers(value)
                .unwrap()
                .into_iter()
                .map(|server| server.url)
                .collect::<Vec<_>>()
        };
        assert_eq!(urls("http://a.example/p"), ["http://a.example/p"]);
        assert_eq!(
            urls("http://a.example/p|http://b.example/q|"),
            ["http://a.example/p", "http://b.example/q"]
        );
        assert_eq!(
            urls("http://dl[1-3,7]/x"),
            [
                "http://dl1/x",
                "http://dl2/x",
                "http://dl3/x",
                "http://dl7/x"
            ]
        );
        // Host text after the bracket is dropped.
        assert_eq!(
            urls("http://dl[1-2].example/x"),
            ["http://dl1/x", "http://dl2/x"]
        );
        // A bracket in the path, or not plain HTTP: left as it is.
        assert_eq!(urls("http://a.example/p[1-3]"), ["http://a.example/p[1-3]"]);
        assert_eq!(urls("https://dl[1-2]/x"), ["https://dl[1-2]/x"]);
        // A bad item is skipped; a bad range end is the start.
        assert_eq!(urls("http://dl[x,4-y]/z"), ["http://dl4/z"]);
        assert!(expand_servers("http://dl[0-100000]/x").is_none());
        assert!(
            expand_servers("http://a/p")
                .unwrap()
                .iter()
                .all(|server| server.first == 0 && server.last == u64::MAX)
        );
    }

    #[test]
    fn a_server_list_adds_servers_with_their_ranges() {
        let data = payload(10);
        let mut torrent = torrent_bytes("n", "http://all/p", 4, &[("f", 10)], &data);
        torrent.pop();
        // The second entry has no range and is skipped.
        torrent.extend_from_slice(
            b"11:server listld5:begini1e3:endi2e3:url8:http://bed3:url5:http:eee",
        );
        let read = Torrent::from_stub(&torrent).unwrap();
        assert_eq!(read.servers.len(), 2);
        assert_eq!(
            read.servers[1],
            Server {
                url: "http://b".to_owned(),
                first: 1,
                last: 2
            }
        );
    }

    #[test]
    fn pieces_cross_file_borders() {
        let data = payload(1000);
        let torrent = Torrent::from_stub(&torrent_bytes(
            "n",
            "http://h/p",
            256,
            &[("one", 300), ("two", 450), ("three", 250)],
            &data,
        ))
        .unwrap();
        let span = |file, offset, length| Span {
            file,
            offset,
            length,
        };
        assert_eq!(torrent.spans(0), [span(0, 0, 256)]);
        assert_eq!(torrent.spans(1), [span(0, 256, 44), span(1, 0, 212)]);
        assert_eq!(torrent.spans(2), [span(1, 212, 238), span(2, 0, 18)]);
        // The last piece is short.
        assert_eq!(torrent.piece_size(3), 232);
        assert_eq!(torrent.spans(3), [span(2, 18, 232)]);
        // Every byte is covered once.
        let covered: u64 = (0..torrent.piece_count())
            .flat_map(|piece| torrent.spans(piece))
            .map(|span| span.length)
            .sum();
        assert_eq!(covered, 1000);
        // An empty file between two others takes no bytes.
        let gap = Torrent::from_stub(&torrent_bytes(
            "n",
            "http://h/p",
            8,
            &[("a", 5), ("empty", 0), ("b", 5)],
            &data[..10],
        ))
        .unwrap();
        assert_eq!(gap.spans(0), [span(0, 0, 5), span(2, 0, 3)]);
    }
}
