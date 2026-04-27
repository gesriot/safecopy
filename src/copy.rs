//! Safe-copy pipeline: .tmp → write → sync → close → cold-read verify → rename.
//!
//! Один файл за раз, без параллелизма между файлами: внутри файла reader/writer
//! стадии перекрываются, но на SD-карту всегда пишет ровно один writer.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context};
use crossbeam_channel::{bounded, Receiver, Sender};
use indicatif::{ProgressBar, ProgressStyle};
use walkdir::WalkDir;

use crate::cli::CopyOpts;
use crate::error::{CopyError, ErrorClass, Result};
use crate::hash::{cold_hash_file, Hash, Hasher};
use crate::io_flags::{self, IoBuf, BLOCK_SIZE};
use crate::manifest::{Manifest, MANIFEST_FILENAME, README_CONTENT, README_FILENAME};
use crate::progress::{LogLevel, NoopReporter, ProgressEvent, ProgressPhase, ProgressReporter};
use crate::quarantine::{self, QuarantineReport};
use crate::{sanity, timestamps};

/// Сколько раз подряд может упасть с `PersistentFile` прежде чем мы решим,
/// что проблема в устройстве, а не в конкретных файлах.
const CONSECUTIVE_FAILURE_LIMIT: u32 = 5;

/// Префикс временного имени при записи.
const TMP_SUFFIX: &str = ".safecopy.tmp";

/// Сколько 1 МБ буферов держим в обороте между reader/writer стадиями.
const BUFFER_POOL_SIZE: usize = 4;

struct FileEntry {
    source_abs: PathBuf,
    relative: PathBuf,
    size: u64,
}

struct CopyOutcome {
    manifest: Manifest,
    failed_files: Vec<PathBuf>,
}

struct Chunk {
    buf: IoBuf,
    len: usize,
}

enum ReaderMessage {
    Chunk(Chunk),
    Done { hash: Hash, bytes: u64 },
    Failed(CopyError),
}

pub fn run(opts: &CopyOpts) -> anyhow::Result<()> {
    run_with_reporter(opts, &NoopReporter)
}

pub fn run_with_reporter(opts: &CopyOpts, reporter: &dyn ProgressReporter) -> anyhow::Result<()> {
    let (source, destination) = prepare_copy(opts)?;

    cleanup_stale_tmp(&destination);

    reporter.report(ProgressEvent::Phase(ProgressPhase::Sanity));
    report_log(reporter, LogLevel::Info, "Sanity-check карты...");
    println!("Sanity-check карты…");
    sanity::run(&destination)
        .context("sanity-check провален — возможно, неисправен картридер или карта")?;
    report_log(reporter, LogLevel::Success, "Карта отвечает корректно");
    println!("  ✓ карта отвечает корректно");

    reporter.report(ProgressEvent::Phase(ProgressPhase::Scanning));
    report_log(
        reporter,
        LogLevel::Info,
        format!("Сканирование {}...", source.display()),
    );
    println!("Сканирование {}…", source.display());
    let entries = scan_source(&source)?;
    if !opts.no_manifest_on_card {
        check_manifest_name_conflict(&entries)?;
    }
    // Когда сам пользователь копирует файл с именем manifest.xxh3, destination/manifest.xxh3 —
    // это его данные (или ровно они после прошлого no-manifest прогона), а не SafeCopy-метаданные.
    // Парсить такой файл как манифест нельзя: упадём на разделителях.
    let source_has_manifest_artifact = entries
        .iter()
        .any(|e| e.relative.as_os_str() == MANIFEST_FILENAME);
    let existing_manifest = if source_has_manifest_artifact {
        report_log(
            reporter,
            LogLevel::Info,
            format!(
                "Источник содержит {MANIFEST_FILENAME} — resume по существующему манифесту отключён"
            ),
        );
        None
    } else {
        load_existing_manifest(&destination)?
    };
    let total_bytes: u64 = entries.iter().map(|e| e.size).sum();
    reporter.report(ProgressEvent::Phase(ProgressPhase::Copying));
    reporter.report(ProgressEvent::TotalBytes(total_bytes));
    report_log(
        reporter,
        LogLevel::Info,
        format!(
            "Найдено {} файлов, {}",
            entries.len(),
            format_bytes(total_bytes)
        ),
    );
    println!(
        "  найдено {} файлов, {} МБ",
        entries.len(),
        total_bytes / (1024 * 1024)
    );

    let bar = copy_progress_bar(total_bytes);
    let outcome = copy_entries(
        &entries,
        &destination,
        existing_manifest.as_ref(),
        opts,
        &bar,
        reporter,
    )?;
    bar.finish_with_message("копирование завершено");

    if outcome.manifest.is_empty() {
        bail!("ни один файл не скопирован успешно");
    }

    println!(
        "\nCooldown {} секунд перед финальной верификацией…",
        opts.cooldown_secs
    );
    run_cooldown(opts.cooldown_secs, reporter);

    reporter.report(ProgressEvent::Phase(ProgressPhase::Verifying));
    println!("Финальный cold re-read всех файлов…");
    final_reread(&destination, &outcome.manifest, reporter)?;
    report_log(
        reporter,
        LogLevel::Success,
        "Все файлы прошли повторную проверку",
    );
    println!("  ✓ все файлы прошли повторную проверку");

    if !opts.no_manifest_on_card {
        write_manifest_artifacts(&destination, &outcome.manifest)?;
        report_log(
            reporter,
            LogLevel::Success,
            format!("{MANIFEST_FILENAME} записан на карту"),
        );
        println!("  ✓ {MANIFEST_FILENAME} записан на карту");
    }

    reporter.report(ProgressEvent::Phase(ProgressPhase::Done));
    report_log(
        reporter,
        LogLevel::Success,
        format!("Готово. Скопировано: {} файлов.", outcome.manifest.len()),
    );
    print_summary(&outcome);
    Ok(())
}

