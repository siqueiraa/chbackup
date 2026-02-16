mod cli;
mod error;

pub use error::ChBackupError;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Create { backup_name, .. } => {
            println!(
                "create: not implemented yet (backup_name={:?})",
                backup_name
            );
        }
        Command::Upload { backup_name, .. } => {
            println!(
                "upload: not implemented yet (backup_name={:?})",
                backup_name
            );
        }
        Command::Download { backup_name, .. } => {
            println!(
                "download: not implemented yet (backup_name={:?})",
                backup_name
            );
        }
        Command::Restore { backup_name, .. } => {
            println!(
                "restore: not implemented yet (backup_name={:?})",
                backup_name
            );
        }
        Command::CreateRemote { backup_name, .. } => {
            println!(
                "create_remote: not implemented yet (backup_name={:?})",
                backup_name
            );
        }
        Command::RestoreRemote { backup_name, .. } => {
            println!(
                "restore_remote: not implemented yet (backup_name={:?})",
                backup_name
            );
        }
        Command::List { location } => {
            println!("list: not implemented yet (location={:?})", location);
        }
        Command::Tables { .. } => {
            println!("tables: not implemented yet");
        }
        Command::Delete {
            location,
            backup_name,
        } => {
            println!(
                "delete: not implemented yet (location={:?}, backup_name={:?})",
                location, backup_name
            );
        }
        Command::Clean { name } => {
            println!("clean: not implemented yet (name={:?})", name);
        }
        Command::CleanBroken { location } => {
            println!(
                "clean_broken: not implemented yet (location={:?})",
                location
            );
        }
        Command::DefaultConfig => {
            println!("default-config: not implemented yet");
        }
        Command::PrintConfig => {
            println!("print-config: not implemented yet");
        }
        Command::Watch { .. } => {
            println!("watch: not implemented yet");
        }
        Command::Server { watch } => {
            println!("server: not implemented yet (watch={watch})");
        }
    }

    Ok(())
}
