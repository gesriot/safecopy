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
    /// Копировать папку на карту с проверкой целостности
    Copy(CopyOpts),
    /// Проверить файлы на карте по manifest.xxh3
    Verify(VerifyOpts),
}

#[derive(Args, Debug)]
pub struct CopyOpts {
    /// Исходная папка (источник надёжен)
    pub source: PathBuf,
    /// Папка назначения на SD-карте
    pub destination: PathBuf,
    /// Сколько секунд ждать перед финальной cold-read верификацией
    #[arg(long, default_value_t = 45)]
    pub cooldown_secs: u64,
    /// Не записывать manifest.xxh3 на карту
    #[arg(long)]
    pub no_manifest_on_card: bool,
    /// Максимум попыток копирования одного файла
    #[arg(long, default_value_t = 3)]
    pub max_retries: u32,
}

#[derive(Args, Debug)]
pub struct VerifyOpts {
    /// Путь к папке с manifest.xxh3 или к самому файлу манифеста
    pub target: PathBuf,
}
