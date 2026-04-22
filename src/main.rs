#![windows_subsystem = "windows"]

mod cli;
mod copy;
mod error;
mod gui;
mod hash;
mod io_flags;
mod manifest;
mod progress;
mod quarantine;
mod sanity;
mod timestamps;
mod verify;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let args = cli::Cli::parse();
    match args.command.unwrap_or(cli::Command::Gui) {
        cli::Command::Copy(opts) => copy::run(&opts),
        cli::Command::Verify(opts) => verify::run(&opts),
        cli::Command::Gui => gui::run(),
    }
}
