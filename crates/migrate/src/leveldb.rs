//! A read-only LevelDB reader, enough to read the desktop's localStorage
//! (Chromium keeps it in `<userData>/Local Storage/leveldb/`; OI-10).
//!
//! It never opens the database the LevelDB way, which would write to it
//! (a new log, a new manifest, the `LOCK` file): it reads the files as they
//! are, while the desktop app may run.
//!
//! - **Logs** (`*.log`): 32 KiB blocks of records (`FULL`, or `FIRST`,
//!   `MIDDLE`… `LAST` fragments), each with a masked CRC-32C; a record is a
//!   write batch: a sequence number, a count, then puts and deletes.
//! - **Tables** (`*.ldb`, `*.sst`): a footer with the index block's handle;
//!   the index points at the data blocks; a block is raw or Snappy
//!   compressed, with a masked CRC-32C; its entries share key prefixes; a
//!   key is the user key plus 8 bytes of sequence and type.
//!
//! Every entry of every file is read, and for each key the entry with the
//! highest sequence number wins: a later put or a delete. Files a compaction
//! made obsolete but did not delete yet hold older entries, so they lose.
//! A record or block whose checksum fails (a write in progress) is skipped.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

/// Size of a log block.
const LOG_BLOCK: usize = 32 * 1024;
/// Header of a log record: checksum, length, type.
const LOG_HEADER: usize = 7;
/// Size of a table's footer.
const FOOTER: usize = 48;
/// The magic number at the end of a table.
const TABLE_MAGIC: u64 = 0xdb47_7524_8b80_fb57;
/// The type byte of a put in a batch or an internal key.
const TYPE_VALUE: u8 = 1;
/// The type byte of a delete.
const TYPE_DELETION: u8 = 0;

/// The newest state of every key: its value, or `None` once deleted.
#[derive(Debug, Default)]
pub struct Snapshot {
    entries: HashMap<Vec<u8>, (u64, Option<Vec<u8>>)>,
    /// Records and blocks skipped because they did not check out.
    pub skipped: usize,
}

impl Snapshot {
    /// The value of `key`, if it is set.
    #[must_use]
    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.entries.get(key)?.1.as_deref()
    }

    /// Every set key and its value.
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &[u8])> {
        self.entries
            .iter()
            .filter_map(|(k, (_, v))| Some((k.as_slice(), v.as_deref()?)))
    }

    /// The sequence number of `key`'s newest entry.
    #[must_use]
    pub fn sequence(&self, key: &[u8]) -> Option<u64> {
        self.entries.get(key).map(|(seq, _)| *seq)
    }

    fn apply(&mut self, key: &[u8], sequence: u64, value: Option<&[u8]>) {
        match self.entries.get(key) {
            Some((newest, _)) if *newest > sequence => {}
            _ => {
                self.entries
                    .insert(key.to_vec(), (sequence, value.map(<[u8]>::to_vec)));
            }
        }
    }
}

/// Reads every log and table file of the LevelDB directory `dir`.
///
/// # Errors
///
/// The directory cannot be listed. Files that cannot be read are skipped.
pub fn read_dir(dir: &Path) -> io::Result<Snapshot> {
    let mut snapshot = Snapshot::default();
    let mut names: Vec<_> = fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    names.sort();
    for path in names {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let Ok(bytes) = fs::read(&path) else {
            snapshot.skipped += 1;
            continue;
        };
        match ext {
            "log" => read_log(&bytes, &mut snapshot),
            "ldb" | "sst" if read_table(&bytes, &mut snapshot).is_none() => snapshot.skipped += 1,
            _ => {}
        }
    }
    Ok(snapshot)
}

