//! Локальное состояние приложения: настройки GUI и checkpoint'ы копирования.
//!
//! Расположение зависит от платформы:
//! * Windows — portable-режим: каталог `safecopy-state` рядом с exe.
//! * macOS — `~/Library/Application Support/SafeCopy` (полноценный .app).
//! * Прочий Unix — `$XDG_DATA_HOME/safecopy` или `~/.local/share/safecopy`.
//!
//! Переменная окружения `SAFECOPY_STATE_DIR` переопределяет расположение
//! (используется тестами; полезна и для portable-сценариев на Unix).
//!
//! Всё состояние — best-effort: недоступность каталога не должна ломать
//! копирование, только отключать resume/персистентность настроек.

use std::fs;
use std::path::{Path, PathBuf};

use crate::hash::Hasher;

// Независимые флаги-настройки, а не state machine.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct Settings {
    pub(crate) cooldown_secs: u64,
    pub(crate) max_retries: u32,
    pub(crate) unlimited_retries: bool,
    pub(crate) no_manifest_on_card: bool,
    pub(crate) respect_gitignore: bool,
    pub(crate) skip_junk: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cooldown_secs: 45,
            max_retries: 3,
            unlimited_retries: true,
            no_manifest_on_card: true,
            respect_gitignore: false,
            skip_junk: false,
        }
    }
}

pub(crate) fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("SAFECOPY_STATE_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    platform_state_dir()
}

#[cfg(windows)]
fn platform_state_dir() -> Option<PathBuf> {
    // Portable: состояние живёт рядом с exe и переезжает вместе с ним.
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("safecopy-state"))
}

#[cfg(target_os = "macos")]
fn platform_state_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/SafeCopy"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_state_dir() -> Option<PathBuf> {
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        if !data.is_empty() {
            return Some(PathBuf::from(data).join("safecopy"));
        }
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/share/safecopy"))
}

fn settings_path() -> Option<PathBuf> {
    Some(state_dir()?.join("settings.conf"))
}

pub(crate) fn load_settings() -> Settings {
    let mut settings = Settings::default();
    let Some(path) = settings_path() else {
        return settings;
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return settings;
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "cooldown_secs" => {
                if let Ok(v) = value.parse() {
                    settings.cooldown_secs = v;
                }
            }
            "max_retries" => {
                if let Ok(v) = value.parse() {
                    settings.max_retries = v;
                }
            }
            "unlimited_retries" => {
                if let Ok(v) = value.parse() {
                    settings.unlimited_retries = v;
                }
            }
            "no_manifest_on_card" => {
                if let Ok(v) = value.parse() {
                    settings.no_manifest_on_card = v;
                }
            }
            "respect_gitignore" => {
                if let Ok(v) = value.parse() {
                    settings.respect_gitignore = v;
                }
            }
            "skip_junk" => {
                if let Ok(v) = value.parse() {
                    settings.skip_junk = v;
                }
            }
            _ => {}
        }
    }
    settings
}

pub(crate) fn save_settings(settings: &Settings) {
    let Some(path) = settings_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let text = format!(
        "cooldown_secs={}\nmax_retries={}\nunlimited_retries={}\nno_manifest_on_card={}\nrespect_gitignore={}\nskip_junk={}\n",
        settings.cooldown_secs,
        settings.max_retries,
        settings.unlimited_retries,
        settings.no_manifest_on_card,
        settings.respect_gitignore,
        settings.skip_junk,
    );
    let _ = fs::write(&path, text);
}

/// Путь к checkpoint-файлу для пары (source, destination).
///
/// Checkpoint хранит манифест успешно записанных и проверенных файлов в формате
/// `manifest.xxh3`, что даёт resume даже с включённым «Без манифеста на карте».
/// `None` — каталог состояния недоступен, resume просто не будет.
pub(crate) fn checkpoint_path(source: &Path, destination: &Path) -> Option<PathBuf> {
    let dir = state_dir()?.join("checkpoints");
    fs::create_dir_all(&dir).ok()?;
    let mut hasher = Hasher::new();
    hasher.update(source.as_os_str().as_encoded_bytes());
    hasher.update(b"\0");
    hasher.update(destination.as_os_str().as_encoded_bytes());
    Some(dir.join(format!("{}.xxh3", hasher.finish().to_hex())))
}