fn prepare_copy(opts: &CopyOpts) -> anyhow::Result<(PathBuf, PathBuf)> {
    let source = opts
        .source
        .canonicalize()
        .with_context(|| format!("source не найден: {}", opts.source.display()))?;
    if !source.is_file() && !source.is_dir() {
        bail!("source должен быть файлом или папкой: {}", source.display());
    }

    fs::create_dir_all(&opts.destination).with_context(|| {
        format!(
            "не удалось создать destination {}",
            opts.destination.display()
        )
    })?;
    let destination = opts.destination.canonicalize()?;

    Ok((source, destination))
}

fn load_existing_manifest(destination: &Path) -> anyhow::Result<Option<Manifest>> {
    let manifest_path = destination.join(MANIFEST_FILENAME);
    // Resume: читаем существующий манифест, если он есть.
    // Файлы пропускаем позже, только если source и cold-read destination совпали с ним.
    if manifest_path.exists() {
        match Manifest::read_from(&manifest_path) {
            Ok(m) => {
                println!(
                    "Найден существующий манифест ({} файлов) — продолжаем с места остановки.",
                    m.len()
                );
                Ok(Some(m))
            }
            Err(e) => bail!(
                "не удалось прочитать существующий манифест: {e}\n\
                     Если хотите начать заново — удалите {}",
                manifest_path.display()
            ),
        }
    } else {
        Ok(None)
    }
}

fn copy_progress_bar(total_bytes: u64) -> ProgressBar {
    let bar = ProgressBar::new(total_bytes);
    bar.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, ETA {eta}) {msg}",
        )
        .expect("valid template")
        .progress_chars("=>-"),
    );
    bar
}

fn run_cooldown(seconds: u64, reporter: &dyn ProgressReporter) {
    reporter.report(ProgressEvent::Phase(ProgressPhase::Cooldown));
    reporter.report(ProgressEvent::CooldownLeft(seconds));
    if seconds == 0 {
        return;
    }

    report_log(
        reporter,
        LogLevel::Info,
        format!("Cooldown {seconds} секунд перед финальной верификацией"),
    );
    for remaining in (0..seconds).rev() {
        thread::sleep(Duration::from_secs(1));
        reporter.report(ProgressEvent::CooldownLeft(remaining));
    }
}