/// Applies the write batches of a log file.
pub fn read_log(bytes: &[u8], snapshot: &mut Snapshot) {
    let mut record: Vec<u8> = Vec::new();
    let mut in_fragments = false;
    let mut block_start = 0;
    while block_start < bytes.len() {
        let block = &bytes[block_start..bytes.len().min(block_start + LOG_BLOCK)];
        let mut at = 0;
        while at + LOG_HEADER <= block.len() {
            let crc = u32::from_le_bytes(block[at..at + 4].try_into().expect("4 bytes"));
            let length = usize::from(u16::from_le_bytes([block[at + 4], block[at + 5]]));
            let kind = block[at + 6];
            if kind == 0 && length == 0 {
                // Preallocated, never written: the rest of the block is empty.
                break;
            }
            let start = at + LOG_HEADER;
            let Some(data) = block.get(start..start + length) else {
                snapshot.skipped += 1;
                break;
            };
            at = start + length;
            if unmask(crc) != crc32c(&[&[kind], data]) {
                snapshot.skipped += 1;
                record.clear();
                in_fragments = false;
                continue;
            }
            match kind {
                1 => {
                    read_batch(data, snapshot);
                    record.clear();
                    in_fragments = false;
                }
                2 => {
                    record.clear();
                    record.extend_from_slice(data);
                    in_fragments = true;
                }
                3 if in_fragments => record.extend_from_slice(data),
                4 if in_fragments => {
                    record.extend_from_slice(data);
                    read_batch(&record, snapshot);
                    record.clear();
                    in_fragments = false;
                }
                _ => {
                    snapshot.skipped += 1;
                    record.clear();
                    in_fragments = false;
                }
            }
        }
        block_start += LOG_BLOCK;
    }
}

/// Applies one write batch.
fn read_batch(batch: &[u8], snapshot: &mut Snapshot) {
    let (Some(head), Some(count)) = (batch.get(..8), batch.get(8..12)) else {
        snapshot.skipped += 1;
        return;
    };
    let sequence = u64::from_le_bytes(head.try_into().expect("8 bytes"));
    let count = u32::from_le_bytes(count.try_into().expect("4 bytes"));
    let mut at = 12;
    for i in 0..u64::from(count) {
        let Some(&kind) = batch.get(at) else {
            snapshot.skipped += 1;
            return;
        };
        at += 1;
        let Some(key) = length_prefixed(batch, &mut at) else {
            snapshot.skipped += 1;
            return;
        };
        match kind {
            TYPE_VALUE => {
                let Some(value) = length_prefixed(batch, &mut at) else {
                    snapshot.skipped += 1;
                    return;
                };
                snapshot.apply(key, sequence + i, Some(value));
            }
            TYPE_DELETION => snapshot.apply(key, sequence + i, None),
            _ => {
                snapshot.skipped += 1;
                return;
            }
        }
    }
}

/// Applies every entry of a table file; `None` when it is not one.
pub fn read_table(bytes: &[u8], snapshot: &mut Snapshot) -> Option<()> {
    let footer = bytes.get(bytes.len().checked_sub(FOOTER)?..)?;
    let magic = u64::from_le_bytes(footer[40..48].try_into().ok()?);
    if magic != TABLE_MAGIC {
        return None;
    }
    let mut at = 0;
    let _metaindex = (varint(footer, &mut at)?, varint(footer, &mut at)?);
    let index = (varint(footer, &mut at)?, varint(footer, &mut at)?);
    let index = block(bytes, index)?;
    for (_, handle) in block_entries(&index)? {
        let mut at = 0;
        let handle = (varint(&handle, &mut at)?, varint(&handle, &mut at)?);
        let Some(data) = block(bytes, handle) else {
            snapshot.skipped += 1;
            continue;
        };
        let Some(entries) = block_entries(&data) else {
            snapshot.skipped += 1;
            continue;
        };
        for (key, value) in entries {
            let Some(split) = key.len().checked_sub(8) else {
                continue;
            };
            let tag = u64::from_le_bytes(key[split..].try_into().ok()?);
            let (sequence, kind) = (tag >> 8, (tag & 0xff) as u8);
            match kind {
                TYPE_VALUE => snapshot.apply(&key[..split], sequence, Some(&value)),
                TYPE_DELETION => snapshot.apply(&key[..split], sequence, None),
                _ => {}
            }
        }
    }
    Some(())
}

