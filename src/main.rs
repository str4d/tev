use clap::Parser;

mod cli;
mod commands;
mod formats;

fn main() -> anyhow::Result<()> {
    let opts = cli::Options::parse();

    match opts.command {
        cli::Command::Inspect(command) => command.run(),
        cli::Command::VerifyBackup(command) => command.run(),
    }
}
