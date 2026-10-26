//! A small ZIP reader and writer covering what MDZip needs (Section 4):
//! stored and DEFLATE entries, UTF-8 names, and ZIP64 only when a value
//! doesn't fit. Every check that protects against hostile input lives here
//! or in `archive.rs`, so nothing depends on another library's policy.

use std::borrow::Cow;

use crate::error::{ErrorCode, MdzError, Result, err};

pub(crate) const METHOD_STORED: u16 = 0;
pub(crate) const METHOD_DEFLATE: u16 = 8;
pub(crate) const FLAG_ENCRYPTED: u16 = 1;
pub(crate) const FLAG_DATA_DESCRIPTOR: u16 = 1 << 3;
pub(crate) const FLAG_STRONG_ENCRYPTION: u16 = 1 << 6;
pub(crate) const FLAG_UTF8: u16 = 1 << 11;

const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_ZIP64_EOCD: u32 = 0x0606_4b50;
const SIG_ZIP64_LOCATOR: u32 = 0x0706_4b50;
const ZIP64_EXTRA: u16 = 0x0001;
const U16_MAX: u64 = 0xFFFF;
const U32_MAX: u64 = 0xFFFF_FFFF;

/// Unix file type bits in the high 16 bits of the external attributes.
const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;
const S_IFREG: u32 = 0o100000;
#[cfg(test)]
const S_IFDIR: u32 = 0o040000;

/// An entry as described by the central directory (which wins over the
/// local header, Section 4).
#[derive(Clone, Debug)]
pub(crate) struct ZipEntry {
    pub name: String,
    pub version_made_by: u16,
    pub flags: u16,
    pub method: u16,
    pub dos_time: u16,
    pub dos_date: u16,
    pub crc32: u32,
    pub compressed_size: u64,
    pub size: u64,
    pub external_attributes: u32,
    pub local_offset: u64,
    /// Start of the entry's compressed data, set by [`locate_data`].
    pub data_start: usize,
}

impl ZipEntry {
    pub fn is_symlink(&self) -> bool {
        (self.external_attributes >> 16) & S_IFMT == S_IFLNK
    }

    pub fn data_end(&self) -> usize {
        self.data_start + self.compressed_size as usize
    }

    pub fn raw_data<'a>(&self, archive: &'a [u8]) -> &'a [u8] {
        &archive[self.data_start..self.data_end()]
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn at(data: &'a [u8], pos: usize) -> Self {
        Cursor { data, pos }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.data.len())
            .ok_or_else(|| MdzError::new(ErrorCode::ZipInvalid, "ZIP structure runs past the end of the file"))?;
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

fn to_usize(value: u64) -> Result<usize> {
    usize::try_from(value).map_err(|_| MdzError::new(ErrorCode::ZipInvalid, "ZIP offset out of range"))
}

struct Directory {
    entries: u64,
    size: u64,
    offset: u64,
    /// Where the central directory must end (the ZIP64 record or the EOCD).
    end: u64,
}