/// The contents of the block at `handle` (offset, size), checked and
/// decompressed.
fn block(bytes: &[u8], (offset, size): (u64, u64)) -> Option<Vec<u8>> {
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(usize::try_from(size).ok()?)?;
    let contents = bytes.get(start..end)?;
    let trailer = bytes.get(end..end + 5)?;
    let crc = u32::from_le_bytes(trailer[1..5].try_into().ok()?);
    if unmask(crc) != crc32c(&[contents, &trailer[..1]]) {
        return None;
    }
    match trailer[0] {
        0 => Some(contents.to_vec()),
        1 => snappy_decompress(contents),
        _ => None,
    }
}

/// The key-value entries of a block, its shared key prefixes expanded.
fn block_entries(block: &[u8]) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
    let restarts = u32::from_le_bytes(block.get(block.len().checked_sub(4)?..)?.try_into().ok()?);
    let data_end = block
        .len()
        .checked_sub(4)?
        .checked_sub(usize::try_from(restarts).ok()?.checked_mul(4)?)?;
    let mut out = Vec::new();
    let mut key: Vec<u8> = Vec::new();
    let mut at = 0;
    while at < data_end {
        let shared = usize::try_from(varint(block, &mut at)?).ok()?;
        let unshared = usize::try_from(varint(block, &mut at)?).ok()?;
        let value_len = usize::try_from(varint(block, &mut at)?).ok()?;
        if shared > key.len() {
            return None;
        }
        key.truncate(shared);
        key.extend_from_slice(block.get(at..at.checked_add(unshared)?)?);
        at += unshared;
        let value = block.get(at..at.checked_add(value_len)?)?.to_vec();
        at += value_len;
        out.push((key.clone(), value));
    }
    Some(out)
}

/// A varint-length-prefixed slice at `at`, which moves past it.
fn length_prefixed<'a>(bytes: &'a [u8], at: &mut usize) -> Option<&'a [u8]> {
    let len = usize::try_from(varint(bytes, at)?).ok()?;
    let slice = bytes.get(*at..at.checked_add(len)?)?;
    *at += len;
    Some(slice)
}

/// A LEB128 varint (up to 64 bits) at `at`, which moves past it.
fn varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// Decompresses raw Snappy data; `None` when it is malformed.
#[must_use]
pub fn snappy_decompress(input: &[u8]) -> Option<Vec<u8>> {
    let mut at = 0;
    let expected = usize::try_from(varint(input, &mut at)?).ok()?;
    // A block is at most a few KiB; a lying length must not allocate much.
    let mut out = Vec::with_capacity(expected.min(1 << 20));
    while at < input.len() {
        let tag = input[at];
        at += 1;
        match tag & 0b11 {
            0 => {
                let mut len = usize::from(tag >> 2);
                if len >= 60 {
                    let bytes = len - 59;
                    let raw = input.get(at..at + bytes)?;
                    len = raw
                        .iter()
                        .rev()
                        .fold(0usize, |acc, &b| (acc << 8) | usize::from(b));
                    at += bytes;
                }
                let len = len + 1;
                out.extend_from_slice(input.get(at..at.checked_add(len)?)?);
                at += len;
            }
            kind => {
                let (len, offset) = match kind {
                    1 => {
                        let len = 4 + usize::from((tag >> 2) & 0b111);
                        let offset = (usize::from(tag >> 5) << 8) | usize::from(*input.get(at)?);
                        at += 1;
                        (len, offset)
                    }
                    2 => {
                        let raw = input.get(at..at + 2)?;
                        at += 2;
                        (
                            1 + usize::from(tag >> 2),
                            usize::from(u16::from_le_bytes([raw[0], raw[1]])),
                        )
                    }
                    _ => {
                        let raw = input.get(at..at + 4)?;
                        at += 4;
                        (
                            1 + usize::from(tag >> 2),
                            usize::try_from(u32::from_le_bytes(raw.try_into().ok()?)).ok()?,
                        )
                    }
                };
                if offset == 0 || offset > out.len() || out.len() + len > expected {
                    return None;
                }
                let start = out.len() - offset;
                for i in 0..len {
                    out.push(out[start + i]);
                }
            }
        }
        if out.len() > expected {
            return None;
        }
    }
    (out.len() == expected).then_some(out)
}

