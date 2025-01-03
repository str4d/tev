use clap::Parser;
use tokio::runtime::Builder;

mod cli;
mod commands;
mod formats;

fn main() -> anyhow::Result<()> {
    let opts = cli::Options::parse();

    match opts.command {
        cli::Command::Inspect(command) => command.run(),
        cli::Command::Backup(cli::Backup::Verify(command)) => {
            let runtime = Builder::new_multi_thread()
                .thread_name("tev-worker")
                .build()?;
            runtime.block_on(command.run())
        }
        cli::Command::Backup(cli::Backup::Mount(command)) => command.run(),
    }
}
