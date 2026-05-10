use crate::checksums;
use crate::hash;
use egui::Context;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

#[derive(Debug)]
pub enum Msg {
    Planned {
        total: usize,
    },
    Progress {
        done: usize,
        total: usize,
        filename: String,
    },
    Item(Item),
    Finished(Result<Summary, String>),
}

#[derive(Debug, Clone)]
pub enum Item {
    #[allow(dead_code)]
    Ok {
        filename: String,
    },
    Fail {
        filename: String,
        expected: String,
        got: String,
    },
    Missing {
        filename: String,
    },
    Extra {
        filename: String,
    },
    HashError {
        filename: String,
        error: String,
    },
}

#[derive(Debug, Default, Clone)]
pub struct Summary {
    pub error: Option<String>,
    pub checked: usize,
    pub ok: usize,
    pub fail: usize,
    pub missing: usize,
    pub extra: usize,
    pub hash_errors: usize,
    pub partial: bool,
    pub cancelled: bool,
}

struct FileEntry {
    display_name: String,
    real_path: PathBuf,
}

fn collect_files(dir: &Path) -> Result<Vec<FileEntry>, String> {
    let read_dir = std::fs::read_dir(dir).map_err(|e| format!("cannot read directory: {e}"))?;

    let mut files = Vec::new();
    for entry_result in read_dir {
        let entry = entry_result.map_err(|e| format!("directory entry error: {e}"))?;
        let metadata = entry
            .metadata()
            .map_err(|e| format!("metadata error: {e}"))?;
        if metadata.is_dir() {
            continue;
        }
        let os_name = entry.file_name();
        let raw = os_name.to_string_lossy();
        let display_name = checksums::normalize(&raw);
        if checksums::should_skip(&display_name) {
            continue;
        }
        files.push(FileEntry {
            display_name,
            real_path: entry.path(),
        });
    }

    files.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    Ok(files)
}

pub fn spawn_generate(dir: PathBuf, cancel: Arc<AtomicBool>, tx: mpsc::Sender<Msg>, ctx: Context) {
    std::thread::spawn(move || {
        let result = generate_inner(&dir, &cancel, &tx, &ctx);
        tx.send(Msg::Finished(result)).ok();
        ctx.request_repaint();
    });
}

fn generate_inner(
    dir: &Path,
    cancel: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Msg>,
    ctx: &Context,
) -> Result<Summary, String> {
    let files = collect_files(dir)?;
    let total = files.len();
    tx.send(Msg::Planned { total }).ok();
    ctx.request_repaint();

    let done_count = Arc::new(AtomicUsize::new(0));

    let pairs: Vec<(String, Result<String, std::io::Error>)> = files
        .par_iter()
        .map(|f| {
            let result = hash::md5_file(&f.real_path, cancel);
            let done = done_count.fetch_add(1, Ordering::Relaxed) + 1;
            tx.send(Msg::Progress {
                done,
                total,
                filename: f.display_name.clone(),
            })
            .ok();
            ctx.request_repaint();
            (f.display_name.clone(), result)
        })
        .collect();

    let cancelled = cancel.load(Ordering::Relaxed);

    let mut entries: Vec<(String, String)> = Vec::new();
    let mut hash_errors = 0usize;

    for (name, result) in &pairs {
        match result {
            Ok(hash) => entries.push((name.clone(), hash.clone())),
            Err(e) if hash::is_cancelled(e) => {}
            Err(e) => {
                hash_errors += 1;
                tx.send(Msg::Item(Item::HashError {
                    filename: name.clone(),
                    error: e.to_string(),
                }))
                .ok();
            }
        }
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));

    if !cancelled || !entries.is_empty() {
        checksums::write(dir, &entries).map_err(|e| format!("write checksums.txt: {e}"))?;
    }

    let ok = entries.len();
    Ok(Summary {
        checked: ok,
        ok,
        hash_errors,
        partial: hash_errors > 0 || cancelled,
        cancelled,
        ..Default::default()
    })
}

pub fn spawn_check(dir: PathBuf, cancel: Arc<AtomicBool>, tx: mpsc::Sender<Msg>, ctx: Context) {
    std::thread::spawn(move || {
        let result = check_inner(&dir, &cancel, &tx, &ctx);
        tx.send(Msg::Finished(result)).ok();
        ctx.request_repaint();
    });
}

