//! Копирование timestamps.
//!
//! * mtime + atime — штатно через `filetime` (Unix) / `File::set_times` (Windows).
//! * Creation time — только Windows: `FileTimesExt::set_created` (стабильно с Rust 1.75).

use std::io;
use std::path::Path;

pub fn copy_times(source: &Path, destination: &Path) -> io::Result<()> {
    copy_times_impl(source, destination)
}

#[cfg(windows)]
fn copy_times_impl(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::fs::FileTimesExt as _;

    let meta = std::fs::metadata(source)?;
    let mtime = meta.modified()?;
    let atime = meta.accessed()?;

    // set_created доступен через FileTimesExt (stable с 1.75 на windows).
    let mut ft = std::fs::FileTimes::new()
        .set_accessed(atime)
        .set_modified(mtime);
    if let Ok(ctime) = meta.created() {
        ft = ft.set_created(ctime);
    }

    // Открываем destination только для записи метаданных (без truncate).
    let dest_file = std::fs::OpenOptions::new().write(true).open(destination)?;
    dest_file.set_times(ft)
}

#[cfg(not(windows))]
fn copy_times_impl(source: &Path, destination: &Path) -> io::Result<()> {
    use filetime::FileTime;
    let meta = std::fs::metadata(source)?;
    let mtime = FileTime::from_last_modification_time(&meta);
    let atime = FileTime::from_last_access_time(&meta);
    filetime::set_file_times(destination, atime, mtime)
}
