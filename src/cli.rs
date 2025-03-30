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
    #[command(subcommand)]
    Backup(Backup),
}

/// Inspect a Steam file.
#[derive(Debug, Args)]
pub(crate) struct Inspect {
    /// Path to the file.
    pub(crate) path: PathBuf,
}

/// Manage Steam game backups.
#[derive(Debug, Subcommand)]
pub(crate) enum Backup {
    Verify(VerifyBackup),
    Mount(MountBackup),
}

/// Verify a Steam game backup.
///
/// If `--manifest-dir` is provided, it will be checked for the presence of the manifest
/// files necessary to access the files in this backup.
#[derive(Debug, Args)]
pub(crate) struct VerifyBackup {
    /// Path to the game's backup folder, or a file within it.
    pub(crate) path: PathBuf,

    /// Path to the folder containing the user's cached manifest files.
    #[arg(long)]
    pub(crate) manifest_dir: Option<PathBuf>,
}

/// Mount a Steam game backup.
#[derive(Debug, Args)]
pub(crate) struct MountBackup {
    /// Path to the game's backup folder, or a file within it.
    pub(crate) path: PathBuf,

    /// Path to the game's backup folder, or a file within it.
    pub(crate) mountpoint: PathBuf,

    /// Path to the folder containing the user's cached manifest files.
    #[arg(long)]
    pub(crate) manifest_dir: PathBuf,
}