fn find_directory(data: &[u8]) -> Result<Directory> {
    if data.len() < 22 {
        return err(ErrorCode::ZipInvalid, "not a ZIP archive (too short)");
    }
    let lowest = data.len().saturating_sub(22 + 0xFFFF);
    let mut pos = data.len() - 22;
    let eocd = loop {
        if data[pos..pos + 4] == SIG_EOCD.to_le_bytes() {
            let comment = u16::from_le_bytes([data[pos + 20], data[pos + 21]]) as usize;
            if pos + 22 + comment <= data.len() {
                break pos;
            }
        }
        if pos == lowest {
            return err(
                ErrorCode::ZipInvalid,
                "not a ZIP archive (no end of central directory record)",
            );
        }
        pos -= 1;
    };
    let mut c = Cursor::at(data, eocd + 4);
    let disk = c.u16()?;
    let cd_disk = c.u16()?;
    let entries_disk = c.u16()? as u64;
    let entries = c.u16()? as u64;
    let size = c.u32()? as u64;
    let offset = c.u32()? as u64;
    if disk != 0 || cd_disk != 0 || entries_disk != entries {
        return err(ErrorCode::ZipInvalid, "multi-volume ZIP archives aren't supported");
    }
    if entries != U16_MAX && size != U32_MAX && offset != U32_MAX {
        return Ok(Directory {
            entries,
            size,
            offset,
            end: eocd as u64,
        });
    }
    // ZIP64 (Section 4: only when a value doesn't fit; readers accept it).
    if eocd < 20 {
        return err(ErrorCode::ZipInvalid, "ZIP64 locator missing");
    }
    let mut c = Cursor::at(data, eocd - 20);
    if c.u32()? != SIG_ZIP64_LOCATOR {
        return err(ErrorCode::ZipInvalid, "ZIP64 locator missing");
    }
    let _disk = c.u32()?;
    let record = c.u64()?;
    let record_pos = to_usize(record)?;
    let mut c = Cursor::at(data, record_pos);
    if c.u32()? != SIG_ZIP64_EOCD {
        return err(ErrorCode::ZipInvalid, "ZIP64 end of central directory record missing");
    }
    let _size = c.u64()?;
    let _made = c.u16()?;
    let _needed = c.u16()?;
    let disk = c.u32()?;
    let cd_disk = c.u32()?;
    let entries_disk = c.u64()?;
    let entries = c.u64()?;
    let size = c.u64()?;
    let offset = c.u64()?;
    if disk != 0 || cd_disk != 0 || entries_disk != entries {
        return err(ErrorCode::ZipInvalid, "multi-volume ZIP archives aren't supported");
    }
    Ok(Directory {
        entries,
        size,
        offset,
        end: record,
    })
}

/// Read the central directory. Fails with [`ErrorCode::LimitExceeded`] before
/// parsing if the archive declares more than `max_entries` entries.
pub(crate) fn read_directory(data: &[u8], max_entries: u64) -> Result<Vec<ZipEntry>> {
    let dir = find_directory(data)?;
    if dir.entries > max_entries {
        return err(
            ErrorCode::LimitExceeded,
            format!("archive has {} entries (limit {max_entries})", dir.entries),
        );
    }
    if dir.offset.checked_add(dir.size).is_none_or(|end| end > dir.end) || dir.entries.saturating_mul(46) > dir.size {
        return err(ErrorCode::ZipInvalid, "central directory is out of bounds");
    }
    let mut c = Cursor::at(&data[..to_usize(dir.offset + dir.size)?], to_usize(dir.offset)?);
    let mut entries = Vec::with_capacity(dir.entries as usize);
    for _ in 0..dir.entries {
        if c.u32()? != SIG_CENTRAL {
            return err(ErrorCode::ZipInvalid, "bad central directory entry");
        }
        let version_made_by = c.u16()?;
        let _needed = c.u16()?;
        let flags = c.u16()?;
        let method = c.u16()?;
        let dos_time = c.u16()?;
        let dos_date = c.u16()?;
        let crc32 = c.u32()?;
        let mut compressed_size = c.u32()? as u64;
        let mut size = c.u32()? as u64;
        let name_len = c.u16()? as usize;
        let extra_len = c.u16()? as usize;
        let comment_len = c.u16()? as usize;
        let disk = c.u16()? as u64;
        let _internal = c.u16()?;
        let external_attributes = c.u32()?;
        let mut local_offset = c.u32()? as u64;
        let raw_name = c.take(name_len)?;
        let extra = c.take(extra_len)?;
        c.take(comment_len)?;

        // ZIP64 extended information: only the fields whose 32-bit value is saturated.
        if size == U32_MAX || compressed_size == U32_MAX || local_offset == U32_MAX || disk == U16_MAX {
            let mut x = Cursor::at(extra, 0);
            let mut found = false;
            while x.pos + 4 <= extra.len() {
                let id = x.u16()?;
                let len = x.u16()? as usize;
                let body = x.take(len)?;
                if id != ZIP64_EXTRA {
                    continue;
                }
                found = true;
                let mut f = Cursor::at(body, 0);
                if size == U32_MAX {
                    size = f.u64()?;
                }
                if compressed_size == U32_MAX {
                    compressed_size = f.u64()?;
                }
                if local_offset == U32_MAX {
                    local_offset = f.u64()?;
                }
                break;
            }
            if !found {
                return err(ErrorCode::ZipInvalid, "ZIP64 sizes missing from an entry");
            }
        }
        entries.push(ZipEntry {
            name: decode_name(raw_name, flags)?,
            version_made_by,
            flags,
            method,
            dos_time,
            dos_date,
            crc32,
            compressed_size,
            size,
            external_attributes,
            local_offset,
            data_start: 0,
        });
    }
    Ok(entries)
}

