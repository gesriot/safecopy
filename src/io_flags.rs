//! Кросс-платформенный cache-bypass, full-sync и выровненный I/O буфер.
//!
//! Реализации:
//! * macOS — `fcntl(F_NOCACHE, 1)` + `fcntl(F_FULLFSYNC)`
//! * Windows — `FILE_FLAG_NO_BUFFERING` + `FILE_FLAG_WRITE_THROUGH` + `FlushFileBuffers`
//! * Прочий Unix (Linux) — `posix_fadvise(DONTNEED)` + `fsync`

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Рекомендуемый размер блока I/O (1 МБ).
/// Кратен любому разумному sector size (512 Б, 4 КБ) и хорошо ложится на exFAT.
pub const BLOCK_SIZE: usize = 1024 * 1024;

/// Открыть файл для чтения с обходом page cache ОС.
///
/// На Windows открывает с `FILE_FLAG_NO_BUFFERING` — чтение идёт физически с устройства,
/// минуя page cache. Требует буферы, выровненные по [`SECTOR_ALIGN`] (используй [`IoBuf`]).
/// На macOS — `F_NOCACHE`.
/// На прочих Unix — `posix_fadvise(DONTNEED)`.
pub fn open_cold_read(path: &Path) -> io::Result<File> {
    open_cold_read_impl(path)
}

/// Открыть файл назначения для записи.
///
/// На Windows добавляет `FILE_FLAG_WRITE_THROUGH` — запись проходит сквозь page cache
/// прямо в аппаратный write-back кэш устройства. Последующий `full_sync` (= `FlushFileBuffers`)
/// проталкивает данные до физического носителя.
pub fn open_dest_write(path: &Path) -> io::Result<File> {
    open_dest_write_impl(path)
}

/// Гарантированный сброс данных в устройство (сильнее обычного fsync).
pub fn full_sync(file: &File) -> io::Result<()> {
    file.sync_all()?;
    full_sync_platform(file)
}

// ---------------------------------------------------------------------------
// I/O Buffer
// ---------------------------------------------------------------------------

/// Буфер для I/O с гарантией выравнивания, необходимого `FILE_FLAG_NO_BUFFERING` на Windows.
///
/// На Windows выделяется с выравниванием [`SECTOR_ALIGN`] байт через глобальный аллокатор.
/// На прочих платформах — обычный `Vec<u8>` (глобальный аллокатор достаточен).
///
/// Реализует `Deref<Target=[u8]>` и `DerefMut`, поэтому совместим с `Read::read`,
/// `Write::write_all` и срезами без явных преобразований.
pub struct IoBuf {
    #[cfg(windows)]
    inner: WindowsAlignedBuf,
    #[cfg(not(windows))]
    inner: Vec<u8>,
}

impl IoBuf {
    /// Выделить новый буфер размером `size` байт, заполненный нулями.
    pub fn new(size: usize) -> Self {
        #[cfg(windows)]
        {
            IoBuf {
                inner: WindowsAlignedBuf::new(size),
            }
        }
        #[cfg(not(windows))]
        {
            IoBuf {
                inner: vec![0u8; size],
            }
        }
    }
}

impl std::ops::Deref for IoBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        #[cfg(windows)]
        {
            &self.inner
        }
        #[cfg(not(windows))]
        {
            &self.inner
        }
    }
}