fn check_inner(
    dir: &Path,
    cancel: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Msg>,
    ctx: &Context,
) -> Result<Summary, String> {
    let checksums_path = dir.join(checksums::FILE_NAME);
    let expected = checksums::read(&checksums_path)?;

    let files = collect_files(dir)?;
    let actual: HashMap<String, PathBuf> = files
        .into_iter()
        .map(|f| (f.display_name, f.real_path))
        .collect();

    let mut missing: Vec<String> = expected
        .keys()
        .filter(|n| !actual.contains_key(*n))
        .cloned()
        .collect();
    missing.sort();

    let mut extra: Vec<String> = actual
        .keys()
        .filter(|n| !expected.contains_key(*n))
        .cloned()
        .collect();
    extra.sort();

    for name in &missing {
        tx.send(Msg::Item(Item::Missing {
            filename: name.clone(),
        }))
        .ok();
    }
    for name in &extra {
        tx.send(Msg::Item(Item::Extra {
            filename: name.clone(),
        }))
        .ok();
    }

    let to_check: Vec<(String, PathBuf, String)> = expected
        .iter()
        .filter_map(|(name, hash)| {
            actual
                .get(name)
                .map(|p| (name.clone(), p.clone(), hash.clone()))
        })
        .collect();

    let total = to_check.len();
    tx.send(Msg::Planned { total }).ok();
    ctx.request_repaint();

    let done_count = Arc::new(AtomicUsize::new(0));

    let results: Vec<(String, String, Result<String, std::io::Error>)> = to_check
        .par_iter()
        .map(|(name, path, exp_hash)| {
            let result = hash::md5_file(path, cancel);
            let done = done_count.fetch_add(1, Ordering::Relaxed) + 1;
            tx.send(Msg::Progress {
                done,
                total,
                filename: name.clone(),
            })
            .ok();
            ctx.request_repaint();
            (name.clone(), exp_hash.clone(), result)
        })
        .collect();

    let cancelled = cancel.load(Ordering::Relaxed);
    let mut ok = 0usize;
    let mut fail = 0usize;
    let mut hash_errors = 0usize;

    for (name, exp_hash, result) in results {
        match result {
            Ok(got) if got.eq_ignore_ascii_case(&exp_hash) => {
                ok += 1;
                tx.send(Msg::Item(Item::Ok { filename: name })).ok();
            }
            Ok(got) => {
                fail += 1;
                tx.send(Msg::Item(Item::Fail {
                    filename: name,
                    expected: exp_hash,
                    got,
                }))
                .ok();
            }
            Err(e) if hash::is_cancelled(&e) => {}
            Err(e) => {
                hash_errors += 1;
                tx.send(Msg::Item(Item::HashError {
                    filename: name,
                    error: e.to_string(),
                }))
                .ok();
            }
        }
    }

    Ok(Summary {
        error: None,
        checked: ok + fail + hash_errors,
        ok,
        fail,
        missing: missing.len(),
        extra: extra.len(),
        hash_errors,
        cancelled,
        partial: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::tempdir;

    fn make_file(dir: &Path, name: &str, content: &[u8]) {
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(content).unwrap();
    }

    fn make_checksums(dir: &Path, entries: &[(&str, &str)]) {
        let mut f = std::fs::File::create(dir.join(checksums::FILE_NAME)).unwrap();
        for (name, hash) in entries {
            writeln!(f, "{hash}  {name}").unwrap();
        }
    }

    #[test]
    fn generate_empty_directory_succeeds() {
        let dir = tempdir().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = mpsc::channel::<Msg>();
        let ctx = egui::Context::default();
        let summary = generate_inner(dir.path(), &cancel, &tx, &ctx).unwrap();
        assert_eq!(summary.ok, 0);
        assert!(!summary.partial);
    }

    #[test]
    fn generate_creates_checksums_file() {
        let dir = tempdir().unwrap();
        make_file(dir.path(), "hello.txt", b"hello world");
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = mpsc::channel::<Msg>();
        let ctx = egui::Context::default();
        generate_inner(dir.path(), &cancel, &tx, &ctx).unwrap();
        assert!(dir.path().join(checksums::FILE_NAME).exists());
    }

    #[test]
    fn generate_empty_directory_creates_empty_checksums_file() {
        let dir = tempdir().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = mpsc::channel::<Msg>();
        let ctx = egui::Context::default();
        let summary = generate_inner(dir.path(), &cancel, &tx, &ctx).unwrap();
        let checksums_path = dir.path().join(checksums::FILE_NAME);
        assert_eq!(summary.ok, 0);
        assert!(checksums_path.exists());
        assert_eq!(std::fs::read_to_string(checksums_path).unwrap(), "");
    }

    #[test]
    fn check_all_ok() {
        let dir = tempdir().unwrap();
        make_file(dir.path(), "hello.txt", b"hello world");
        make_checksums(
            dir.path(),
            &[("hello.txt", "5eb63bbbe01eeed093cb22bb8f5acdc3")],
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = mpsc::channel::<Msg>();
        let ctx = egui::Context::default();
        let s = check_inner(dir.path(), &cancel, &tx, &ctx).unwrap();
        assert_eq!(s.ok, 1);
        assert_eq!(s.fail, 0);
        assert_eq!(s.missing, 0);
        assert_eq!(s.extra, 0);
    }

    #[test]
    fn check_detects_fail() {
        let dir = tempdir().unwrap();
        make_file(dir.path(), "hello.txt", b"hello world");
        make_checksums(
            dir.path(),
            &[("hello.txt", "aaaabbbbccccddddeeeeffffaaaabbbb")],
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = mpsc::channel::<Msg>();
        let ctx = egui::Context::default();
        let s = check_inner(dir.path(), &cancel, &tx, &ctx).unwrap();
        assert_eq!(s.ok, 0);
        assert_eq!(s.fail, 1);
    }

    #[test]
    fn check_detects_missing_and_extra() {
        let dir = tempdir().unwrap();
        make_file(dir.path(), "actual.txt", b"data");
        make_checksums(
            dir.path(),
            &[("expected.txt", "d41d8cd98f00b204e9800998ecf8427e")],
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = mpsc::channel::<Msg>();
        let ctx = egui::Context::default();
        let s = check_inner(dir.path(), &cancel, &tx, &ctx).unwrap();
        assert_eq!(s.missing, 1);
        assert_eq!(s.extra, 1);
    }
}
