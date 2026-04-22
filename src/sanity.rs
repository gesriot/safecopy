//! Pre-flight sanity-check карты.
//!
//! Пишем 10 МБ детерминированного псевдослучайного паттерна, sync-close-open-cold-read,
//! сверяем хеш. Детектирует мёртвый картридер или неисправную карту до начала
//! основной работы.

use std::io::Write;
use std::path::Path;

use crate::error::{CopyError, Result};
use crate::hash::{cold_hash_file, Hasher};
use crate::io_flags::{self, IoBuf, BLOCK_SIZE};

const SANITY_SIZE: usize = 10 * 1024 * 1024;
const SANITY_FILENAME: &str = ".safecopy-sanity.tmp";

pub fn run(destination_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(destination_dir)?;
    let path = destination_dir.join(SANITY_FILENAME);

    let expected = write_sanity_file(&path)?;
    let got = cold_read_hash(&path)?;
    std::fs::remove_file(&path).ok();

    if expected != got {
        return Err(CopyError::SanityFailed {
            reason: format!("хеш не совпал: ожидали {expected}, получили {got}"),
        });
    }
    Ok(())
}

fn write_sanity_file(path: &Path) -> Result<crate::hash::Hash> {
    let mut f = io_flags::open_dest_write(path)?;

    let mut hasher = Hasher::new();
    let mut buf = IoBuf::new(BLOCK_SIZE);
    let mut rng_state: u64 = 0xDEAD_BEEF_CAFE_BABE;

    let mut written = 0usize;
    while written < SANITY_SIZE {
        fill_pseudo_random(&mut buf, &mut rng_state);
        let take = buf.len().min(SANITY_SIZE - written);
        f.write_all(&buf[..take])?;
        hasher.update(&buf[..take]);
        written += take;
    }

    io_flags::full_sync(&f)?;
    drop(f);
    Ok(hasher.finish())
}

fn cold_read_hash(path: &Path) -> Result<crate::hash::Hash> {
    cold_hash_file(path).map_err(CopyError::Io)
}

/// Детерминированный xorshift-64 — чтобы между запусками можно было
/// воспроизвести паттерн, но содержимое было «шумным» (а не нулями).
fn fill_pseudo_random(buf: &mut [u8], state: &mut u64) {
    let mut i = 0;
    while i < buf.len() {
        let mut s = *state;
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        *state = s;
        let bytes = s.to_le_bytes();
        let take = bytes.len().min(buf.len() - i);
        buf[i..i + take].copy_from_slice(&bytes[..take]);
        i += take;
    }
}