/// Find each entry's data from its local header, and reject entries whose
/// data runs into the central directory or overlaps another entry's (the
/// shape of an overlapping ZIP bomb, Section 13.1).
pub(crate) fn locate_data(data: &[u8], entries: &mut [ZipEntry]) -> Result<()> {
    let dir_start = find_directory(data)?.offset;
    for e in entries.iter_mut() {
        let start = to_usize(e.local_offset)?;
        let mut c = Cursor::at(data, start);
        if c.u32()? != SIG_LOCAL {
            return err(ErrorCode::ZipInvalid, format!("'{}' has no local file header", e.name));
        }
        c.take(22)?;
        let name_len = c.u16()? as usize;
        let extra_len = c.u16()? as usize;
        c.take(name_len + extra_len)?;
        let end = (c.pos as u64).checked_add(e.compressed_size);
        if end.is_none_or(|end| end > dir_start) {
            return err(
                ErrorCode::ZipInvalid,
                format!("'{}' data runs past the entries area", e.name),
            );
        }
        e.data_start = c.pos;
    }
    let mut ranges: Vec<(u64, u64, &str)> = entries
        .iter()
        .map(|e| (e.local_offset, (e.data_end()) as u64, e.name.as_str()))
        .collect();
    ranges.sort_unstable_by_key(|r| r.0);
    for pair in ranges.windows(2) {
        if pair[1].0 < pair[0].1 {
            return err(
                ErrorCode::ZipInvalid,
                format!("entries '{}' and '{}' overlap (Section 13.1)", pair[0].2, pair[1].2),
            );
        }
    }
    Ok(())
}

/// An entry name as UTF-8 (Section 4). Names without the UTF-8 flag are tried
/// as UTF-8 first, since many tools write UTF-8 without setting it, then
/// decoded as CP437.
fn decode_name(raw: &[u8], flags: u16) -> Result<String> {
    match std::str::from_utf8(raw) {
        Ok(s) => Ok(s.to_owned()),
        Err(_) if flags & FLAG_UTF8 != 0 => err(ErrorCode::ZipInvalid, "entry name is flagged UTF-8 but isn't"),
        Err(_) => Ok(raw
            .iter()
            .map(|&b| {
                if b < 0x80 {
                    b as char
                } else {
                    CP437_HIGH[(b - 0x80) as usize]
                }
            })
            .collect()),
    }
}

/// Decompress an entry, never producing more than its declared size or
/// `max_size`, and verify its CRC-32.
pub(crate) fn read_entry(archive: &[u8], e: &ZipEntry, max_size: u64) -> Result<Vec<u8>> {
    let raw = e.raw_data(archive);
    let declared = e.size;
    let cap = declared.min(max_size);
    let out = match e.method {
        METHOD_STORED => {
            if raw.len() as u64 > cap {
                return err(
                    ErrorCode::LimitExceeded,
                    format!("'{}' holds more data than its declared size", e.name),
                );
            }
            raw.to_vec()
        }
        METHOD_DEFLATE => {
            let limit = to_usize(cap)?.saturating_add(1);
            match miniz_oxide::inflate::decompress_to_vec_with_limit(raw, limit) {
                Ok(v) if v.len() as u64 > cap => {
                    return err(
                        ErrorCode::LimitExceeded,
                        format!("'{}' decompresses to more than its declared size", e.name),
                    );
                }
                Ok(v) => v,
                Err(failure) if failure.status == miniz_oxide::inflate::TINFLStatus::HasMoreOutput => {
                    return err(
                        ErrorCode::LimitExceeded,
                        format!("'{}' decompresses to more than its declared size", e.name),
                    );
                }
                Err(failure) => {
                    return err(ErrorCode::ZipInvalid, format!("'{}' is corrupt: {failure}", e.name));
                }
            }
        }
        other => {
            return err(
                ErrorCode::ZipInvalid,
                format!(
                    "'{}' uses compression method {other}; MDZip allows 0 (stored) and 8 (DEFLATE)",
                    e.name
                ),
            );
        }
    };
    if (out.len() as u64) < declared {
        return err(
            ErrorCode::ZipInvalid,
            format!("'{}' is shorter than its declared size", e.name),
        );
    }
    if crc32fast::hash(&out) != e.crc32 {
        return err(
            ErrorCode::ZipInvalid,
            format!("'{}' is corrupt (CRC-32 mismatch)", e.name),
        );
    }
    Ok(out)
}