fn report_log(reporter: &dyn ProgressReporter, level: LogLevel, message: impl Into<String>) {
    reporter.report(ProgressEvent::Log {
        level,
        message: message.into(),
    });
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut divisor = 1u64;
    let mut unit = 0usize;
    while bytes / divisor >= 1024 && unit + 1 < UNITS.len() {
        divisor *= 1024;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        let whole = bytes / divisor;
        let decimal = (bytes % divisor) * 10 / divisor;
        format!("{whole}.{decimal} {}", UNITS[unit])
    }
}

fn copy_entries(
    entries: &[FileEntry],
    destination: &Path,
    existing_manifest: Option<&Manifest>,
    opts: &CopyOpts,
    bar: &ProgressBar,
    reporter: &dyn ProgressReporter,
) -> anyhow::Result<CopyOutcome> {
    let mut manifest = Manifest::new();
    let mut consecutive_failures: u32 = 0;
    let mut failed_files: Vec<PathBuf> = Vec::new();

    for entry in entries {
        bar.set_message(entry.relative.display().to_string());
        reporter.report(ProgressEvent::CurrentFile(
            entry.relative.display().to_string(),
        ));
        let dest_abs = destination.join(&entry.relative);

        // Resume: пропустить файл только если и source, и cold-read destination
        // совпадают с уже записанным манифестом.
        if let Some(em) = existing_manifest {
            if let Some(&prev_hash) = em.get(&entry.relative) {
                if should_skip_existing(&entry.source_abs, &dest_abs, prev_hash) {
                    manifest.insert(entry.relative.clone(), prev_hash);
                    bar.inc(entry.size);
                    reporter.report(ProgressEvent::BytesAdvanced(entry.size));
                    report_log(
                        reporter,
                        LogLevel::Success,
                        format!("[OK] {} уже проверен", entry.relative.display()),
                    );
                    continue;
                }
            }
        }

        match copy_one(
            &entry.source_abs,
            &dest_abs,
            opts.max_retries,
            opts.unlimited_retries,
            bar,
            reporter,
        ) {
            Ok(hash) => {
                manifest.insert(entry.relative.clone(), hash);
                consecutive_failures = 0;
                report_log(
                    reporter,
                    LogLevel::Success,
                    format!("[OK] {}", entry.relative.display()),
                );
            }
            Err(e) => {
                let class = e.classify();
                eprintln!("\n✗ {}: {e}", entry.relative.display());
                report_log(
                    reporter,
                    LogLevel::Error,
                    format!("[ERROR] {}: {e}", entry.relative.display()),
                );

                quarantine::record(
                    destination,
                    &QuarantineReport {
                        source_relative: &entry.relative,
                        reason: &e.to_string(),
                        attempts: opts.max_retries,
                    },
                )
                .context("не удалось записать запись в карантин")?;
                report_log(
                    reporter,
                    LogLevel::Quarantine,
                    format!("[QUARANTINE] {}", entry.relative.display()),
                );

                failed_files.push(entry.relative.clone());
                bar.inc(entry.size);
                reporter.report(ProgressEvent::BytesAdvanced(entry.size));

                match class {
                    ErrorClass::PersistentDevice => {
                        bar.abandon_with_message("остановлено: неисправность устройства");
                        bail!(
                            "неисправность устройства (файл {}): {e}",
                            entry.relative.display()
                        );
                    }
                    ErrorClass::Transient | ErrorClass::PersistentFile => {
                        consecutive_failures += 1;
                        if consecutive_failures >= CONSECUTIVE_FAILURE_LIMIT {
                            bar.abandon_with_message(
                                "остановлено: слишком много подряд failed-файлов",
                            );
                            bail!(
                                "{consecutive_failures} файлов подряд упали — похоже, проблема в устройстве, а не в файлах"
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(CopyOutcome {
        manifest,
        failed_files,
    })
}

fn should_skip_existing(source: &Path, dest: &Path, expected: Hash) -> bool {
    if !dest.exists() {
        return false;
    }
    if !matches!(cold_read_hash(dest), Ok(hash) if hash == expected) {
        return false;
    }
    matches!(hash_source_file(source), Ok(hash) if hash == expected)
}

fn print_summary(outcome: &CopyOutcome) {
    println!("\nГотово. Скопировано: {} файлов.", outcome.manifest.len());
    if !outcome.failed_files.is_empty() {
        println!("В карантине: {} файлов:", outcome.failed_files.len());
        for f in &outcome.failed_files {
            println!("  - {}", f.display());
        }
    }
}

fn scan_source(source: &Path) -> anyhow::Result<Vec<FileEntry>> {
    if source.is_file() {
        let relative = PathBuf::from(source.file_name().context("у source-файла нет имени")?);
        let size = source.metadata().context("metadata")?.len();
        return Ok(vec![FileEntry {
            source_abs: source.to_path_buf(),
            relative,
            size,
        }]);
    }

    let mut entries = Vec::new();
    for entry in WalkDir::new(source).follow_links(false).sort_by_file_name() {
        let entry = entry.context("ошибка обхода source")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let abs = entry.path().to_path_buf();
        let relative = abs
            .strip_prefix(source)
            .context("strip_prefix failed")?
            .to_path_buf();
        let size = entry.metadata().context("metadata")?.len();
        entries.push(FileEntry {
            source_abs: abs,
            relative,
            size,
        });
    }
    Ok(entries)
}

/// Артефакты манифеста пишутся в корень destination, так что относительный путь
/// в корне с тем же именем затёр бы только что скопированный файл.
fn check_manifest_name_conflict(entries: &[FileEntry]) -> anyhow::Result<()> {
    for entry in entries {
        let name = entry.relative.as_os_str();
        if name == MANIFEST_FILENAME || name == README_FILENAME {
            bail!(
                "имя {} конфликтует с артефактом манифеста — \
                 запустите с --no-manifest-on-card или переименуйте файл",
                entry.relative.display()
            );
        }
    }
    Ok(())
}

/// Копирует один файл с retry; возвращает финальный xxh3 хеш.
///
/// В режиме `unlimited_retries` для transient-ошибок записи/верификации повторяет
/// попытки без ограничений: каждая новая попытка пишет в **новый** `.tmp.<N>`, а
/// предыдущие неудачные `.tmp` не удаляются — они держат свои кластеры занятыми,
/// так что FS вынуждена отдавать следующей попытке другие сектора карты.
/// Остановка — только по `PersistentDevice` (disk full / permission denied) или
/// по истощению `max_retries` для `PersistentFile` (нечитаемый source).
fn copy_one(
    source: &Path,
    dest: &Path,
    max_retries: u32,
    unlimited_retries: bool,
    bar: &ProgressBar,
    reporter: &dyn ProgressReporter,
) -> Result<Hash> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut kept_tmps: Vec<PathBuf> = Vec::new();
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;
        let tmp = tmp_path_for(dest, attempt);
        // На всякий случай: если .tmp.<N> висит от прошлой прерванной сессии — снести.
        let _ = fs::remove_file(&tmp);

        match attempt_copy_and_verify(source, &tmp) {
            Ok((source_hash, size)) => {
                if let Err(e) = timestamps::copy_times(source, &tmp) {
                    // Timestamps — best-effort: не откатываем копию из-за них.
                    eprintln!(
                        "  ⚠ не удалось скопировать времена для {}: {e}",
                        dest.display()
                    );
                    report_log(
                        reporter,
                        LogLevel::Warning,
                        format!(
                            "[WARN] Не удалось скопировать времена для {}: {e}",
                            dest.display()
                        ),
                    );
                }
                fs::rename(&tmp, dest).map_err(|e| CopyError::DestinationWrite {
                    path: dest.to_path_buf(),
                    source: e,
                })?;
                cleanup_failed_tmps(&kept_tmps);
                bar.inc(size);
                reporter.report(ProgressEvent::BytesAdvanced(size));
                return Ok(source_hash);
            }
            Err(e) => {
                let class = e.classify();

                if matches!(class, ErrorClass::PersistentDevice) {
                    let _ = fs::remove_file(&tmp);
                    cleanup_failed_tmps(&kept_tmps);
                    return Err(e);
                }

                // Transient: unlimited или до max_retries.
                // PersistentFile: всегда до max_retries (больше попыток source не починит).
                let transient = matches!(class, ErrorClass::Transient);
                let more_allowed = if transient {
                    unlimited_retries || attempt < max_retries
                } else {
                    attempt < max_retries
                };

                if !more_allowed {
                    let _ = fs::remove_file(&tmp);
                    cleanup_failed_tmps(&kept_tmps);
                    break;
                }

                let retry_label = if transient && unlimited_retries {
                    format!("попытка {attempt} (unlimited)")
                } else {
                    format!("попытка {attempt}/{max_retries}")
                };
                report_log(
                    reporter,
                    LogLevel::Retry,
                    format!("[RETRY] {}: {retry_label} не прошла: {e}", dest.display()),
                );

                // В unlimited-режиме удерживаем неудачный .tmp — пусть сектор останется занятым,
                // и FS на следующей попытке выберет другие кластеры.
                if transient && unlimited_retries {
                    kept_tmps.push(tmp);
                } else {
                    let _ = fs::remove_file(&tmp);
                }

                thread::sleep(backoff(attempt));
            }
        }
    }

    Err(CopyError::RetriesExhausted {
        path: dest.to_path_buf(),
        attempts: attempt,
    })
}

/// Одна попытка: запись + cold read-back + проверка хеша.
/// Возвращает хеш source и размер в байтах для прогресс-бара.
fn attempt_copy_and_verify(source: &Path, tmp: &Path) -> Result<(Hash, u64)> {
    let (expected, size) = write_with_pipeline(source, tmp)?;
    let actual = cold_read_hash(tmp)?;
    if actual != expected {
        return Err(CopyError::HashMismatch {
            path: tmp.to_path_buf(),
            written: expected.to_hex(),
            read_back: actual.to_hex(),
        });
    }
    Ok((expected, size))
}

/// Читает файл-источник целиком и считает xxh3-128.
fn hash_source_file(path: &Path) -> Result<Hash> {
    let mut f = File::open(path).map_err(|e| CopyError::SourceRead {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut hasher = Hasher::new();
    let mut buf = IoBuf::new(BLOCK_SIZE);
    loop {
        let n = f.read(&mut buf).map_err(|e| CopyError::SourceRead {
            path: path.to_path_buf(),
            source: e,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finish())
}

/// Копирует содержимое source → tmp через reader/writer pipeline.
///
/// Source читается один раз: reader одновременно считает `xxh3_128` и передаёт
/// заполненные буферы writer-стадии. После `full_sync` файл закрывается, а
/// вызывающий код запускает отдельную cold-read verifier-стадию.
fn write_with_pipeline(source: &Path, tmp: &Path) -> Result<(Hash, u64)> {
    let (pool_tx, pool_rx) = bounded(BUFFER_POOL_SIZE);
    for _ in 0..BUFFER_POOL_SIZE {
        pool_tx
            .send(IoBuf::new(BLOCK_SIZE))
            .expect("buffer pool receiver is alive");
    }

    let (data_tx, data_rx) = bounded(BUFFER_POOL_SIZE);
    let source_for_reader = source.to_path_buf();
    let reader = thread::spawn(move || {
        if let Err(e) = reader_stage_inner(&source_for_reader, &data_tx, &pool_rx) {
            let _ = data_tx.send(ReaderMessage::Failed(e));
        }
    });

    let writer_result = writer_stage(source, tmp, &data_rx, &pool_tx);
    drop(data_rx);
    drop(pool_tx);
    if reader.join().is_err() {
        return Err(CopyError::SourceRead {
            path: source.to_path_buf(),
            source: io::Error::other("reader stage panicked"),
        });
    }
    writer_result
}

fn reader_stage_inner(
    source: &Path,
    data_tx: &Sender<ReaderMessage>,
    pool_rx: &Receiver<IoBuf>,
) -> Result<()> {
    let mut src = File::open(source).map_err(|e| CopyError::SourceRead {
        path: source.to_path_buf(),
        source: e,
    })?;

    let mut hasher = Hasher::new();
    let mut total = 0u64;
    loop {
        let mut buf = pool_rx.recv().map_err(|_| CopyError::SourceRead {
            path: source.to_path_buf(),
            source: io::Error::new(io::ErrorKind::BrokenPipe, "buffer pool closed"),
        })?;
        let n = src.read(&mut buf).map_err(|e| CopyError::SourceRead {
            path: source.to_path_buf(),
            source: e,
        })?;
        if n == 0 {
            send_reader_message(
                data_tx,
                ReaderMessage::Done {
                    hash: hasher.finish(),
                    bytes: total,
                },
                source,
            )?;
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
        send_reader_message(data_tx, ReaderMessage::Chunk(Chunk { buf, len: n }), source)?;
    }
    Ok(())
}

fn send_reader_message(
    data_tx: &Sender<ReaderMessage>,
    msg: ReaderMessage,
    source: &Path,
) -> Result<()> {
    data_tx.send(msg).map_err(|_| CopyError::SourceRead {
        path: source.to_path_buf(),
        source: io::Error::new(io::ErrorKind::BrokenPipe, "writer stage stopped"),
    })
}

fn writer_stage(
    source: &Path,
    tmp: &Path,
    data_rx: &Receiver<ReaderMessage>,
    pool_tx: &Sender<IoBuf>,
) -> Result<(Hash, u64)> {
    // На Windows открываем с FILE_FLAG_WRITE_THROUGH — записи сразу уходят в аппаратный кэш.
    let mut dst = io_flags::open_dest_write(tmp).map_err(|e| CopyError::DestinationWrite {
        path: tmp.to_path_buf(),
        source: e,
    })?;

    let mut written = 0u64;
    loop {
        match data_rx.recv().map_err(|_| CopyError::SourceRead {
            path: source.to_path_buf(),
            source: io::Error::new(io::ErrorKind::BrokenPipe, "reader stage stopped"),
        })? {
            ReaderMessage::Chunk(chunk) => {
                write_chunk(&mut dst, tmp, chunk, pool_tx, &mut written)?;
            }
            ReaderMessage::Done { hash, bytes } => {
                if written != bytes {
                    return Err(CopyError::DestinationWrite {
                        path: tmp.to_path_buf(),
                        source: io::Error::new(
                            io::ErrorKind::WriteZero,
                            format!("written {written} bytes, reader sent {bytes} bytes"),
                        ),
                    });
                }
                io_flags::full_sync(&dst).map_err(|e| CopyError::DestinationWrite {
                    path: tmp.to_path_buf(),
                    source: e,
                })?;
                drop(dst);
                return Ok((hash, written));
            }
            ReaderMessage::Failed(e) => return Err(e),
        }
    }
}

fn write_chunk(
    dst: &mut File,
    tmp: &Path,
    chunk: Chunk,
    pool_tx: &Sender<IoBuf>,
    written: &mut u64,
) -> Result<()> {
    dst.write_all(&chunk.buf[..chunk.len])
        .map_err(|e| CopyError::DestinationWrite {
            path: tmp.to_path_buf(),
            source: e,
        })?;
    *written += chunk.len as u64;
    let _ = pool_tx.send(chunk.buf);
    Ok(())
}

/// Открывает новый handle с cache-bypass, читает файл, возвращает xxh3-128.
fn cold_read_hash(path: &Path) -> Result<Hash> {
    cold_hash_file(path).map_err(|e| CopyError::VerifyRead {
        path: path.to_path_buf(),
        source: e,
    })
}

fn tmp_path_for(dest: &Path, attempt: u32) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(TMP_SUFFIX);
    s.push(format!(".{attempt}"));
    PathBuf::from(s)
}

/// Проверяет, является ли имя файла временным файлом `SafeCopy`.
/// Ловит и legacy-формат (`<name>.safecopy.tmp`), и attempt-numbered
/// (`<name>.safecopy.tmp.<N>`), чтобы `cleanup_stale_tmp` подметал оба.
fn is_safecopy_tmp_name(name: &str) -> bool {
    if name.ends_with(TMP_SUFFIX) {
        return true;
    }
    let Some(dot) = name.rfind('.') else {
        return false;
    };
    let (prefix, num_with_dot) = name.split_at(dot);
    let num = &num_with_dot[1..];
    prefix.ends_with(TMP_SUFFIX) && !num.is_empty() && num.chars().all(|c| c.is_ascii_digit())
}

/// Чистит удержанные неудачные .tmp файлы. Ошибки игнорируем —
/// файл мог быть уже вытеснен, заблокирован или отсутствовать.
fn cleanup_failed_tmps(tmps: &[PathBuf]) {
    for p in tmps {
        let _ = fs::remove_file(p);
    }
}

fn backoff(attempt: u32) -> Duration {
    // 1с, 2с, 4с …
    Duration::from_secs(1u64 << (attempt - 1).min(5))
}

/// Финальный проход: cold re-read всех скопированных файлов и сверка с манифестом.
fn final_reread(
    destination: &Path,
    manifest: &Manifest,
    reporter: &dyn ProgressReporter,
) -> anyhow::Result<()> {
    let total_bytes: u64 = manifest
        .iter()
        .map(|(rel, _)| fs::metadata(destination.join(rel)).map_or(0, |m| m.len()))
        .sum();
    reporter.report(ProgressEvent::TotalBytes(total_bytes));

    let bar = ProgressBar::new(total_bytes);
    bar.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} {msg}",
        )
        .expect("valid template")
        .progress_chars("=>-"),
    );

    let mut mismatches: Vec<PathBuf> = Vec::new();
    for (rel, expected) in manifest.iter() {
        let path = destination.join(rel);
        bar.set_message(rel.display().to_string());
        reporter.report(ProgressEvent::CurrentFile(rel.display().to_string()));
        let size = fs::metadata(&path).map_or(0, |m| m.len());
        match cold_read_hash(&path) {
            Ok(h) if h == *expected => {}
            Ok(h) => {
                eprintln!(
                    "\n✗ {}: хеш расходится (ожидали {expected}, прочитали {h})",
                    rel.display()
                );
                report_log(
                    reporter,
                    LogLevel::Error,
                    format!(
                        "[ERROR] {}: хеш расходится (ожидали {expected}, прочитали {h})",
                        rel.display()
                    ),
                );
                mismatches.push(rel.clone());
            }
            Err(e) => {
                eprintln!("\n✗ {}: ошибка чтения: {e}", rel.display());
                report_log(
                    reporter,
                    LogLevel::Error,
                    format!("[ERROR] {}: ошибка чтения: {e}", rel.display()),
                );
                mismatches.push(rel.clone());
            }
        }
        bar.inc(size);
        reporter.report(ProgressEvent::BytesAdvanced(size));
    }
    bar.finish_with_message("верификация завершена");

    if !mismatches.is_empty() {
        bail!(
            "финальная верификация не прошла для {} файлов — карта, видимо, теряет данные после записи",
            mismatches.len()
        );
    }
    Ok(())
}

/// Удаляет зависшие .tmp файлы, оставшиеся от прерванного предыдущего запуска.
/// Ошибки игнорируются — файл мог уже быть удалён или быть занят.
fn cleanup_stale_tmp(dir: &Path) {
    for entry in WalkDir::new(dir).follow_links(false) {
        let Ok(e) = entry else { continue };
        if e.file_type().is_file() && is_safecopy_tmp_name(&e.file_name().to_string_lossy()) {
            let _ = fs::remove_file(e.path());
        }
    }
}

fn write_manifest_artifacts(destination: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    let mpath = destination.join(MANIFEST_FILENAME);
    manifest.write_to(&mpath).context("запись manifest.xxh3")?;
    let rpath = destination.join(README_FILENAME);
    let mut f = File::create(&rpath)?;
    f.write_all(README_CONTENT.as_bytes())?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::cli::{CopyOpts, VerifyOpts};
    use crate::verify;

    use super::*;

    struct TempTree(PathBuf);

    impl TempTree {
        fn new(name: &str) -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before UNIX_EPOCH")
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("safecopy-{name}-{}-{stamp}", std::process::id()));
            fs::create_dir_all(&path).expect("create temp tree");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn resume_rewrites_corrupted_destination_file() {
        let tree = TempTree::new("resume-corrupt");
        let src = tree.path().join("src");
        let dst = tree.path().join("dst");
        fs::create_dir_all(src.join("nested")).expect("create source");
        fs::create_dir_all(&dst).expect("create destination");
        fs::write(src.join("small.txt"), b"hello safecopy").expect("write source file");
        fs::write(src.join("nested").join("other.txt"), b"another file").expect("write nested");

        let opts = CopyOpts {
            source: src.clone(),
            destination: dst.clone(),
            cooldown_secs: 0,
            no_manifest_on_card: false,
            max_retries: 3,
            unlimited_retries: false,
        };
        super::run(&opts).expect("initial copy");

        fs::write(dst.join("small.txt"), b"corrupted on card").expect("corrupt destination");
        super::run(&opts).expect("resume should repair destination");

        let repaired = fs::read_to_string(dst.join("small.txt")).expect("read repaired file");
        assert_eq!(repaired, "hello safecopy");
        verify::run(&VerifyOpts { target: dst }).expect("verify repaired destination");
    }

    #[test]
    fn single_file_copy_places_file_under_destination_with_manifest() {
        let tree = TempTree::new("single-file");
        let src_dir = tree.path().join("src");
        let dst = tree.path().join("dst");
        fs::create_dir_all(&src_dir).expect("create source dir");
        fs::create_dir_all(&dst).expect("create destination");
        let src_file = src_dir.join("photo.raw");
        fs::write(&src_file, b"single-file payload").expect("write source file");

        let opts = CopyOpts {
            source: src_file.clone(),
            destination: dst.clone(),
            cooldown_secs: 0,
            no_manifest_on_card: false,
            max_retries: 3,
            unlimited_retries: false,
        };
        super::run(&opts).expect("single-file copy");

        let copied = fs::read(dst.join("photo.raw")).expect("read copied file");
        assert_eq!(copied, b"single-file payload");
        assert!(
            dst.join(MANIFEST_FILENAME).is_file(),
            "manifest должен лежать рядом с файлом"
        );
        verify::run(&VerifyOpts { target: dst }).expect("verify single-file destination");
    }

    #[test]
    fn manifest_name_collision_aborts_when_manifest_enabled() {
        let tree = TempTree::new("manifest-collision");
        let src_dir = tree.path().join("src");
        let dst = tree.path().join("dst");
        fs::create_dir_all(&src_dir).expect("create source dir");
        fs::create_dir_all(&dst).expect("create destination");
        // Имя файла совпадает с артефактом манифеста — копировать с записью манифеста нельзя.
        fs::write(
            src_dir.join(MANIFEST_FILENAME),
            b"user payload, not a manifest",
        )
        .expect("write colliding file");

        let opts = CopyOpts {
            source: src_dir.clone(),
            destination: dst.clone(),
            cooldown_secs: 0,
            no_manifest_on_card: false,
            max_retries: 3,
            unlimited_retries: false,
        };
        let err = super::run(&opts).expect_err("должно упасть из-за конфликта имени");
        assert!(
            err.to_string().contains("--no-manifest-on-card"),
            "сообщение должно подсказывать решение, было: {err}"
        );

        // С отключённым манифестом тот же запуск должен пройти.
        let opts_no_manifest = CopyOpts {
            no_manifest_on_card: true,
            ..opts
        };
        super::run(&opts_no_manifest).expect("копирование без манифеста проходит");
        let copied = fs::read(dst.join(MANIFEST_FILENAME)).expect("read copied file");
        assert_eq!(copied, b"user payload, not a manifest");

        // Повторный запуск не должен пытаться парсить пользовательский manifest.xxh3 как метаданные.
        super::run(&opts_no_manifest).expect("повторный no-manifest запуск тоже проходит");
        let copied_again = fs::read(dst.join(MANIFEST_FILENAME)).expect("read copied file again");
        assert_eq!(copied_again, b"user payload, not a manifest");
    }
}