/// The CRC-32C (Castagnoli) of the concatenated `parts`.
#[must_use]
pub fn crc32c(parts: &[&[u8]]) -> u32 {
    const fn table() -> [u32; 256] {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut crc = i as u32;
            let mut bit = 0;
            while bit < 8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0x82f6_3b78
                } else {
                    crc >> 1
                };
                bit += 1;
            }
            table[i] = crc;
            i += 1;
        }
        table
    }
    const TABLE: [u32; 256] = table();
    let mut crc = !0u32;
    for part in parts {
        for &byte in *part {
            crc = TABLE[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8);
        }
    }
    !crc
}

/// LevelDB stores checksums masked, so a checksum of data that holds
/// checksums does not degenerate.
#[must_use]
pub fn mask(crc: u32) -> u32 {
    crc.rotate_right(15).wrapping_add(0xa282_ead8)
}

fn unmask(masked: u32) -> u32 {
    masked.wrapping_sub(0xa282_ead8).rotate_left(15)
}

/// Builders of LevelDB files, for tests: a log and a table with the formats
/// above.
#[doc(hidden)]
pub mod fixture {
    use super::{LOG_BLOCK, LOG_HEADER, TABLE_MAGIC, TYPE_DELETION, TYPE_VALUE, crc32c, mask};

    /// One operation of a write batch: a put, or a delete when `None`.
    pub type Op<'a> = (&'a [u8], Option<&'a [u8]>);
    /// One entry of a table: user key, sequence, and a value or a delete.
    pub type Entry<'a> = (&'a [u8], u64, Option<&'a [u8]>);

    fn put_varint(out: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            out.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }

    /// A write batch starting at `sequence`.
    #[must_use]
    pub fn batch(sequence: u64, ops: &[Op<'_>]) -> Vec<u8> {
        let mut out = sequence.to_le_bytes().to_vec();
        out.extend_from_slice(&u32::try_from(ops.len()).unwrap_or(0).to_le_bytes());
        for (key, value) in ops {
            out.push(if value.is_some() {
                TYPE_VALUE
            } else {
                TYPE_DELETION
            });
            put_varint(&mut out, key.len() as u64);
            out.extend_from_slice(key);
            if let Some(value) = value {
                put_varint(&mut out, value.len() as u64);
                out.extend_from_slice(value);
            }
        }
        out
    }

    /// A log file of these records, fragmented across blocks as LevelDB
    /// writes them.
    #[must_use]
    pub fn log(records: &[Vec<u8>]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        for record in records {
            let mut rest = record.as_slice();
            let mut first = true;
            loop {
                let left = LOG_BLOCK - out.len() % LOG_BLOCK;
                if left < LOG_HEADER {
                    out.extend(std::iter::repeat_n(0u8, left));
                    continue;
                }
                let room = left - LOG_HEADER;
                let take = rest.len().min(room);
                let last = take == rest.len();
                let kind: u8 = match (first, last) {
                    (true, true) => 1,
                    (true, false) => 2,
                    (false, false) => 3,
                    (false, true) => 4,
                };
                let data = &rest[..take];
                out.extend_from_slice(&mask(crc32c(&[&[kind], data])).to_le_bytes());
                out.extend_from_slice(&u16::try_from(take).unwrap_or(0).to_le_bytes());
                out.push(kind);
                out.extend_from_slice(data);
                rest = &rest[take..];
                first = false;
                if last {
                    break;
                }
            }
        }
        out
    }

    /// The bytes of a block of `entries` (sorted keys), sharing prefixes,
    /// one restart point.
    fn block_bytes(entries: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut previous: &[u8] = &[];
        for (key, value) in entries {
            let shared = previous
                .iter()
                .zip(key.iter())
                .take_while(|(a, b)| a == b)
                .count();
            put_varint(&mut out, shared as u64);
            put_varint(&mut out, (key.len() - shared) as u64);
            put_varint(&mut out, value.len() as u64);
            out.extend_from_slice(&key[shared..]);
            out.extend_from_slice(value);
            previous = key;
        }
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out
    }

    /// Snappy-compresses `data` with literals and 1-byte-offset copies:
    /// valid input for any Snappy decoder.
    #[must_use]
    pub fn snappy_compress(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, data.len() as u64);
        let mut literal_start = 0;
        let mut at = 0;
        let flush = |out: &mut Vec<u8>, literal: &[u8]| {
            for chunk in literal.chunks(60) {
                out.push(((chunk.len() - 1) as u8) << 2);
                out.extend_from_slice(chunk);
            }
        };
        while at < data.len() {
            // A match of 4–11 bytes within the last 2047 bytes.
            let best = (1..=at.min(2047))
                .filter_map(|offset| {
                    let len = (0..11)
                        .take_while(|&i| {
                            at + i < data.len() && data[at + i] == data[at + i - offset]
                        })
                        .count();
                    (len >= 4).then_some((len, offset))
                })
                .max_by_key(|&(len, _)| len);
            match best {
                Some((len, offset)) => {
                    flush(&mut out, &data[literal_start..at]);
                    out.push(0b01 | (((len - 4) as u8) << 2) | (((offset >> 8) as u8) << 5));
                    out.push((offset & 0xff) as u8);
                    at += len;
                    literal_start = at;
                }
                None => at += 1,
            }
        }
        flush(&mut out, &data[literal_start..]);
        out
    }

