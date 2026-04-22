//! Типы ошибок и классификация.
//!
//! Классификация определяет реакцию pipeline:
//! * `Transient` — retry с backoff.
//! * `PersistentFile` — карантин файла, продолжаем со следующим.
//! * `PersistentDevice` — остановка всей операции, алерт.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    Transient,
    PersistentFile,
    PersistentDevice,
}

#[derive(Debug, Error)]
pub enum CopyError {
    #[error("хеш не совпал для {path}: записано {written}, прочитано {read_back}")]
    HashMismatch {
        path: PathBuf,
        written: String,
        read_back: String,
    },

    #[error("исчерпан лимит попыток ({attempts}) для {path}")]
    RetriesExhausted { path: PathBuf, attempts: u32 },

    #[error("sanity-check карты провален: {reason}")]
    SanityFailed { reason: String },

    #[error("ошибка чтения источника {path}: {source}")]
    SourceRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("ошибка записи {path}: {source}")]
    DestinationWrite {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("ошибка cold-read при верификации {path}: {source}")]
    VerifyRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("manifest: {0}")]
    Manifest(String),

    #[error("I/O: {0}")]
    Io(#[from] io::Error),
}

impl CopyError {
    pub fn classify(&self) -> ErrorClass {
        use CopyError::{
            DestinationWrite, HashMismatch, Io, Manifest, RetriesExhausted, SanityFailed,
            SourceRead, VerifyRead,
        };
        match self {
            SanityFailed { .. } => ErrorClass::PersistentDevice,

            RetriesExhausted { .. } | SourceRead { .. } | Manifest(_) => {
                ErrorClass::PersistentFile
            }

            HashMismatch { .. } => ErrorClass::Transient,

            // Смотрим на kind() внутреннего io::Error:
            // StorageFull / PermissionDenied / NotFound → PersistentDevice (смысла retry нет).
            // Прочие I/O ошибки (USB glitch, временный сбой) → Transient.
            DestinationWrite { source, .. } | VerifyRead { source, .. } => {
                classify_io_error(source)
            }

            Io(e) => classify_io_error(e),
        }
    }
}

fn classify_io_error(e: &io::Error) -> ErrorClass {
    use io::ErrorKind::{NotFound, PermissionDenied, StorageFull};
    match e.kind() {
        StorageFull | PermissionDenied | NotFound => ErrorClass::PersistentDevice,
        _ => ErrorClass::Transient,
    }
}

pub type Result<T> = std::result::Result<T, CopyError>;