/// An entry to write. `data` is the stored (possibly compressed) bytes.
pub(crate) struct WriteEntry<'a> {
    pub name: String,
    pub version_made_by: u16,
    pub flags: u16,
    pub method: u16,
    pub dos_time: u16,
    pub dos_date: u16,
    pub crc32: u32,
    pub size: u64,
    pub data: Cow<'a, [u8]>,
    pub external_attributes: u32,
}

/// Made by Unix (3), ZIP 2.0, so the external attributes carry Unix modes.
const MADE_BY_UNIX: u16 = (3 << 8) | 20;

impl<'a> WriteEntry<'a> {
    /// A new regular file, DEFLATE-compressed when that makes it smaller.
    pub fn file(name: String, content: &[u8], compress: bool, (dos_date, dos_time): (u16, u16)) -> Self {
        let deflated = compress.then(|| miniz_oxide::deflate::compress_to_vec(content, 6));
        let (method, data) = match deflated {
            Some(d) if d.len() < content.len() => (METHOD_DEFLATE, d),
            _ => (METHOD_STORED, content.to_vec()),
        };
        WriteEntry {
            name,
            version_made_by: MADE_BY_UNIX,
            flags: FLAG_UTF8,
            method,
            dos_time,
            dos_date,
            crc32: crc32fast::hash(content),
            size: content.len() as u64,
            data: Cow::Owned(data),
            external_attributes: (S_IFREG | 0o644) << 16,
        }
    }

    /// A new directory entry (name ends in `/`).
    #[cfg(test)]
    pub fn directory(name: String, (dos_date, dos_time): (u16, u16)) -> Self {
        WriteEntry {
            name,
            version_made_by: MADE_BY_UNIX,
            flags: FLAG_UTF8,
            method: METHOD_STORED,
            dos_time,
            dos_date,
            crc32: 0,
            size: 0,
            data: Cow::Borrowed(&[]),
            external_attributes: ((S_IFDIR | 0o755) << 16) | 0x10,
        }
    }

    /// An existing entry copied as stored: same compressed bytes, method,
    /// timestamp and attributes. The name is written as UTF-8.
    pub fn copy(name: String, e: &ZipEntry, archive: &'a [u8]) -> Self {
        WriteEntry {
            name,
            version_made_by: e.version_made_by,
            flags: (e.flags & !FLAG_DATA_DESCRIPTOR) | FLAG_UTF8,
            method: e.method,
            dos_time: e.dos_time,
            dos_date: e.dos_date,
            crc32: e.crc32,
            size: e.size,
            data: Cow::Borrowed(e.raw_data(archive)),
            external_attributes: e.external_attributes,
        }
    }
}

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn sat32(v: u64) -> u32 {
    v.min(U32_MAX) as u32
}