    /// A table file of `entries` (user key, sequence, value or delete),
    /// sorted by key, split in two data blocks: the first raw, the second
    /// Snappy compressed when `snappy`.
    #[must_use]
    pub fn table(entries: &[Entry<'_>], snappy: bool) -> Vec<u8> {
        let mut sorted: Vec<(Vec<u8>, Vec<u8>)> = entries
            .iter()
            .map(|(key, sequence, value)| {
                let mut internal = key.to_vec();
                let kind = if value.is_some() {
                    TYPE_VALUE
                } else {
                    TYPE_DELETION
                };
                internal.extend_from_slice(&((sequence << 8) | u64::from(kind)).to_le_bytes());
                (internal, value.map(<[u8]>::to_vec).unwrap_or_default())
            })
            .collect();
        sorted.sort();
        let half = sorted.len().div_ceil(2);
        let mut out = Vec::new();
        let mut index = Vec::new();
        for (n, part) in [&sorted[..half], &sorted[half..]].into_iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            let raw = block_bytes(part);
            let (contents, kind) = if snappy && n == 1 {
                (snappy_compress(&raw), 1u8)
            } else {
                (raw, 0u8)
            };
            let mut handle = Vec::new();
            put_varint(&mut handle, out.len() as u64);
            put_varint(&mut handle, contents.len() as u64);
            out.extend_from_slice(&contents);
            out.push(kind);
            out.extend_from_slice(&mask(crc32c(&[&contents, &[kind]])).to_le_bytes());
            let last_key = part.last().map(|(k, _)| k.clone()).unwrap_or_default();
            index.push((last_key, handle));
        }
        let index_block = block_bytes(&index);
        let mut index_handle = Vec::new();
        put_varint(&mut index_handle, out.len() as u64);
        put_varint(&mut index_handle, index_block.len() as u64);
        out.extend_from_slice(&index_block);
        out.push(0);
        out.extend_from_slice(&mask(crc32c(&[&index_block, &[0]])).to_le_bytes());
        // Footer: an empty metaindex handle, the index handle, padding, magic.
        let mut footer = Vec::new();
        put_varint(&mut footer, 0);
        put_varint(&mut footer, 0);
        footer.extend_from_slice(&index_handle);
        footer.resize(40, 0);
        footer.extend_from_slice(&TABLE_MAGIC.to_le_bytes());
        out.extend_from_slice(&footer);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{batch, log, snappy_compress, table};
    use super::*;

    #[test]
    fn crc32c_matches_the_standard_vectors() {
        assert_eq!(crc32c(&[b"123456789"]), 0xe306_9283);
        assert_eq!(crc32c(&[&[0u8; 32]]), 0x8a91_36aa);
        assert_eq!(crc32c(&[b"1234", b"56789"]), crc32c(&[b"123456789"]));
        assert_eq!(unmask(mask(0x1234_5678)), 0x1234_5678);
    }

    #[test]
    fn snappy_round_trips_and_refuses_garbage() {
        let data = b"abcabcabcabcabc hello hello hello hello world".repeat(20);
        let packed = snappy_compress(&data);
        assert!(packed.len() < data.len());
        assert_eq!(snappy_decompress(&packed).unwrap(), data);
        // A 2-byte-offset copy and a long literal, written by hand.
        let mut hand = vec![70];
        hand.push(60 << 2);
        hand.push(60);
        hand.extend(std::iter::repeat_n(b'x', 61));
        hand.push(0b10 | (8 << 2));
        hand.extend_from_slice(&3u16.to_le_bytes());
        let out = snappy_decompress(&hand).unwrap();
        assert_eq!(out.len(), 70);
        assert!(out.iter().all(|&b| b == b'x'));
        for bad in [
            &[5u8, 0b01, 0][..],
            &[3, 4 << 2, b'a'][..],
            &[200, 1, 0][..],
            &[][..],
        ] {
            assert_eq!(snappy_decompress(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_newest_entry_of_logs_and_tables_wins() {
        let dir = tempfile::tempdir().unwrap();
        // A table from a compaction, then a log with later changes; a big
        // value is fragmented across log blocks.
        let big = vec![b'v'; 50_000];
        fs::write(
            dir.path().join("000003.ldb"),
            table(
                &[
                    (b"a", 1, Some(b"old a")),
                    (b"b", 2, Some(b"b")),
                    (b"c", 3, Some(b"c")),
                    (b"d", 4, Some(b"gone later")),
                ],
                true,
            ),
        )
        .unwrap();
        let mut logged = log(&[
            batch(10, &[(b"a", Some(b"new a")), (b"d", None)]),
            batch(12, &[(b"big", Some(&big))]),
        ]);
        // A record torn by a crash at the end is skipped.
        logged.extend_from_slice(&[1, 2, 3, 4, 50, 0, 1, b'x']);
        fs::write(dir.path().join("000004.log"), logged).unwrap();
        // An obsolete log with an older value loses.
        fs::write(
            dir.path().join("000001.log"),
            log(&[batch(1, &[(b"a", Some(b"oldest a"))])]),
        )
        .unwrap();
        fs::write(dir.path().join("LOCK"), b"").unwrap();
        fs::write(dir.path().join("CURRENT"), b"MANIFEST-000001\n").unwrap();

        let snapshot = read_dir(dir.path()).unwrap();
        assert_eq!(snapshot.get(b"a"), Some(&b"new a"[..]));
        assert_eq!(snapshot.get(b"b"), Some(&b"b"[..]));
        assert_eq!(snapshot.get(b"c"), Some(&b"c"[..]));
        assert_eq!(snapshot.get(b"d"), None, "deleted");
        assert_eq!(snapshot.get(b"big").map(<[u8]>::len), Some(50_000));
        assert_eq!(snapshot.sequence(b"a"), Some(10));
        assert_eq!(snapshot.iter().count(), 4);
        assert!(snapshot.skipped >= 1, "the torn record");
    }

    #[test]
    fn a_corrupt_block_is_skipped() {
        let mut bytes = table(&[(b"k", 1, Some(b"value"))], false);
        bytes[3] ^= 0xff;
        let mut snapshot = Snapshot::default();
        read_table(&bytes, &mut snapshot).unwrap();
        assert_eq!(snapshot.get(b"k"), None);
        assert_eq!(snapshot.skipped, 1);
        assert!(read_table(b"not a table", &mut snapshot).is_none());
    }
}
