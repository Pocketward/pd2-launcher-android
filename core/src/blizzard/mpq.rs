//! The MPQ archives of Diablo II's installers and game: reading members of
//! the Installer Tome (stored or zlib, no encryption) and adding members to
//! a game archive in place.
//!
//! An addition never breaks the archive: the member data goes after the
//! archive's current end, then the new hash and block tables after it, and
//! the header that points at them is written last. Until that write the old
//! header still names the old tables, which are untouched.
//!
//! Written for this engine from the MPQ format (the hash function and the
//! table cipher are the format's own), as libd2's `formats/mpq` reads it
//! (MIT, Copyright (c) 2026 jaenster; `LICENSE-libd2` beside this file).

use std::{
    fs::File,
    io::{self, Write},
    os::unix::fs::FileExt,
    sync::OnceLock,
};

const MAGIC: &[u8; 4] = b"MPQ\x1a";
const EXISTS: u32 = 0x8000_0000;
const COMPRESSED: u32 = 0x0000_0200;
const IMPLODED: u32 = 0x0000_0100;
const ENCRYPTED: u32 = 0x0001_0000;
const SINGLE_UNIT: u32 = 0x0100_0000;
const SECTOR_CRC: u32 = 0x0400_0000;
const EMPTY: u32 = 0xffff_ffff;
const DELETED: u32 = 0xffff_fffe;
/// The most a member read into memory may hold.
const MEMORY_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MpqError {
    /// Not an MPQ this code reads: no header at the start, another format,
    /// tables beyond 4 GiB, or a table outside the file.
    Format,
    /// The member is not in the archive.
    Missing,
    /// The member is encrypted, imploded, or compressed with anything but
    /// zlib, or its data does not decompress to its size.
    Unsupported,
    /// The hash table has no free entry for a new member.
    Full,
    /// Reading or writing the file failed.
    Io,
}

impl From<io::Error> for MpqError {
    fn from(_: io::Error) -> Self {
        Self::Io
    }
}

/// The format's 0x500-word table for hashing names and ciphering tables.
fn crypt_table() -> &'static [u32; 0x500] {
    static TABLE: OnceLock<[u32; 0x500]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0_u32; 0x500];
        let mut seed = 0x0010_0001_u32;
        for first in 0..0x100 {
            for round in 0..5 {
                seed = (seed * 125 + 3) % 0x2a_aaab;
                let high = (seed & 0xffff) << 16;
                seed = (seed * 125 + 3) % 0x2a_aaab;
                table[first + round * 0x100] = high | (seed & 0xffff);
            }
        }
        table
    })
}

/// The format's string hash of `name` (upper case, `/` as `\`) for `kind`:
/// 0 the table index, 1 and 2 the two checks, 3 a table key.
pub(crate) fn hash(name: &str, kind: u32) -> u32 {
    let table = crypt_table();
    let (mut seed1, mut seed2) = (0x7fed_7fed_u32, 0xeeee_eeee_u32);
    for byte in name.bytes() {
        let byte = match byte.to_ascii_uppercase() {
            b'/' => b'\\',
            other => other,
        };
        seed1 = table[(kind as usize) * 0x100 + usize::from(byte)] ^ seed1.wrapping_add(seed2);
        seed2 = u32::from(byte)
            .wrapping_add(seed1)
            .wrapping_add(seed2)
            .wrapping_add(seed2 << 5)
            .wrapping_add(3);
    }
    seed1
}

fn cipher(words: &mut [u32], mut key: u32, decrypt: bool) {
    let table = crypt_table();
    let mut seed = 0xeeee_eeee_u32;
    for word in words {
        seed = seed.wrapping_add(table[0x400 + (key & 0xff) as usize]);
        let plain = if decrypt {
            *word ^ key.wrapping_add(seed)
        } else {
            *word
        };
        *word ^= key.wrapping_add(seed);
        key = ((!key << 21).wrapping_add(0x1111_1111)) | (key >> 11);
        seed = plain
            .wrapping_add(seed)
            .wrapping_add(seed << 5)
            .wrapping_add(3);
    }
}

