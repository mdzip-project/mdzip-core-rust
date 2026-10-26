//! Test helpers: a minimal ZIP writer that stores names and attributes
//! exactly as given, so tests can build archives no conforming producer
//! would write (invalid names, symbolic links, no manifest).

#![allow(dead_code)]

use std::path::PathBuf;

pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

pub fn load(name: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(fixtures().join(name)).unwrap();
    serde_json::from_str(&text).unwrap()
}

pub struct RawEntry {
    pub name: Vec<u8>,
    pub data: Vec<u8>,
    /// Unix mode, written in the high 16 bits of the external attributes.
    pub mode: u32,
}

impl RawEntry {
    pub fn file(name: &str, data: impl AsRef<[u8]>) -> Self {
        let mode = if name.ends_with('/') { 0o040755 } else { 0o100644 };
        RawEntry {
            name: name.as_bytes().to_vec(),
            data: data.as_ref().to_vec(),
            mode,
        }
    }

    pub fn symlink(name: &str, target: &str) -> Self {
        RawEntry {
            name: name.as_bytes().to_vec(),
            data: target.as_bytes().to_vec(),
            mode: 0o120777,
        }
    }
}

/// Stored (uncompressed) entries, in order, with the UTF-8 flag on
/// non-ASCII names.
pub fn raw_zip(entries: &[RawEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for e in entries {
        let offset = out.len() as u32;
        let crc = crc32(&e.data);
        let flags: u16 = if e.name.is_ascii() { 0 } else { 1 << 11 };
        let header = |sig: u32, central_record: bool, buf: &mut Vec<u8>| {
            buf.extend_from_slice(&sig.to_le_bytes());
            if central_record {
                buf.extend_from_slice(&((3u16 << 8) | 20).to_le_bytes());
            }
            buf.extend_from_slice(&20u16.to_le_bytes());
            buf.extend_from_slice(&flags.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes()); // stored
            buf.extend_from_slice(&0u16.to_le_bytes()); // time
            buf.extend_from_slice(&0x5b21u16.to_le_bytes()); // date
            buf.extend_from_slice(&crc.to_le_bytes());
            buf.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            buf.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            buf.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes()); // extra
            if central_record {
                buf.extend_from_slice(&0u16.to_le_bytes()); // comment
                buf.extend_from_slice(&0u16.to_le_bytes()); // disk
                buf.extend_from_slice(&0u16.to_le_bytes()); // internal
                buf.extend_from_slice(&(e.mode << 16).to_le_bytes());
                buf.extend_from_slice(&offset.to_le_bytes());
            }
            buf.extend_from_slice(&e.name);
        };
        header(0x0403_4b50, false, &mut out);
        out.extend_from_slice(&e.data);
        header(0x0201_4b50, true, &mut central);
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

/// Shorthand: files given as (name, content) pairs.
pub fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
    raw_zip(&files.iter().map(|(n, d)| RawEntry::file(n, d)).collect::<Vec<_>>())
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Central directory records of an archive: (name, flags, method, external attributes).
pub fn central_records(data: &[u8]) -> Vec<(String, u16, u16, u32)> {
    let eocd = (0..=data.len() - 22)
        .rev()
        .find(|&i| data[i..i + 4] == 0x0605_4b50u32.to_le_bytes())
        .unwrap();
    let u16_at = |i: usize| u16::from_le_bytes([data[i], data[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
    let count = u16_at(eocd + 10) as usize;
    let mut p = u32_at(eocd + 16) as usize;
    let mut out = Vec::new();
    for _ in 0..count {
        let flags = u16_at(p + 8);
        let method = u16_at(p + 10);
        let n = u16_at(p + 28) as usize;
        let x = u16_at(p + 30) as usize;
        let c = u16_at(p + 32) as usize;
        let attrs = u32_at(p + 38);
        let name = String::from_utf8(data[p + 46..p + 46 + n].to_vec()).unwrap();
        out.push((name, flags, method, attrs));
        p += 46 + n + x + c;
    }
    out
}
