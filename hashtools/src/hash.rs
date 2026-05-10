use md5::{Digest, Md5};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const CHUNK: usize = 64 * 1024;

pub fn md5_file(path: &Path, cancel: &AtomicBool) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; CHUNK];

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

pub fn is_cancelled(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::Interrupted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::tempdir;

    #[test]
    fn hash_empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty");
        File::create(&path).unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            md5_file(&path, &cancel).unwrap(),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
    }

    #[test]
    fn hash_known_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        let mut f = File::create(&path).unwrap();
        f.write_all(b"hello world").unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            md5_file(&path, &cancel).unwrap(),
            "5eb63bbbe01eeed093cb22bb8f5acdc3"
        );
    }

    #[test]
    fn cancellation_returns_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("big.bin");
        let mut f = File::create(&path).unwrap();
        f.write_all(&vec![0u8; 256 * 1024]).unwrap();
        let cancel = AtomicBool::new(true);
        let err = md5_file(&path, &cancel).unwrap_err();
        assert!(is_cancelled(&err));
    }
}
