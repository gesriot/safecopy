use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "safecopy",
    about = "Надёжное копирование файлов на SD-карту с верификацией",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Открыть графический интерфейс
    Gui,
    /// Копировать файл или папку на карту с проверкой целостности
    Copy(CopyOpts),
    /// Проверить файлы на карте по manifest.xxh3
    Verify(VerifyOpts),
}

#[derive(Args, Debug)]
pub struct CopyOpts {
    /// Исходный файл или папка (источник надёжен)
    pub source: PathBuf,
    /// Папка назначения на SD-карте
    pub destination: PathBuf,
    /// Сколько секунд ждать перед финальной cold-read верификацией
    #[arg(long, default_value_t = 45)]
    pub cooldown_secs: u64,
    /// Не записывать manifest.xxh3 на карту.
    /// Включено по умолчанию; вернуть манифест: --no-manifest-on-card=false
    #[arg(
        long,
        default_value_t = true,
        num_args = 0..=1,
        default_missing_value = "true",
        action = clap::ArgAction::Set
    )]
    pub no_manifest_on_card: bool,
    /// Максимум попыток копирования одного файла
    #[arg(long, default_value_t = 3)]
    pub max_retries: u32,
    /// Не ограничивать число попыток для transient-ошибок записи/верификации:
    /// копировать, пока файл не запишется корректно или пока не кончится место.
    /// Неудачные .tmp остаются на карте — FS выдаёт следующей попытке другие сектора.
    /// Включено по умолчанию; ограничить попытки: --unlimited-retries=false
    #[arg(
        long,
        default_value_t = true,
        num_args = 0..=1,
        default_missing_value = "true",
        action = clap::ArgAction::Set
    )]
    pub unlimited_retries: bool,
    /// Не копировать файлы, игнорируемые по правилам .gitignore внутри источника
    #[arg(long)]
    pub respect_gitignore: bool,
}

#[derive(Args, Debug)]
pub struct VerifyOpts {
    /// Путь к папке с manifest.xxh3 или к самому файлу манифеста
    pub target: PathBuf,
}
