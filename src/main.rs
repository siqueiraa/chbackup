mod cli;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Result};
use chbackup::clickhouse::ChClient;
use chbackup::config::Config;
use chbackup::lock::{lock_for_command, lock_path_for_scope, PidLock};
use chbackup::logging;
use chbackup::storage::S3Client;
use chbackup::{backup, download, list, restore, upload};
use chrono::Utc;
use clap::Parser;
use cli::{Cli, Command};
use tracing::{info, warn};

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
        Command::Create {
            tables,
            partitions,
            diff_from,
            skip_projections,
            schema,
            rbac,
            configs,
            named_collections,
            skip_check_parts_columns,
            resume,
            backup_name,
        } => {
            // Warn about Phase 2+ flags that are not yet implemented
            if skip_projections.is_some() {
                warn!("--skip-projections flag is not yet implemented, ignoring");
            }
            if rbac {
                warn!("--rbac flag is not yet implemented, ignoring");
            }
            if configs {
                warn!("--configs flag is not yet implemented, ignoring");
            }
            if named_collections {
                warn!("--named-collections flag is not yet implemented, ignoring");
            }
            let name = resolve_backup_name(backup_name);
            let ch = ChClient::new(&config.clickhouse)?;

            // Note: --resume is not applicable to `create` (backup is local-only,
            // no resume state tracking needed). The flag is accepted but ignored.
            if resume {
                info!("--resume flag has no effect on the create command");
            }

            let _manifest = backup::create(
                &config,
                &ch,
                &name,
                tables.as_deref(),
                schema,
                diff_from.as_deref(),
                partitions.as_deref(),
                skip_check_parts_columns,
            )
            .await?;

            info!(backup_name = %name, "Create command complete");
        }

        Command::Upload {
            delete_local,
            diff_from_remote,
            resume,
            backup_name,
        } => {
            let name = backup_name_required(backup_name, "upload")?;
            let s3 = S3Client::new(&config.s3).await?;

            let backup_dir = PathBuf::from(&config.clickhouse.data_path)
                .join("backup")
                .join(&name);

            let effective_resume = resume && config.general.use_resumable_state;
            upload::upload(
                &config,
                &s3,
                &name,
                &backup_dir,
                delete_local,
                diff_from_remote.as_deref(),
                effective_resume,
            )
            .await?;

            info!(backup_name = %name, "Upload command complete");
        }

        Command::Download {
            hardlink_exists_files,
            resume,
            backup_name,
        } => {
            if hardlink_exists_files {
                warn!("--hardlink-exists-files flag is not yet implemented, ignoring");
            }

            let name = backup_name_required(backup_name, "download")?;
            let s3 = S3Client::new(&config.s3).await?;

            let effective_resume = resume && config.general.use_resumable_state;
            let backup_dir = download::download(&config, &s3, &name, effective_resume).await?;

            info!(
                backup_name = %name,
                backup_dir = %backup_dir.display(),
                "Download command complete"
            );
        }

        Command::Restore {
            tables,
            rename_as,
            database_mapping,
            partitions,
            schema,
            data_only,
            rm,
            resume,
            rbac,
            configs,
            named_collections,
            skip_empty_tables,
            backup_name,
        } => {
            // Warn about flags not yet implemented
            if rename_as.is_some() {
                warn!("--as flag is not yet implemented, ignoring");
            }
            if database_mapping.is_some() {
                warn!("--database-mapping flag is not yet implemented, ignoring");
            }
            if partitions.is_some() {
                warn!("--partitions flag is not yet implemented for restore, ignoring");
            }
            if rm {
                warn!("--rm flag is not yet implemented, ignoring");
            }
            if rbac {
                warn!("--rbac flag is not yet implemented, ignoring");
            }
            if configs {
                warn!("--configs flag is not yet implemented, ignoring");
            }
            if named_collections {
                warn!("--named-collections flag is not yet implemented, ignoring");
            }
            if skip_empty_tables {
                warn!("--skip-empty-tables flag is not yet implemented, ignoring");
            }

            let name = backup_name_required(backup_name, "restore")?;
            let ch = ChClient::new(&config.clickhouse)?;

            let effective_resume = resume && config.general.use_resumable_state;
            restore::restore(
                &config,
                &ch,
                &name,
                tables.as_deref(),
                schema,
                data_only,
                effective_resume,
            )
            .await?;

            info!(backup_name = %name, "Restore command complete");
        }

        Command::CreateRemote {
            tables,
            diff_from_remote,
            delete_source,
            rbac,
            configs,
            named_collections,
            skip_check_parts_columns,
            skip_projections,
            resume,
            backup_name,
        } => {
            // Warn about unimplemented flags
            if rbac {
                warn!("--rbac flag is not yet implemented, ignoring");
            }
            if configs {
                warn!("--configs flag is not yet implemented, ignoring");
            }
            if named_collections {
                warn!("--named-collections flag is not yet implemented, ignoring");
            }
            if skip_projections.is_some() {
                warn!("--skip-projections flag is not yet implemented, ignoring");
            }

            let name = resolve_backup_name(backup_name);
            let ch = ChClient::new(&config.clickhouse)?;
            let s3 = S3Client::new(&config.s3).await?;

            // Step 1: Create local backup (no local diff-from for create_remote)
            let _manifest = backup::create(
                &config,
                &ch,
                &name,
                tables.as_deref(),
                false, // schema_only
                None,  // diff_from (create_remote uses diff_from_remote on upload side)
                None,  // partitions (create_remote doesn't support --partitions)
                skip_check_parts_columns,
            )
            .await?;

            // Step 2: Upload to S3 (with optional diff-from-remote)
            let backup_dir = PathBuf::from(&config.clickhouse.data_path)
                .join("backup")
                .join(&name);

            let effective_resume = resume && config.general.use_resumable_state;
            upload::upload(
                &config,
                &s3,
                &name,
                &backup_dir,
                delete_source,
                diff_from_remote.as_deref(),
                effective_resume,
            )
            .await?;

            info!(backup_name = %name, "CreateRemote command complete");
        }

        Command::RestoreRemote { backup_name, .. } => {
            info!(backup_name = ?backup_name, "restore_remote: not implemented in Phase 1");
        }

        Command::List { location } => {
            let s3 = S3Client::new(&config.s3).await?;
            let loc = location.map(map_cli_location);

            list::list(&config.clickhouse.data_path, &s3, loc.as_ref()).await?;

            info!("List command complete");
        }

        Command::Tables { .. } => {
            info!("tables: not implemented in Phase 1");
        }

        Command::Delete {
            location,
            backup_name,
        } => {
            let name = backup_name_required(backup_name, "delete")?;
            let s3 = S3Client::new(&config.s3).await?;
            let loc = map_cli_location(location);

            list::delete(&config.clickhouse.data_path, &s3, &loc, &name).await?;

            info!(backup_name = %name, "Delete command complete");
        }

        Command::Clean { name } => {
            let ch = ChClient::new(&config.clickhouse)?;
            let data_path = &config.clickhouse.data_path;
            let count = list::clean_shadow(&ch, data_path, name.as_deref()).await?;
            info!(removed = count, "Clean command complete");
        }

        Command::CleanBroken { location } => {
            let s3 = S3Client::new(&config.s3).await?;
            let loc = map_cli_location(location);

            list::clean_broken(&config.clickhouse.data_path, &s3, &loc).await?;

            info!("CleanBroken command complete");
        }

        Command::Watch {
            watch_interval,
            full_interval,
            name_template,
            tables,
        } => {
            // Apply CLI overrides to config watch fields
            let mut config = config;
            if let Some(v) = watch_interval {
                config.watch.watch_interval = v;
            }
            if let Some(v) = full_interval {
                config.watch.full_interval = v;
            }
            if let Some(v) = name_template {
                config.watch.name_template = v;
            }
            if tables.is_some() {
                config.watch.tables = tables;
            }

            let ch = ChClient::new(&config.clickhouse)?;
            let s3 = S3Client::new(&config.s3).await?;

            // Query macros from ClickHouse for template resolution
            let macros = ch.get_macros().await.unwrap_or_default();
            if !macros.is_empty() {
                info!(macros = ?macros, "Resolved ClickHouse macros for watch templates");
            }

            // Create shutdown and reload channels
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let (reload_tx, reload_rx) = tokio::sync::watch::channel(false);

            // Spawn Ctrl+C handler for graceful shutdown
            let shutdown_tx_clone = shutdown_tx.clone();
            tokio::spawn(async move {
                tokio::signal::ctrl_c().await.ok();
                info!("Shutdown signal received");
                shutdown_tx_clone.send(true).ok();
            });

            // Spawn SIGHUP handler for config reload (Unix only)
            #[cfg(unix)]
            {
                let reload_tx_clone = reload_tx.clone();
                tokio::spawn(async move {
                    use tokio::signal::unix::{signal, SignalKind};
                    let mut sighup =
                        signal(SignalKind::hangup()).expect("failed to register SIGHUP handler");
                    loop {
                        sighup.recv().await;
                        info!("SIGHUP received, triggering config reload");
                        reload_tx_clone.send(true).ok();
                    }
                });
            }

            let config_path = PathBuf::from(&cli.config);
            let ctx = chbackup::watch::WatchContext {
                config: Arc::new(config),
                ch,
                s3,
                metrics: None, // No metrics in standalone watch mode
                state: chbackup::watch::WatchState::Idle,
                consecutive_errors: 0,
                force_next_full: false,
                last_backup_name: None,
                shutdown_rx,
                reload_rx,
                config_path,
                macros,
            };

            let exit = chbackup::watch::run_watch_loop(ctx).await;

            // Suppress unused variable warnings for channel senders
            drop(shutdown_tx);
            drop(reload_tx);

            match exit {
                chbackup::watch::WatchLoopExit::Shutdown => {
                    info!("Watch loop stopped by shutdown signal");
                }
                chbackup::watch::WatchLoopExit::MaxErrors => {
                    bail!("Watch loop aborted: max consecutive errors reached");
                }
                chbackup::watch::WatchLoopExit::Stopped => {
                    info!("Watch loop stopped");
                }
            }
        }

        Command::Server { watch } => {
            let ch = ChClient::new(&config.clickhouse)?;
            let s3 = S3Client::new(&config.s3).await?;
            let config_path = PathBuf::from(&cli.config);
            chbackup::server::start_server(Arc::new(config), ch, s3, watch, config_path).await?;
        }

        // default-config and print-config handled above (early return).
        Command::DefaultConfig | Command::PrintConfig => unreachable!(),
    }

    // 5. Lock is released automatically when _lock_guard is dropped.
    info!(command = cmd_name, "Command complete");

    Ok(())
}

/// Generate a backup name from the current UTC timestamp if none is provided.
///
/// Format: `YYYY-MM-DDTHHMMSS` (e.g. `2024-01-15T143052`).
fn resolve_backup_name(name: Option<String>) -> String {
    name.unwrap_or_else(|| Utc::now().format("%Y-%m-%dT%H%M%S").to_string())
}

/// Require a backup name, returning an error if not provided.
fn backup_name_required(name: Option<String>, command: &str) -> Result<String> {
    match name {
        Some(n) => Ok(n),
        None => bail!("backup_name is required for the '{}' command", command),
    }
}

/// Map the CLI `Location` enum to the list module's `Location` enum.
fn map_cli_location(loc: cli::Location) -> list::Location {
    match loc {
        cli::Location::Local => list::Location::Local,
        cli::Location::Remote => list::Location::Remote,
    }
}
