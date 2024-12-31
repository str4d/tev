use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
pub(crate) struct Options {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    Inspect(Inspect),
    VerifyBackup(VerifyBackup),
}

/// Inspect a Steam file.
#[derive(Debug, Args)]
pub(crate) struct Inspect {
    /// Path to the file.
    pub(crate) path: PathBuf,
}

/// Verify a Steam game backup.
#[derive(Debug, Args)]
pub(crate) struct VerifyBackup {
    /// Path to the game's backup folder, or a file within it.
    pub(crate) path: PathBuf,
}
