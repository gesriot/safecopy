//! Чтение и запись manifest.xxh3.
//!
//! Формат совместим с `xxhsum -c`:
//! ```text
//! <hex_hash>  <relative_path>
//! ```
//! Два пробела между хешем и путём — как у GNU coreutils.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::error::{CopyError, Result};
use crate::hash::Hash;

pub const MANIFEST_FILENAME: &str = "manifest.xxh3";
pub const README_FILENAME: &str = "manifest.README.txt";
pub const README_CONTENT: &str = "\
Этот манифест содержит XXH3-128 хеши всех файлов в папке.
Для проверки целостности можно использовать:

    xxhsum -c manifest.xxh3

или `safecopy verify <путь_к_папке>`.
";

#[derive(Debug, Default, Clone)]
pub struct Manifest {
    // BTreeMap для детерминированного порядка записи.
    entries: BTreeMap<PathBuf, Hash>,
}

impl Manifest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, relative_path: PathBuf, hash: Hash) {
        self.entries.insert(relative_path, hash);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&PathBuf, &Hash)> {
        self.entries.iter()
    }

    pub fn get(&self, path: &Path) -> Option<&Hash> {
        self.entries.get(path)
    }

    pub fn write_to(&self, path: &Path) -> Result<()> {
        let file = File::create(path)?;
        let mut w = BufWriter::new(file);
        for (rel, hash) in &self.entries {
            let rel_str = rel
                .to_str()
                .ok_or_else(|| CopyError::Manifest(format!("путь не UTF-8: {}", rel.display())))?;
            // Формат xxhsum: <hash>  <path>\n  (два пробела)
            writeln!(w, "{hash}  {rel_str}")?;
        }
        w.flush()?;
        // Синхронизируем манифест на устройство: без sync_all данные могут остаться
        // в page cache и потеряться при внезапном отключении питания.
        w.get_ref().sync_all()?;
        Ok(())
    }

    pub fn read_from(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut manifest = Self::new();
        for (lineno, line) in reader.lines().enumerate() {
            let line = line?;
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                continue;
            }
            let (hash_str, rest) = trimmed.split_once("  ").ok_or_else(|| {
                CopyError::Manifest(format!(
                    "строка {}: нет разделителя из двух пробелов",
                    lineno + 1
                ))
            })?;
            let hash = Hash::from_hex(hash_str).ok_or_else(|| {
                CopyError::Manifest(format!(
                    "строка {}: некорректный hex хеш: {hash_str}",
                    lineno + 1
                ))
            })?;
            // Защита от path traversal: абсолютные пути, ".." и Windows-префиксы
            // могут вывести verify за пределы destination.
            let rel_path = PathBuf::from(rest);
            if rel_path.is_absolute()
                || rel_path.components().any(|c| {
                    matches!(
                        c,
                        std::path::Component::ParentDir | std::path::Component::Prefix(_)
                    )
                })
            {
                return Err(CopyError::Manifest(format!(
                    "строка {}: небезопасный путь в манифесте: {rest}",
                    lineno + 1
                )));
            }
            manifest.insert(rel_path, hash);
        }
        Ok(manifest)
    }
}

/// Путь к manifest.xxh3 по пути к папке или к самому манифесту.
pub fn resolve_manifest_path(target: &Path) -> PathBuf {
    if target.is_file() {
        target.to_path_buf()
    } else {
        target.join(MANIFEST_FILENAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    #[test]
    fn roundtrip() {
        let mut m = Manifest::new();
        m.insert(
            PathBuf::from("a/b.txt"),
            Hash::from_hex(&"ab".repeat(16)).unwrap(),
        );
        m.insert(
            PathBuf::from("c.jpg"),
            Hash::from_hex(&"01".repeat(16)).unwrap(),
        );

        let path = temp_dir().join(format!("manifest-test-{}.xxh3", std::process::id()));
        m.write_to(&path).unwrap();
        let read = Manifest::read_from(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(m.len(), read.len());
        let from_orig: Vec<_> = m.iter().collect();
        let from_read: Vec<_> = read.iter().collect();
        assert_eq!(from_orig, from_read);
    }
}