#[derive(Clone, Copy, Debug)]
struct HashEntry {
    name1: u32,
    name2: u32,
    locale: u16,
    platform: u16,
    block: u32,
}

#[derive(Clone, Copy, Debug)]
struct BlockEntry {
    offset: u32,
    stored: u32,
    size: u32,
    flags: u32,
}

/// One open archive.
pub(crate) struct Archive {
    file: File,
    header: [u8; 32],
    header_size: u32,
    sector: u64,
    hashes: Vec<HashEntry>,
    blocks: Vec<BlockEntry>,
}

/// Where an addition puts everything.
struct AddPlan {
    hashes: Vec<HashEntry>,
    blocks: Vec<BlockEntry>,
    offsets: Vec<u64>,
    hash_at: u64,
}

/// One member to add: its name and its whole data, stored as it is.
pub(crate) struct NewMember<'a> {
    pub(crate) name: &'a str,
    pub(crate) data: &'a [u8],
}

impl Archive {
    /// Read the header and tables of the archive at the start of `file`.
    pub(crate) fn open(file: File) -> Result<Self, MpqError> {
        let length = file.metadata()?.len();
        let mut header = [0_u8; 44];
        let read = file.read_at(&mut header, 0)?;
        if read < 32 || &header[..4] != MAGIC {
            return Err(MpqError::Format);
        }
        let word = |at: usize| {
            u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
        };
        let header_size = word(4);
        let version = u16::from_le_bytes([header[12], header[13]]);
        let shift = u16::from_le_bytes([header[14], header[15]]);
        match (version, header_size) {
            (0, 32) => {}
            // Format 1 adds high bits for tables and blocks past 4 GiB;
            // none may be used.
            (1, 44) if read == 44 && header[32..44].iter().all(|byte| *byte == 0) => {}
            _ => return Err(MpqError::Format),
        }
        if shift > 16 {
            return Err(MpqError::Format);
        }
        let (hash_at, block_at) = (u64::from(word(16)), u64::from(word(20)));
        let (hash_count, block_count) = (word(24) as usize, word(28) as usize);
        if hash_count == 0
            || !hash_count.is_power_of_two()
            || hash_at + hash_count as u64 * 16 > length
            || block_at + block_count as u64 * 16 > length
        {
            return Err(MpqError::Format);
        }
        let hashes = read_table(&file, hash_at, hash_count, hash("(hash table)", 3))?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|entry| HashEntry {
                name1: entry[0],
                name2: entry[1],
                locale: entry[2] as u16,
                platform: (entry[2] >> 16) as u16,
                block: entry[3],
            })
            .collect();
        let blocks = read_table(&file, block_at, block_count, hash("(block table)", 3))?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|entry| BlockEntry {
                offset: entry[0],
                stored: entry[1],
                size: entry[2],
                flags: entry[3],
            })
            .collect();
        Ok(Self {
            file,
            header: header[..32].try_into().map_err(|_| MpqError::Format)?,
            header_size,
            sector: 512 << shift,
            hashes,
            blocks,
        })
    }

    /// The index of the hash entry for `name`, the neutral one first.
    fn lookup(&self, name: &str) -> Option<usize> {
        let count = self.hashes.len();
        let (start, name1, name2) = (hash(name, 0) as usize % count, hash(name, 1), hash(name, 2));
        let mut found = None;
        for step in 0..count {
            let index = (start + step) % count;
            let entry = self.hashes[index];
            if entry.block == EMPTY {
                break;
            }
            if entry.block != DELETED && entry.name1 == name1 && entry.name2 == name2 {
                if entry.locale == 0 && entry.platform == 0 {
                    return Some(index);
                }
                found = found.or(Some(index));
            }
        }
        found
    }

    fn block(&self, name: &str) -> Result<BlockEntry, MpqError> {
        let entry = self.lookup(name).ok_or(MpqError::Missing)?;
        let block = *self
            .blocks
            .get(self.hashes[entry].block as usize)
            .ok_or(MpqError::Format)?;
        if block.flags & EXISTS == 0 {
            return Err(MpqError::Missing);
        }
        if block.flags & (ENCRYPTED | IMPLODED) != 0 {
            return Err(MpqError::Unsupported);
        }
        Ok(block)
    }

    /// The size of member `name`.
    pub(crate) fn size(&self, name: &str) -> Result<u64, MpqError> {
        Ok(u64::from(self.block(name)?.size))
    }

    /// Copy member `name` to `out`, a sector at a time; the bytes copied.
    pub(crate) fn copy_member(
        &self,
        name: &str,
        out: &mut dyn Write,
        mut progress: impl FnMut(u64),
    ) -> Result<u64, MpqError> {
        let block = self.block(name)?;
        let (offset, stored, size) = (
            u64::from(block.offset),
            u64::from(block.stored),
            u64::from(block.size),
        );
        if block.flags & COMPRESSED == 0 {
            if stored < size {
                return Err(MpqError::Format);
            }
            let mut buffer = vec![0_u8; 1 << 20];
            let mut done = 0;
            while done < size {
                let length = (size - done).min(buffer.len() as u64) as usize;
                self.file
                    .read_exact_at(&mut buffer[..length], offset + done)?;
                out.write_all(&buffer[..length])?;
                done += length as u64;
                progress(done);
            }
            return Ok(size);
        }
        if block.flags & SINGLE_UNIT != 0 {
            let raw = self.read(offset, stored)?;
            let data = if stored == size {
                raw
            } else {
                inflate(&raw, size)?
            };
            out.write_all(&data)?;
            progress(size);
            return Ok(size);
        }
        let sectors = size.div_ceil(self.sector);
        let entries = sectors + 1 + u64::from(block.flags & SECTOR_CRC != 0);
        let table = self.read(offset, entries * 4)?;
        let table: Vec<u64> = table
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u64::from(u32::from_le_bytes(*word)))
            .collect();
        let mut done = 0;
        for sector in 0..sectors as usize {
            let (start, end) = (table[sector], table[sector + 1]);
            let want = (size - done).min(self.sector);
            if end < start || end > stored {
                return Err(MpqError::Format);
            }
            let raw = self.read(offset + start, end - start)?;
            let data = if raw.len() as u64 == want {
                raw
            } else {
                inflate(&raw, want)?
            };
            out.write_all(&data)?;
            done += want;
            progress(done);
        }
        Ok(size)
    }

    /// Member `name` in memory (at most 64 MiB).
    pub(crate) fn member(&self, name: &str) -> Result<Vec<u8>, MpqError> {
        let size = self.size(name)?;
        if size > MEMORY_LIMIT {
            return Err(MpqError::Unsupported);
        }
        let mut out = Vec::with_capacity(size as usize);
        self.copy_member(name, &mut out, |_| {})?;
        Ok(out)
    }

    fn read(&self, at: u64, length: u64) -> Result<Vec<u8>, MpqError> {
        if length > MEMORY_LIMIT {
            return Err(MpqError::Format);
        }
        let mut buffer = vec![0_u8; length as usize];
        self.file.read_exact_at(&mut buffer, at)?;
        Ok(buffer)
    }

    /// Whether member `name` is in the archive.
    #[cfg(test)]
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.block(name).is_ok()
    }

    /// Whether `members` can be added: every slot and offset found, nothing
    /// written. An archive that is refused or full fails here.
    pub(crate) fn check_add(&self, members: &[NewMember<'_>]) -> Result<(), MpqError> {
        self.plan_add(members).map(drop)
    }

    /// The tables after adding `members`, and where everything goes.
    fn plan_add(&self, members: &[NewMember<'_>]) -> Result<AddPlan, MpqError> {
        let mut end = self.file.metadata()?.len();
        let mut hashes = self.hashes.clone();
        let mut blocks = self.blocks.clone();
        let mut offsets = Vec::with_capacity(members.len());
        for member in members {
            let size = u32::try_from(member.data.len()).map_err(|_| MpqError::Unsupported)?;
            let offset = u32::try_from(end).map_err(|_| MpqError::Format)?;
            offsets.push(end);
            end += u64::from(size);
            let block = u32::try_from(blocks.len()).map_err(|_| MpqError::Format)?;
            blocks.push(BlockEntry {
                offset,
                stored: size,
                size,
                flags: EXISTS,
            });
            let slot = insertion_slot(&hashes, member.name).ok_or(MpqError::Full)?;
            hashes[slot] = HashEntry {
                name1: hash(member.name, 1),
                name2: hash(member.name, 2),
                locale: 0,
                platform: 0,
                block,
            };
        }
        let hash_at = end;
        let block_at = hash_at + hashes.len() as u64 * 16;
        let archive_end = block_at + blocks.len() as u64 * 16;
        for value in [archive_end, hash_at, block_at, blocks.len() as u64] {
            u32::try_from(value).map_err(|_| MpqError::Format)?;
        }
        Ok(AddPlan {
            hashes,
            blocks,
            offsets,
            hash_at,
        })
    }

    /// Add `members`, stored, replacing any with the same name: the data
    /// after the archive's end, then the new tables, then the header.
    pub(crate) fn add(&mut self, members: &[NewMember<'_>]) -> Result<(), MpqError> {
        let AddPlan {
            hashes,
            blocks,
            offsets,
            hash_at,
        } = self.plan_add(members)?;
        for (member, offset) in members.iter().zip(offsets) {
            self.file.write_all_at(member.data, offset)?;
        }
        let mut hash_words: Vec<u32> = hashes
            .iter()
            .flat_map(|entry| {
                [
                    entry.name1,
                    entry.name2,
                    u32::from(entry.locale) | u32::from(entry.platform) << 16,
                    entry.block,
                ]
            })
            .collect();
        cipher(&mut hash_words, hash("(hash table)", 3), false);
        self.file
            .write_all_at(&words_to_bytes(&hash_words), hash_at)?;
        let block_at = hash_at + hash_words.len() as u64 * 4;
        let mut block_words: Vec<u32> = blocks
            .iter()
            .flat_map(|entry| [entry.offset, entry.stored, entry.size, entry.flags])
            .collect();
        cipher(&mut block_words, hash("(block table)", 3), false);
        self.file
            .write_all_at(&words_to_bytes(&block_words), block_at)?;
        let archive_end = block_at + block_words.len() as u64 * 4;
        self.file.sync_all()?;
        let mut header = self.header;
        let mut put = |at: usize, value: u64| -> Result<(), MpqError> {
            let value = u32::try_from(value).map_err(|_| MpqError::Format)?;
            header[at..at + 4].copy_from_slice(&value.to_le_bytes());
            Ok(())
        };
        put(8, archive_end)?;
        put(16, hash_at)?;
        put(20, block_at)?;
        put(28, blocks.len() as u64)?;
        debug_assert!(self.header_size >= 32);
        self.file.write_all_at(&header, 0)?;
        self.file.sync_all()?;
        self.header = header;
        self.hashes = hashes;
        self.blocks = blocks;
        Ok(())
    }
}

/// The entry `name` replaces, or the first free one on its probe path.
fn insertion_slot(hashes: &[HashEntry], name: &str) -> Option<usize> {
    let count = hashes.len();
    let (start, name1, name2) = (hash(name, 0) as usize % count, hash(name, 1), hash(name, 2));
    let mut free = None;
    for step in 0..count {
        let index = (start + step) % count;
        let entry = hashes[index];
        if entry.block == EMPTY {
            return free.or(Some(index));
        }
        if entry.block == DELETED {
            free = free.or(Some(index));
        } else if entry.name1 == name1
            && entry.name2 == name2
            && entry.locale == 0
            && entry.platform == 0
        {
            return Some(index);
        }
    }
    free
}

fn read_table(file: &File, at: u64, entries: usize, key: u32) -> Result<Vec<u32>, MpqError> {
    let mut bytes = vec![0_u8; entries * 16];
    file.read_exact_at(&mut bytes, at)?;
    let mut words: Vec<u32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| u32::from_le_bytes(*word))
        .collect();
    cipher(&mut words, key, true);
    Ok(words)
}

fn words_to_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

/// One compressed unit: a mask byte, then zlib data (the only one used).
fn inflate(raw: &[u8], size: u64) -> Result<Vec<u8>, MpqError> {
    let (Some(&0x02), Some(data)) = (raw.first(), raw.get(1..)) else {
        return Err(MpqError::Unsupported);
    };
    let out = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, size as usize)
        .map_err(|_| MpqError::Unsupported)?;
    if out.len() as u64 != size {
        return Err(MpqError::Unsupported);
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A synthetic format-0 archive with `members` (name, data, compressed
    /// in sectors when `zlib`), and its hash table of `slots` entries.
    pub(crate) fn build(
        members: &[(&str, &[u8], bool)],
        slots: usize,
        sector_shift: u16,
    ) -> Vec<u8> {
        let sector = 512_usize << sector_shift;
        let mut out = vec![0_u8; 32];
        let mut blocks = Vec::new();
        for (_, data, zlib) in members {
            let offset = out.len() as u32;
            if *zlib {
                let sectors = data.len().div_ceil(sector);
                let mut table = Vec::new();
                let mut body = Vec::new();
                let mut at = (sectors + 1) * 4;
                for chunk in data.chunks(sector) {
                    table.push(at as u32);
                    let mut packed = vec![0x02];
                    packed.extend(miniz_oxide::deflate::compress_to_vec_zlib(chunk, 6));
                    let unit = if packed.len() < chunk.len() {
                        packed
                    } else {
                        chunk.to_vec()
                    };
                    at += unit.len();
                    body.extend(unit);
                }
                table.push(at as u32);
                out.extend(words_to_bytes(&table));
                out.extend(body);
                blocks.push([offset, at as u32, data.len() as u32, EXISTS | COMPRESSED]);
            } else {
                out.extend_from_slice(data);
                blocks.push([offset, data.len() as u32, data.len() as u32, EXISTS]);
            }
        }
        let mut hashes = vec![[EMPTY, EMPTY, 0xffff_ffff, EMPTY]; slots];
        for (block, (name, _, _)) in members.iter().enumerate() {
            let mut index = hash(name, 0) as usize % slots;
            while hashes[index][3] != EMPTY {
                index = (index + 1) % slots;
            }
            hashes[index] = [hash(name, 1), hash(name, 2), 0, block as u32];
        }
        let hash_at = out.len();
        let mut words: Vec<u32> = hashes.concat();
        cipher(&mut words, hash("(hash table)", 3), false);
        out.extend(words_to_bytes(&words));
        let block_at = out.len();
        let mut words: Vec<u32> = blocks.concat();
        cipher(&mut words, hash("(block table)", 3), false);
        out.extend(words_to_bytes(&words));
        let length = out.len();
        let mut header = Vec::new();
        header.extend_from_slice(MAGIC);
        header.extend(words_to_bytes(&[32, length as u32]));
        header.extend(0_u16.to_le_bytes());
        header.extend(sector_shift.to_le_bytes());
        header.extend(words_to_bytes(&[
            hash_at as u32,
            block_at as u32,
            slots as u32,
            blocks.len() as u32,
        ]));
        out[..32].copy_from_slice(&header);
        out
    }

    fn open(bytes: &[u8]) -> (tempfile::NamedTempFile, Archive) {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(bytes).unwrap();
        let archive = Archive::open(file.reopen().unwrap()).unwrap();
        (file, archive)
    }

    fn payload(length: usize, seed: u8) -> Vec<u8> {
        (0..length)
            .map(|index| ((index / 7) as u8).wrapping_mul(13).wrapping_add(seed))
            .collect()
    }

    #[test]
    fn the_format_hash_and_cipher_match_known_values() {
        // The format's well-known table keys.
        assert_eq!(hash("(hash table)", 3), 0xc3af_3770);
        assert_eq!(hash("(block table)", 3), 0xec83_b3a3);
        // Case and separators do not matter.
        assert_eq!(hash(r"data\global\x.wav", 1), hash("DATA/GLOBAL/X.WAV", 1));
        let mut words = [1_u32, 2, 3, 0xdead_beef];
        cipher(&mut words, 0x1234_5678, false);
        assert_ne!(words, [1, 2, 3, 0xdead_beef]);
        cipher(&mut words, 0x1234_5678, true);
        assert_eq!(words, [1, 2, 3, 0xdead_beef]);
    }

    #[test]
    fn stored_and_zlib_members_are_read() {
        let stored = payload(10_000, 1);
        let packed = payload(20_000, 2);
        let bytes = build(
            &[
                (r"Common\a.mpq", &stored, false),
                (r"PC\b.dll", &packed, true),
            ],
            16,
            3,
        );
        let (_file, archive) = open(&bytes);
        assert_eq!(archive.member(r"common\A.MPQ").unwrap(), stored);
        assert_eq!(archive.member(r"PC\b.dll").unwrap(), packed);
        let mut seen = Vec::new();
        let mut copied = Vec::new();
        archive
            .copy_member(r"PC\b.dll", &mut copied, |done| seen.push(done))
            .unwrap();
        assert_eq!(copied, packed);
        assert_eq!(seen.last(), Some(&20_000));
        assert_eq!(archive.member("missing").unwrap_err(), MpqError::Missing);
    }

    #[test]
    fn added_members_replace_by_name_and_keep_the_others() {
        let first = payload(3_000, 3);
        let second = payload(5_000, 4);
        let bytes = build(&[("a.dll", &first, false), ("b.dll", &second, true)], 8, 3);
        let (file, mut archive) = open(&bytes);
        let new = payload(1_234, 5);
        let key = payload(72, 6);
        archive
            .add(&[
                NewMember {
                    name: "a.dll",
                    data: &new,
                },
                NewMember {
                    name: r"data\global\sfx\cursor\wavindx.wav",
                    data: &key,
                },
            ])
            .unwrap();
        let reopened = Archive::open(file.reopen().unwrap()).unwrap();
        assert_eq!(reopened.member("a.dll").unwrap(), new);
        assert_eq!(reopened.member("b.dll").unwrap(), second);
        assert_eq!(
            reopened
                .member(r"DATA\GLOBAL\SFX\CURSOR\WAVINDX.WAV")
                .unwrap(),
            key
        );
        // Added again: the same entry, not a second one.
        let mut archive = reopened;
        archive
            .add(&[NewMember {
                name: "a.dll",
                data: &first,
            }])
            .unwrap();
        let reopened = Archive::open(file.reopen().unwrap()).unwrap();
        assert_eq!(reopened.member("a.dll").unwrap(), first);
        assert_eq!(
            reopened
                .hashes
                .iter()
                .filter(|entry| entry.block != EMPTY)
                .count(),
            3
        );
    }

    #[test]
    fn a_stop_before_the_header_leaves_the_old_archive() {
        let first = payload(3_000, 7);
        let bytes = build(&[("a.dll", &first, false)], 8, 3);
        let (file, mut archive) = open(&bytes);
        archive
            .add(&[NewMember {
                name: "b.dll",
                data: &payload(100, 8),
            }])
            .unwrap();
        // The bytes as they were before the header write: the old header.
        let mut stopped = std::fs::read(file.path()).unwrap();
        stopped[..32].copy_from_slice(&bytes[..32]);
        let (_file, before) = open(&stopped);
        assert_eq!(before.member("a.dll").unwrap(), first);
        assert!(!before.contains("b.dll"));
    }

    #[test]
    fn a_full_table_and_unsupported_members_fail_closed() {
        let data = payload(10, 9);
        let bytes = build(&[("a", &data, false), ("b", &data, false)], 2, 3);
        let (file, mut archive) = open(&bytes);
        let new = [NewMember {
            name: "c",
            data: &data,
        }];
        assert_eq!(archive.check_add(&new).unwrap_err(), MpqError::Full);
        assert_eq!(archive.add(&new).unwrap_err(), MpqError::Full);
        // Refused before any write.
        assert_eq!(std::fs::read(file.path()).unwrap(), bytes);
        assert!(Archive::open(tempfile::tempfile().unwrap()).is_err());
        let mut broken = bytes.clone();
        broken[0] = b'X';
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&broken).unwrap();
        assert_eq!(
            Archive::open(file.reopen().unwrap()).err(),
            Some(MpqError::Format)
        );
        // A compressed unit with another method than zlib.
        assert_eq!(
            inflate(&[0x08, 1, 2], 10).unwrap_err(),
            MpqError::Unsupported
        );
    }
}
