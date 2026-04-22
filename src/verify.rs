//! Standalone режим верификации: читаем manifest.xxh3 и прогоняем cold re-read.

use std::path::PathBuf;

use anyhow::{bail, Context};
use indicatif::{ProgressBar, ProgressStyle};

use crate::cli::VerifyOpts;
use crate::error::CopyError;
use crate::hash::{cold_hash_file, Hash};
use crate::manifest::{resolve_manifest_path, Manifest};
use crate::progress::{LogLevel, NoopReporter, ProgressEvent, ProgressPhase, ProgressReporter};

pub fn run(opts: &VerifyOpts) -> anyhow::Result<()> {
    run_with_reporter(opts, &NoopReporter)
}

pub fn run_with_reporter(
    opts: &VerifyOpts,
    reporter: &dyn ProgressReporter,
) -> anyhow::Result<()> {
    let target = load_verify_target(opts)?;
    announce_verify(&target, reporter);

    let problems = verify_manifest(&target.root, &target.manifest, reporter);
    report_problems(&problems);

    let problem_count = problems.len();
    if problem_count == 0 {
        println!("✓ все {} файлов целы", target.manifest.len());
        reporter.report(ProgressEvent::Phase(ProgressPhase::Done));
        report_log(
            reporter,
            LogLevel::Success,
            format!("Все {} файлов целы", target.manifest.len()),
        );
        Ok(())
    } else {
        bail!(
            "проверка провалилась: {problem_count} файлов с проблемами из {}",
            target.manifest.len()
        );
    }
}

struct VerifyTarget {
    root: PathBuf,
    manifest: Manifest,
}

#[derive(Default)]
struct VerifyProblems {
    missing: Vec<PathBuf>,
    mismatched: Vec<PathBuf>,
    unreadable: Vec<(PathBuf, CopyError)>,
}

impl VerifyProblems {
    fn len(&self) -> usize {
        self.missing.len() + self.mismatched.len() + self.unreadable.len()
    }
}

fn load_verify_target(opts: &VerifyOpts) -> anyhow::Result<VerifyTarget> {
    let manifest_path = resolve_manifest_path(&opts.target);
    let root = manifest_path
        .parent()
        .context("manifest должен быть в папке")?
        .to_path_buf();

    let manifest = Manifest::read_from(&manifest_path)
        .with_context(|| format!("чтение {}", manifest_path.display()))?;
    if manifest.is_empty() {
        bail!("манифест пуст");
    }
    Ok(VerifyTarget { root, manifest })
}

fn announce_verify(target: &VerifyTarget, reporter: &dyn ProgressReporter) {
    println!(
        "Проверяем {} файлов в {}…",
        target.manifest.len(),
        target.root.display()
    );
    reporter.report(ProgressEvent::Phase(ProgressPhase::Verifying));
    report_log(
        reporter,
        LogLevel::Info,
        format!(
            "Проверяем {} файлов в {}",
            target.manifest.len(),
            target.root.display()
        ),
    );
}

fn verify_manifest(
    root: &std::path::Path,
    manifest: &Manifest,
    reporter: &dyn ProgressReporter,
) -> VerifyProblems {
    let total_bytes: u64 = manifest
        .iter()
        .map(|(rel, _)| std::fs::metadata(root.join(rel)).map_or(0, |m| m.len()))
        .sum();
    reporter.report(ProgressEvent::TotalBytes(total_bytes));

    let bar = verify_progress_bar(total_bytes);
    let mut problems = VerifyProblems::default();

    for (rel, expected) in manifest.iter() {
        let path = root.join(rel);
        bar.set_message(rel.display().to_string());
        reporter.report(ProgressEvent::CurrentFile(rel.display().to_string()));
        let size = std::fs::metadata(&path).map_or(0, |m| m.len());

        if !path.exists() {
            problems.missing.push(rel.clone());
            bar.inc(size);
            reporter.report(ProgressEvent::BytesAdvanced(size));
            report_log(
                reporter,
                LogLevel::Error,
                format!("[MISSING] {}", rel.display()),
            );
            continue;
        }

        match cold_hash(&path) {
            Ok(actual) if actual == *expected => {}
            Ok(_) => {
                report_log(
                    reporter,
                    LogLevel::Error,
                    format!("[DAMAGED] {}", rel.display()),
                );
                problems.mismatched.push(rel.clone());
            }
            Err(e) => {
                report_log(
                    reporter,
                    LogLevel::Error,
                    format!("[UNREADABLE] {}: {e}", rel.display()),
                );
                problems.unreadable.push((rel.clone(), e));
            }
        }
        bar.inc(size);
        reporter.report(ProgressEvent::BytesAdvanced(size));
    }

    bar.finish_with_message("готово");
    problems
}

fn verify_progress_bar(total_bytes: u64) -> ProgressBar {
    let bar = ProgressBar::new(total_bytes);
    bar.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}) {msg}",
        )
        .expect("valid template")
        .progress_chars("=>-"),
    );
    bar
}

fn report_problems(problems: &VerifyProblems) {
    if !problems.missing.is_empty() {
        eprintln!("\nОтсутствуют ({}):", problems.missing.len());
        for p in &problems.missing {
            eprintln!("  - {}", p.display());
        }
    }
    if !problems.mismatched.is_empty() {
        eprintln!("\nПовреждены ({}):", problems.mismatched.len());
        for p in &problems.mismatched {
            eprintln!("  - {}", p.display());
        }
    }
    if !problems.unreadable.is_empty() {
        eprintln!("\nНе читаются ({}):", problems.unreadable.len());
        for (p, e) in &problems.unreadable {
            eprintln!("  - {}: {e}", p.display());
        }
    }
}

fn report_log(
    reporter: &dyn ProgressReporter,
    level: LogLevel,
    message: impl Into<String>,
) {
    reporter.report(ProgressEvent::Log {
        level,
        message: message.into(),
    });
}

fn cold_hash(path: &std::path::Path) -> Result<Hash, CopyError> {
    cold_hash_file(path).map_err(|e| CopyError::VerifyRead {
        path: path.to_path_buf(),
        source: e,
    })
}