impl std::ops::DerefMut for IoBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        #[cfg(windows)]
        {
            &mut self.inner
        }
        #[cfg(not(windows))]
        {
            &mut self.inner
        }
    }
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn open_cold_read_impl(path: &Path) -> io::Result<File> {
    use std::os::unix::io::AsRawFd;
    let file = OpenOptions::new().read(true).open(path)?;
    let fd = file.as_raw_fd();
    // SAFETY: fd валиден, пока file живёт; F_NOCACHE — документированный fcntl.
    let rc = unsafe { libc::fcntl(fd, libc::F_NOCACHE, 1) };
    if rc == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

#[cfg(target_os = "macos")]
fn open_dest_write_impl(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

#[cfg(target_os = "macos")]
fn full_sync_platform(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: fd валиден.
    let rc = unsafe { libc::fcntl(fd, libc::F_FULLFSYNC) };
    if rc == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Linux / прочий Unix
// ---------------------------------------------------------------------------

#[cfg(all(unix, not(target_os = "macos")))]
fn open_cold_read_impl(path: &Path) -> io::Result<File> {
    use std::os::unix::io::AsRawFd;
    let file = OpenOptions::new().read(true).open(path)?;
    let fd = file.as_raw_fd();
    // posix_fadvise DONTNEED — подсказка ядру не кэшировать эти страницы.
    // SAFETY: fd валиден.
    let rc = unsafe { libc::posix_fadvise(fd, 0, 0, libc::POSIX_FADV_DONTNEED) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    Ok(file)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_dest_write_impl(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

#[cfg(all(unix, not(target_os = "macos")))]
#[allow(clippy::unnecessary_wraps)]
fn full_sync_platform(_file: &File) -> io::Result<()> {
    // На Linux sync_all() = fsync; более строгого аналога F_FULLFSYNC нет.
    Ok(())
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// Alignment requirement for `FILE_FLAG_NO_BUFFERING` read buffers.
/// 4096 покрывает как 512-байтовые (NAND), так и 4096-байтовые (Advanced Format) сектора.
#[cfg(windows)]
const SECTOR_ALIGN: usize = 4096;

/// `FILE_FLAG_NO_BUFFERING` — чтение/запись минуют page cache, идут прямо на устройство.
/// Требует: адрес буфера и размер запроса кратны sector size; позиция в файле — тоже.
#[cfg(windows)]
const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;

/// `FILE_FLAG_WRITE_THROUGH` — записи проходят сквозь page cache в аппаратный кэш устройства.
/// Не требует выравнивания буферов (в отличие от `NO_BUFFERING`).
#[cfg(windows)]
const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;

#[cfg(windows)]
fn open_cold_read_impl(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_NO_BUFFERING)
        .open(path)
}

#[cfg(windows)]
fn open_dest_write_impl(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .custom_flags(FILE_FLAG_WRITE_THROUGH)
        .open(path)
}

#[cfg(windows)]
#[allow(clippy::unnecessary_wraps)]
fn full_sync_platform(_file: &File) -> io::Result<()> {
    // sync_all() на Windows транслируется в FlushFileBuffers — этого достаточно.
    Ok(())
}

/// Буфер, выровненный по `SECTOR_ALIGN` байт для использования с `FILE_FLAG_NO_BUFFERING`.
///
/// Размер запроса `ReadFile` должен быть кратен sector size — `BLOCK_SIZE` (1 МБ) удовлетворяет
/// этому условию. Для последнего блока файла Windows возвращает фактическое количество
/// прочитанных байт (может быть меньше `BLOCK_SIZE`), без ошибки — это штатное поведение.
#[cfg(windows)]
struct WindowsAlignedBuf {
    ptr: std::ptr::NonNull<u8>,
    layout: std::alloc::Layout,
    len: usize,
}

#[cfg(windows)]
impl WindowsAlignedBuf {
    fn new(size: usize) -> Self {
        use std::alloc::{alloc_zeroed, Layout};
        let layout =
            Layout::from_size_align(size, SECTOR_ALIGN).expect("valid sector-aligned layout");
        // SAFETY: layout имеет ненулевой размер (BLOCK_SIZE = 1 МБ).
        let ptr = unsafe { alloc_zeroed(layout) };
        let ptr = std::ptr::NonNull::new(ptr).expect("sector-aligned allocation failed");
        WindowsAlignedBuf {
            ptr,
            layout,
            len: size,
        }
    }
}

#[cfg(windows)]
impl std::ops::Deref for WindowsAlignedBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: ptr валиден для len байт и инициализирован нулями при создании.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

#[cfg(windows)]
impl std::ops::DerefMut for WindowsAlignedBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: ptr валиден для len байт; у нас эксклюзивный доступ (&mut self).
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

#[cfg(windows)]
impl Drop for WindowsAlignedBuf {
    fn drop(&mut self) {
        // SAFETY: ptr был выделен с этим самым layout-ом.
        unsafe { std::alloc::dealloc(self.ptr.as_ptr(), self.layout) }
    }
}

// SAFETY: WindowsAlignedBuf владеет выделенной памятью эксклюзивно; указатель не алиасируется.
#[cfg(windows)]
unsafe impl Send for WindowsAlignedBuf {}
// SAFETY: &WindowsAlignedBuf даёт &[u8] — иммутабельный разделяемый доступ безопасен.
#[cfg(windows)]
unsafe impl Sync for WindowsAlignedBuf {}
