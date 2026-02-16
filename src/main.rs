mod cli;

use anyhow::Result;
use chbackup::clickhouse::ChClient;
use chbackup::config::Config;
use chbackup::lock::{lock_for_command, lock_path_for_scope, PidLock};
use chbackup::logging;
use chbackup::storage::S3Client;
use clap::Parser;
use cli::{Cli, Command};
use std::path::Path;
use tracing::info;

/// Extract the command name (as used by [`lock_for_command`]) from a [`Command`].
fn command_name(cmd: &Command) -> &'static str {
    match cmd {
        Command::Create { .. } => "create",
        Command::Upload { .. } => "upload",
        Command::Download { .. } => "download",
        Command::Restore { .. } => "restore",
        Command::CreateRemote { .. } => "create_remote",
        Command::RestoreRemote { .. } => "restore_remote",
        Command::List { .. } => "list",
        Command::Tables { .. } => "tables",
        Command::Delete { .. } => "delete",
        Command::Clean { .. } => "clean",
        Command::CleanBroken { .. } => "clean_broken",
        Command::DefaultConfig => "default-config",
        Command::PrintConfig => "print-config",
        Command::Watch { .. } => "watch",
        Command::Server { .. } => "server",
    }
}

/// Extract the optional backup name from a [`Command`], if applicable.
fn backup_name_from_command(cmd: &Command) -> Option<&str> {
    match cmd {
        Command::Create { backup_name, .. }
        | Command::Upload { backup_name, .. }
        | Command::Download { backup_name, .. }
        | Command::Restore { backup_name, .. }
        | Command::CreateRemote { backup_name, .. }
        | Command::RestoreRemote { backup_name, .. } => backup_name.as_deref(),
        Command::Delete { backup_name, .. } => backup_name.as_deref(),
        _ => None,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // -----------------------------------------------------------------------
    // default-config and print-config are special: they run before logging
    // and config loading because their purpose IS to print the config.
    // -----------------------------------------------------------------------
    match &cli.command {
        Command::DefaultConfig => {
            let yaml = Config::default_yaml()?;
            print!("{yaml}");
            return Ok(());
        }
        Command::PrintConfig => {
            let config_path = Path::new(&cli.config);
            let config = Config::load(config_path, &cli.env_overrides)?;
            let yaml = serde_yaml::to_string(&config)?;
            print!("{yaml}");
            return Ok(());
        }
        _ => {}
    }

    // -----------------------------------------------------------------------
    // Full flow: Load config -> Init logging -> Acquire lock -> Execute -> Release
    // -----------------------------------------------------------------------

    // 1. Load config (with env overlay and CLI --env overrides).
    let config_path = Path::new(&cli.config);
    let config = Config::load(config_path, &cli.env_overrides)?;

    // 2. Init logging.
    let is_server = matches!(&cli.command, Command::Server { .. });
    logging::init_logging(
        &config.general.log_format,
        &config.general.log_level,
        is_server,
    );

    // 3. Acquire lock based on command scope.
    let cmd_name = command_name(&cli.command);
    let bak_name = backup_name_from_command(&cli.command);
    let scope = lock_for_command(cmd_name, bak_name);
    let lock_file_path = lock_path_for_scope(&scope);

    let _lock_guard: Option<PidLock> = match lock_file_path {
        Some(ref path) => {
            info!(
                command = cmd_name,
                lock_path = %path.display(),
                "Acquiring lock"
            );
            let guard = PidLock::acquire(path, cmd_name)?;
            info!("Lock acquired");
            Some(guard)
        }
        None => {
            info!(command = cmd_name, "No lock required");
            None
        }
    };

    // 4. Execute command.
    match cli.command {
        Command::Create { backup_name, .. } => {
            info!(backup_name = ?backup_name, "create: not implemented yet");
        }
        Command::Upload { backup_name, .. } => {
            info!(backup_name = ?backup_name, "upload: not implemented yet");
        }
        Command::Download { backup_name, .. } => {
            info!(backup_name = ?backup_name, "download: not implemented yet");
        }
        Command::Restore { backup_name, .. } => {
            info!(backup_name = ?backup_name, "restore: not implemented yet");
        }
        Command::CreateRemote { backup_name, .. } => {
            info!(backup_name = ?backup_name, "create_remote: not implemented yet");
        }
        Command::RestoreRemote { backup_name, .. } => {
            info!(backup_name = ?backup_name, "restore_remote: not implemented yet");
        }
        Command::List { location } => {
            info!(location = ?location, "Connecting to ClickHouse");
            let ch = ChClient::new(&config.clickhouse)?;
            match ch.ping().await {
                Ok(()) => info!("ClickHouse connection OK"),
                Err(e) => info!(error = %e, "ClickHouse connection failed"),
            }

            info!(location = ?location, "Connecting to S3");
            let s3 = S3Client::new(&config.s3).await?;
            match s3.ping().await {
                Ok(()) => info!("S3 connection OK"),
                Err(e) => info!(error = %e, "S3 connection failed"),
            }

            info!("list: not implemented yet");
        }
        Command::Tables { .. } => {
            info!("tables: not implemented yet");
        }
        Command::Delete {
            location,
            backup_name,
        } => {
            info!(location = ?location, backup_name = ?backup_name, "delete: not implemented yet");
        }
        Command::Clean { name } => {
            info!(name = ?name, "clean: not implemented yet");
        }
        Command::CleanBroken { location } => {
            info!(location = ?location, "clean_broken: not implemented yet");
        }
        Command::Watch { .. } => {
            info!("watch: not implemented yet");
        }
        Command::Server { watch } => {
            info!(watch = watch, "server: not implemented yet");
        }
        // default-config and print-config handled above (early return).
        Command::DefaultConfig | Command::PrintConfig => unreachable!(),
    }

    // 5. Lock is released automatically when _lock_guard is dropped.
    info!(command = cmd_name, "Command complete");

    Ok(())
}
