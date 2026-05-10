use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use unicode_normalization::UnicodeNormalization;

pub const FILE_NAME: &str = "checksums.txt";

pub fn should_skip(name: &str) -> bool {
    name == FILE_NAME || name.ends_with(".DS_Store")
}

pub fn normalize(s: &str) -> String {
    s.nfc().collect()
}

pub fn read(path: &Path) -> Result<HashMap<String, String>, String> {
    let file = fs::File::open(path).map_err(|e| format!("cannot open checksums file: {e}"))?;
    let reader = BufReader::new(file);
    let mut map = HashMap::new();

    for (idx, line_result) in reader.lines().enumerate() {
        let line_num = idx + 1;
        let line = line_result.map_err(|e| format!("read error at line {line_num}: {e}"))?;

        let Some((hash_part, name_part)) = line.split_once("  ") else {
            return Err(format!("line {line_num}: expected '<md5>  <filename>'"));
        };

        if hash_part.len() != 32 || !hash_part.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("line {line_num}: invalid MD5 hash"));
        }
        if name_part.is_empty() {
            return Err(format!("line {line_num}: empty filename"));
        }
        if name_part.contains('/') || name_part.contains('\\') {
            return Err(format!("line {line_num}: filename must not include a path"));
        }

        let filename = normalize(name_part);
        if should_skip(&filename) {
            continue;
        }
        if map.contains_key(&filename) {
            return Err(format!("line {line_num}: duplicate filename {filename:?}"));
        }
        map.insert(filename, hash_part.to_ascii_lowercase());
    }

    Ok(map)
}

pub fn write(dir: &Path, entries: &[(String, String)]) -> io::Result<()> {
    let pid = std::process::id();
    let tmp_path = dir.join(format!("checksums-{pid}.tmp"));

    let file = fs::File::create(&tmp_path)?;
    let mut writer = BufWriter::new(file);

    for (name, hash) in entries {
        writeln!(writer, "{hash}  {name}")?;
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    drop(writer);

    let target = dir.join(FILE_NAME);
    if let Err(e) = fs::rename(&tmp_path, &target) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn normalize_nfd_to_nfc() {
        let nfd = "cafe\u{0301}";
        assert_eq!(normalize(nfd), "café");
    }

    #[test]
    fn normalize_already_nfc_unchanged() {
        let s = "photo_été.jpg";
        assert_eq!(normalize(s), s);
    }

    #[test]
    fn should_skip_checksums() {
        assert!(should_skip("checksums.txt"));
    }

    #[test]
    fn should_skip_ds_store() {
        assert!(should_skip(".DS_Store"));
        assert!(should_skip("folder.DS_Store"));
    }

    #[test]
    fn should_not_skip_regular_file() {
        assert!(!should_skip("photo.jpg"));
        assert!(!should_skip("document.pdf"));
    }

    #[test]
    fn read_valid_checksums() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("checksums.txt");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "d41d8cd98f00b204e9800998ecf8427e  file_a.txt").unwrap();
        writeln!(f, "098f6bcd4621d373cade4e832627b4f6  file_b.txt").unwrap();

        let map = read(&path).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map["file_a.txt"], "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(map["file_b.txt"], "098f6bcd4621d373cade4e832627b4f6");
    }

    #[test]
    fn read_normalizes_filenames_to_nfc() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("checksums.txt");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "d41d8cd98f00b204e9800998ecf8427e  cafe\u{0301}.txt").unwrap();
        let map = read(&path).unwrap();
        assert!(map.contains_key("café.txt"));
    }

    #[test]
    fn read_rejects_duplicate_filename() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("checksums.txt");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "d41d8cd98f00b204e9800998ecf8427e  file.txt").unwrap();
        writeln!(f, "d41d8cd98f00b204e9800998ecf8427e  file.txt").unwrap();
        assert!(read(&path).is_err());
    }

    #[test]
    fn read_rejects_bad_hash() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("checksums.txt");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "not_a_hash  file.txt").unwrap();
        assert!(read(&path).is_err());
    }

    #[test]
    fn read_rejects_path_in_filename() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("checksums.txt");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "d41d8cd98f00b204e9800998ecf8427e  subdir/file.txt").unwrap();
        assert!(read(&path).is_err());
    }

    #[test]
    fn write_and_read_roundtrip() {
        let dir = tempdir().unwrap();
        let entries = vec![
            (
                "photo.jpg".to_string(),
                "d41d8cd98f00b204e9800998ecf8427e".to_string(),
            ),
            (
                "video.mp4".to_string(),
                "098f6bcd4621d373cade4e832627b4f6".to_string(),
            ),
        ];
        write(dir.path(), &entries).unwrap();
        let map = read(&dir.path().join(FILE_NAME)).unwrap();
        assert_eq!(map["photo.jpg"], "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(map["video.mp4"], "098f6bcd4621d373cade4e832627b4f6");
    }
}
