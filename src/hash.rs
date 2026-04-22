//! Streaming XXH3-128 хеширование.

use std::io::{self, Read};
use std::path::Path;

use crate::io_flags::{self, IoBuf, BLOCK_SIZE};

use xxhash_rust::xxh3::Xxh3;

pub struct Hasher(Xxh3);

impl Hasher {
    pub fn new() -> Self {
        Self(Xxh3::new())
    }

    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    pub fn finish(self) -> Hash {
        Hash(self.0.digest128())
    }
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hash(u128);

impl Hash {
    pub fn to_hex(self) -> String {
        format!("{:032x}", self.0)
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 32 {
            return None;
        }
        u128::from_str_radix(s, 16).ok().map(Self)
    }
}

impl std::fmt::Display for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

/// Хеширует файл через cold-read handle, учитывая ровно логический размер файла.
///
/// На Windows с `FILE_FLAG_NO_BUFFERING` последний `ReadFile` может вернуть данные,
/// дополненные до границы сектора. Поэтому хешируем не все возвращённые байты, а
/// только оставшийся `metadata.len()`.
pub fn cold_hash_file(path: &Path) -> io::Result<Hash> {
    let size = std::fs::metadata(path)?.len();
    let mut file = io_flags::open_cold_read(path)?;
    hash_exact_len(&mut file, size)
}

fn hash_exact_len(reader: &mut impl Read, logical_len: u64) -> io::Result<Hash> {
    let mut hasher = Hasher::new();
    let mut buf = IoBuf::new(BLOCK_SIZE);
    let mut remaining = logical_len;

    while remaining > 0 {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{remaining} bytes missing from cold-read"),
            ));
        }

        let take = remaining.min(n as u64) as usize;
        hasher.update(&buf[..take]);
        remaining -= take as u64;
    }

    Ok(hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn hex_roundtrip() {
        let mut h = Hasher::new();
        h.update(b"hello world");
        let hash = h.finish();
        let hex = hash.to_hex();
        assert_eq!(hex.len(), 32);
        let parsed = Hash::from_hex(&hex).unwrap();
        assert_eq!(hash, parsed);
    }

    #[test]
    fn same_input_same_hash() {
        let mut a = Hasher::new();
        let mut b = Hasher::new();
        a.update(b"quick brown fox");
        b.update(b"quick brown fox");
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn from_hex_rejects_bad_length() {
        assert!(Hash::from_hex("abcd").is_none());
    }

    #[test]
    fn exact_len_ignores_bytes_after_logical_tail() {
        let mut reader = Cursor::new(b"abcdef");
        let actual = hash_exact_len(&mut reader, 3).unwrap();

        let mut expected = Hasher::new();
        expected.update(b"abc");

        assert_eq!(actual, expected.finish());
    }

    #[test]
    fn exact_len_reports_short_read() {
        let mut reader = Cursor::new(b"abc");
        let err = hash_exact_len(&mut reader, 4).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }
}
