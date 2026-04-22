//! Карантин для файлов, которые не удалось скопировать успешно.
//!
//! Мы не пытаемся бороться с «зависшими» .tmp файлами — это задача получателя
//! или следующего запуска. Мы только фиксируем факт неудачи в `.quarantine/`,
//! чтобы пользователь увидел список проблем, не останавливая основной pipeline.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Result;

pub const QUARANTINE_DIR: &str = ".quarantine";

pub struct QuarantineReport<'a> {
    pub source_relative: &'a Path,
    pub reason: &'a str,
    pub attempts: u32,
}

pub fn record(destination_root: &Path, report: &QuarantineReport<'_>) -> Result<PathBuf> {
    let qdir = destination_root.join(QUARANTINE_DIR);
    fs::create_dir_all(&qdir)?;

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    // Включаем ts в имя файла → коллизий нет даже при повторном запуске.
    // Заменяем символы, недопустимые в именах файлов Windows/exFAT.
    let safe_name = report
        .source_relative
        .to_string_lossy()
        .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "__");
    let filename = format!("{safe_name}.{ts}.failed.json");
    let path = qdir.join(&filename);

    let mut f = fs::File::create(&path)?;
    // Простой JSON без внешних зависимостей; поля экранируем вручную.
    writeln!(f, "{{")?;
    writeln!(
        f,
        "  \"source_relative\": \"{}\",",
        json_escape(&report.source_relative.to_string_lossy())
    )?;
    writeln!(f, "  \"attempts\": {}," , report.attempts)?;
    writeln!(f, "  \"unix_time\": {ts},")?;
    writeln!(f, "  \"reason\": \"{}\"", json_escape(report.reason))?;
    writeln!(f, "}}")?;
    f.sync_all()?;
    Ok(path)
}

/// Экранирует строку для встраивания в JSON-строку (RFC 8259 §7).
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}