/// Write entries as a ZIP archive, in order. ZIP64 fields are written only
/// for values that don't fit in the classic format (Section 4).
pub(crate) fn write_zip(entries: &[WriteEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offsets = Vec::with_capacity(entries.len());
    for e in entries {
        let offset = out.len() as u64;
        offsets.push(offset);
        let csize = e.data.len() as u64;
        let big = e.size >= U32_MAX || csize >= U32_MAX;
        let name = e.name.as_bytes();
        put32(&mut out, SIG_LOCAL);
        put16(&mut out, if big { 45 } else { 20 });
        put16(&mut out, e.flags);
        put16(&mut out, e.method);
        put16(&mut out, e.dos_time);
        put16(&mut out, e.dos_date);
        put32(&mut out, e.crc32);
        put32(&mut out, if big { U32_MAX as u32 } else { csize as u32 });
        put32(&mut out, if big { U32_MAX as u32 } else { e.size as u32 });
        put16(&mut out, name.len() as u16);
        put16(&mut out, if big { 20 } else { 0 });
        out.extend_from_slice(name);
        if big {
            put16(&mut out, ZIP64_EXTRA);
            put16(&mut out, 16);
            put64(&mut out, e.size);
            put64(&mut out, csize);
        }
        out.extend_from_slice(&e.data);
    }

    let cd_offset = out.len() as u64;
    for (e, &offset) in entries.iter().zip(&offsets) {
        let csize = e.data.len() as u64;
        let mut extra = Vec::new();
        if e.size >= U32_MAX {
            put64(&mut extra, e.size);
        }
        if csize >= U32_MAX {
            put64(&mut extra, csize);
        }
        if offset >= U32_MAX {
            put64(&mut extra, offset);
        }
        let name = e.name.as_bytes();
        put32(&mut out, SIG_CENTRAL);
        put16(&mut out, e.version_made_by);
        put16(&mut out, if extra.is_empty() { 20 } else { 45 });
        put16(&mut out, e.flags);
        put16(&mut out, e.method);
        put16(&mut out, e.dos_time);
        put16(&mut out, e.dos_date);
        put32(&mut out, e.crc32);
        put32(&mut out, sat32(csize));
        put32(&mut out, sat32(e.size));
        put16(&mut out, name.len() as u16);
        put16(&mut out, if extra.is_empty() { 0 } else { extra.len() as u16 + 4 });
        put16(&mut out, 0); // comment
        put16(&mut out, 0); // disk
        put16(&mut out, 0); // internal attributes
        put32(&mut out, e.external_attributes);
        put32(&mut out, sat32(offset));
        out.extend_from_slice(name);
        if !extra.is_empty() {
            put16(&mut out, ZIP64_EXTRA);
            put16(&mut out, extra.len() as u16);
            out.extend_from_slice(&extra);
        }
    }
    let cd_size = out.len() as u64 - cd_offset;
    let count = entries.len() as u64;

    if count >= U16_MAX || cd_size >= U32_MAX || cd_offset >= U32_MAX {
        let record = out.len() as u64;
        put32(&mut out, SIG_ZIP64_EOCD);
        put64(&mut out, 44);
        put16(&mut out, MADE_BY_UNIX);
        put16(&mut out, 45);
        put32(&mut out, 0);
        put32(&mut out, 0);
        put64(&mut out, count);
        put64(&mut out, count);
        put64(&mut out, cd_size);
        put64(&mut out, cd_offset);
        put32(&mut out, SIG_ZIP64_LOCATOR);
        put32(&mut out, 0);
        put64(&mut out, record);
        put32(&mut out, 1);
    }
    put32(&mut out, SIG_EOCD);
    put16(&mut out, 0);
    put16(&mut out, 0);
    put16(&mut out, count.min(U16_MAX) as u16);
    put16(&mut out, count.min(U16_MAX) as u16);
    put32(&mut out, sat32(cd_size));
    put32(&mut out, sat32(cd_offset));
    put16(&mut out, 0);
    out
}

/// True if `head` starts like a ZIP archive.
pub(crate) fn looks_like_zip(head: &[u8]) -> bool {
    head.starts_with(&SIG_LOCAL.to_le_bytes()) || head.starts_with(&SIG_EOCD.to_le_bytes())
}

/// Code page 437, bytes 0x80–0xFF.
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', //
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', //
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', //
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', //
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', //
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀', //
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', //
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

/// Strong encryption or classic encryption.
pub(crate) fn is_encrypted(flags: u16) -> bool {
    flags & (FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let when = (0x5b21, 0x6000);
        let entries = vec![
            WriteEntry::file("a.md".into(), b"# A\n".repeat(50).as_slice(), true, when),
            WriteEntry::file("caf\u{e9}.png".into(), b"\x89PNG", true, when),
            WriteEntry::directory("empty/".into(), when),
        ];
        let data = write_zip(&entries);
        let mut read = read_directory(&data, 10).unwrap();
        locate_data(&data, &mut read).unwrap();
        assert_eq!(read.len(), 3);
        assert_eq!(read[0].method, METHOD_DEFLATE);
        assert_eq!(read[1].method, METHOD_STORED, "tiny files aren't worth compressing");
        assert_eq!(read_entry(&data, &read[0], 1 << 20).unwrap(), b"# A\n".repeat(50));
        assert_eq!(read[1].name, "caf\u{e9}.png");
        assert!(read[1].flags & FLAG_UTF8 != 0);
    }

    #[test]
    fn cp437_names() {
        assert_eq!(decode_name(&[0x63, 0x61, 0x66, 0x82], 0).unwrap(), "café");
    }
}
