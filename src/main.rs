mod cli;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chbackup::clickhouse::{ChClient, TableRow};
use chbackup::config::Config;
use chbackup::error::exit_code_from_error;
use chbackup::lock::{lock_for_command, lock_path_for_scope, PidLock};
use chbackup::logging;
use chbackup::manifest::BackupManifest;
use chbackup::restore::remap;
use chbackup::server::state::validate_backup_name;
use chbackup::storage::S3Client;
use chbackup::table_filter::TableFilter;
use chbackup::{backup, download, list, restore, upload};
use clap::Parser;
use cli::{Cli, Command};
use tokio_util::sync::CancellationToken;
use tracing::info;

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

/// Acquire a PID lock for a command, optionally scoped to a backup name.
///
/// When `backup_name` is `Some`, the lock is per-backup (used after shortcut
/// resolution so the lock targets the real name, not "latest"/"previous").
/// When `None`, the lock is global (for commands like `clean` and `clean_broken`).
fn acquire_lock(cmd_name: &str, backup_name: Option<&str>) -> Result<Option<PidLock>> {
    let scope = lock_for_command(cmd_name, backup_name);
    match lock_path_for_scope(&scope) {
        Some(ref path) => {
            info!(command = cmd_name, lock_path = %path.display(), "Acquiring lock");
            let guard = PidLock::acquire(path, cmd_name)?;
            info!("Lock acquired");
            Ok(Some(guard))
        }
        None => Ok(None),
    }
}

#[tokio::main]
async fn main() {
    let code = match run().await {
        Ok(()) => 0,
        Err(e) => {
            let code = exit_code_from_error(&e);
            // Use eprintln before logging is initialized for early errors,
            // and tracing for errors after logging init.
            eprintln!("Error: {e:#}");
            info!(exit_code = code, "Exiting with code {}", code);
            code
        }
    };
    if code != 0 {
        std::process::exit(code);
    }
}

async fn run() -> Result<()> {
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
            let config_path = resolve_config_path(cli.config.as_deref())?;
            let config = Config::load(&config_path, &cli.env_overrides)?;
            let yaml = config.redacted_yaml()?;
            print!("{yaml}");
            return Ok(());
        }
        _ => {}
    }

    // -----------------------------------------------------------------------
    // Full flow: Load config -> Init logging -> Acquire lock -> Execute -> Release
    // -----------------------------------------------------------------------

    // 1. Load config (with env overlay and CLI --env overrides).
    let config_path = resolve_config_path(cli.config.as_deref())?;
    let config = Config::load(&config_path, &cli.env_overrides)?;

    // 2. Init logging.
    logging::init_logging(&config.general.log_format, &config.general.log_level);

    // 2a. Startup banner with version and key config.
    let cmd_name = format!("{:?}", cli.command)
        .split_whitespace()
        .next()
        .unwrap_or("unknown")
        .to_string();
    info!(
        version = env!("CARGO_PKG_VERSION"),
        git_sha = env!("CHBACKUP_GIT_SHA"),
        command = %cmd_name,
        config_path = %config_path.display(),
        data_path = %config.clickhouse.data_path,
        clickhouse = format_args!("{}:{}", config.clickhouse.host, config.clickhouse.port),
        s3_bucket = %config.s3.bucket,
        s3_prefix = %config.s3.prefix,
        "chbackup starting"
    );

    // 3. Validate backup name BEFORE any processing to prevent path-traversal
    // attacks where a name like "../etc/passwd" would create a lock at an unintended path.
    let bak_name = backup_name_from_command(&cli.command);
    if let Some(name) = bak_name {
        if let Err(e) = validate_backup_name(name) {
            bail!("invalid backup name '{}': {}", name, e);
        }
    }

    // 4. Execute command.
    // Lock acquisition is done INSIDE each command branch, AFTER shortcut resolution,
    // so that the lock is always taken on the resolved (actual) backup name rather
    // than a raw shortcut like "latest" or "previous".
    match cli.command {
        Command::Create {
            tables,
            partitions,
            diff_from,
            diff_from_remote,
            skip_projections,
            schema,
            rbac,
            configs,
            named_collections,
            skip_check_parts_columns,
            backup_name,
        } => {
            let name = resolve_backup_name(backup_name)?;
            let _lock = acquire_lock("create", Some(&name))?;
            let ch = ChClient::new(&config.clickhouse)?;

            // Construct S3 client only when diff-from-remote is needed
            let s3 = if diff_from_remote.is_some() {
                Some(S3Client::new(&config.s3).await?)
            } else {
                None
            };

            // Merge CLI --skip-projections with config.backup.skip_projections
            let effective_skip_projections = merge_skip_projections(
                skip_projections.as_deref(),
                &config.backup.skip_projections,
            );

            let _manifest = backup::create(
                &config,
                &ch,
                &name,
                tables.as_deref(),
                schema,
                diff_from.as_deref(),
                diff_from_remote.as_deref(),
                s3.as_ref(),
                partitions.as_deref(),
                skip_check_parts_columns,
                rbac,
                configs,
                named_collections,
                &effective_skip_projections,
                CancellationToken::new(),
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
            let raw_name = backup_name_required(backup_name, "upload")?;
            let name = resolve_local_shortcut(&raw_name, &config.clickhouse.data_path)?;
            let _lock = acquire_lock("upload", Some(&name))?;
            let ch = ChClient::new(&config.clickhouse)?;
            let s3 = S3Client::new(&config.s3).await?;

            let backup_dir = PathBuf::from(&config.clickhouse.data_path)
                .join("backup")
                .join(&name);

            let effective_resume = resume && config.general.use_resumable_state;
            let _stats = upload::upload(
                &config,
                &ch,
                &s3,
                &name,
                &backup_dir,
                delete_local,
                diff_from_remote.as_deref(),
                effective_resume,
                CancellationToken::new(),
            )
            .await?;

            // Apply retention after successful upload (design doc 3.6 step 7)
            list::apply_retention_after_upload(&config, &s3, Some(&name), None).await;

            info!(backup_name = %name, "Upload command complete");
        }

        Command::Download {
            hardlink_exists_files,
            resume,
            backup_name,
        } => {
            let raw_name = backup_name_required(backup_name, "download")?;
            let s3 = S3Client::new(&config.s3).await?;
            let name = resolve_remote_shortcut(&raw_name, &s3).await?;
            let _lock = acquire_lock("download", Some(&name))?;

            let effective_resume = resume && config.general.use_resumable_state;
            let backup_dir = download::download(
                &config,
                &s3,
                &name,
                effective_resume,
                hardlink_exists_files,
                CancellationToken::new(),
            )
            .await?;

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
            let raw_name = backup_name_required(backup_name, "restore")?;
            let name = resolve_local_shortcut(&raw_name, &config.clickhouse.data_path)?;
            let _lock = acquire_lock("restore", Some(&name))?;
            let ch = ChClient::new(&config.clickhouse)?;

            let db_mapping = match &database_mapping {
                Some(s) => Some(remap::parse_database_mapping(s)?),
                None => None,
            };

            let effective_resume = resume && config.general.use_resumable_state;
            restore::restore(
                &config,
                &ch,
                &name,
                tables.as_deref(),
                schema,
                data_only,
                rm,
                effective_resume,
                rename_as.as_deref(),
                db_mapping.as_ref(),
                rbac,
                configs,
                named_collections,
                partitions.as_deref(),
                skip_empty_tables,
                CancellationToken::new(),
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
            let name = resolve_backup_name(backup_name)?;
            let _lock = acquire_lock("create_remote", Some(&name))?;
            let ch = ChClient::new(&config.clickhouse)?;
            let s3 = S3Client::new(&config.s3).await?;

            // Merge CLI --skip-projections with config.backup.skip_projections
            let effective_skip_projections = merge_skip_projections(
                skip_projections.as_deref(),
                &config.backup.skip_projections,
            );

            // Step 1: Create local backup (pass diff_from_remote for create-time diff)
            let _manifest = backup::create(
                &config,
                &ch,
                &name,
                tables.as_deref(),
                false, // schema_only
                None,  // diff_from (create_remote uses diff_from_remote)
                diff_from_remote.as_deref(),
                Some(&s3),
                None, // partitions (create_remote doesn't support --partitions)
                skip_check_parts_columns,
                rbac,
                configs,
                named_collections,
                &effective_skip_projections,
                CancellationToken::new(),
            )
            .await?;

            // Step 2: Upload to S3 (with optional diff-from-remote)
            let backup_dir = PathBuf::from(&config.clickhouse.data_path)
                .join("backup")
                .join(&name);

            let effective_resume = resume && config.general.use_resumable_state;
            let _stats = upload::upload(
                &config,
                &ch,
                &s3,
                &name,
                &backup_dir,
                delete_source,
                diff_from_remote.as_deref(),
                effective_resume,
                CancellationToken::new(),
            )
            .await?;

            // Apply retention after successful upload (design doc 3.6 step 7)
            list::apply_retention_after_upload(&config, &s3, Some(&name), None).await;

            info!(backup_name = %name, "CreateRemote command complete");
        }

        Command::RestoreRemote {
            tables,
            rename_as,
            database_mapping,
            rm,
            rbac,
            configs,
            named_collections,
            skip_empty_tables,
            resume,
            backup_name,
        } => {
            let raw_name = backup_name_required(backup_name, "restore_remote")?;
            let ch = ChClient::new(&config.clickhouse)?;
            let s3 = S3Client::new(&config.s3).await?;
            let name = resolve_remote_shortcut(&raw_name, &s3).await?;
            let _lock = acquire_lock("restore_remote", Some(&name))?;

            let db_mapping = match &database_mapping {
                Some(s) => Some(remap::parse_database_mapping(s)?),
                None => None,
            };

            // Step 1: Download from S3
            let effective_resume = resume && config.general.use_resumable_state;
            let _backup_dir = download::download(
                &config,
                &s3,
                &name,
                effective_resume,
                false,
                CancellationToken::new(),
            )
            .await?;

            // Step 2: Restore with remap
            restore::restore(
                &config,
                &ch,
                &name,
                tables.as_deref(),
                false, // schema_only (not a flag on restore_remote per design)
                false, // data_only (not a flag on restore_remote per design)
                rm,
                effective_resume,
                rename_as.as_deref(),
                db_mapping.as_ref(),
                rbac,
                configs,
                named_collections,
                None, // partitions (not a flag on restore_remote per design)
                skip_empty_tables,
                CancellationToken::new(),
            )
            .await?;

            info!(backup_name = %name, "RestoreRemote command complete");
        }

        Command::List { location, format } => {
            let loc = location.map(map_cli_location);
            let fmt = map_cli_list_format(format);

            // Only initialize S3 when remote listing is needed.
            let needs_remote = loc.is_none() || loc == Some(list::Location::Remote);
            let s3 = if needs_remote {
                Some(S3Client::new(&config.s3).await?)
            } else {
                None
            };
            list::list(
                &config.clickhouse.data_path,
                s3.as_ref(),
                loc.as_ref(),
                &fmt,
            )
            .await?;

            info!("List command complete");
        }

        Command::Tables {
            tables,
            all,
            remote_backup,
        } => {
            if let Some(backup_name) = remote_backup {
                // Remote mode: download manifest and list tables
                let s3 = S3Client::new(&config.s3).await?;
                let manifest_key = format!("{}/metadata.json", backup_name);
                let manifest_data = s3.get_object(&manifest_key).await.with_context(|| {
                    format!("Failed to download manifest for backup '{}'", backup_name)
                })?;
                let manifest = BackupManifest::from_json_bytes(&manifest_data)
                    .context("Failed to parse backup manifest")?;

                let filter = tables.as_deref().map(TableFilter::new);

                for (full_name, tm) in &manifest.tables {
                    let parts: Vec<&str> = full_name.splitn(2, '.').collect();
                    let (db, tbl) = if parts.len() == 2 {
                        (parts[0], parts[1])
                    } else {
                        (full_name.as_str(), "")
                    };

                    if let Some(ref f) = filter {
                        let matched = if all {
                            f.matches_including_system(db, tbl)
                        } else {
                            f.matches(db, tbl)
                        };
                        if !matched {
                            continue;
                        }
                    }

                    let total: u64 = tm
                        .parts
                        .values()
                        .flat_map(|v| v.iter())
                        .map(|p| p.size)
                        .sum();
                    println!(
                        "  {}\t{}\t{}",
                        full_name,
                        tm.engine,
                        list::format_size(total)
                    );
                }

                info!(
                    backup_name = %backup_name,
                    tables_count = manifest.tables.len(),
                    "Tables command complete (remote)"
                );
            } else {
                // Live mode: query ClickHouse
                let ch = ChClient::new(&config.clickhouse)?;
                ch.ping().await?;

                let rows = if all {
                    ch.list_all_tables().await?
                } else {
                    ch.list_tables().await?
                };

                let filter = tables.as_deref().map(TableFilter::new);

                let filtered: Vec<&TableRow> = rows
                    .iter()
                    .filter(|t| {
                        if let Some(ref f) = filter {
                            if all {
                                f.matches_including_system(&t.database, &t.name)
                            } else {
                                f.matches(&t.database, &t.name)
                            }
                        } else {
                            true
                        }
                    })
                    .collect();

                for t in &filtered {
                    let bytes = t.total_bytes.unwrap_or(0);
                    println!(
                        "  {}.{}\t{}\t{}",
                        t.database,
                        t.name,
                        t.engine,
                        list::format_size(bytes)
                    );
                }

                info!(tables_count = filtered.len(), "Tables command complete");
            }
        }

        Command::Delete {
            location,
            backup_name,
        } => {
            let raw_name = backup_name_required(backup_name, "delete")?;
            let loc = map_cli_location(location);

            // Only initialize S3 for remote delete.
            match loc {
                list::Location::Local => {
                    let name = resolve_local_shortcut(&raw_name, &config.clickhouse.data_path)?;
                    let _lock = acquire_lock("delete", Some(&name))?;
                    list::delete_local(&config.clickhouse.data_path, &name)?;
                    info!(backup_name = %name, "Delete command complete");
                }
                list::Location::Remote => {
                    let s3 = S3Client::new(&config.s3).await?;
                    let name = resolve_remote_shortcut(&raw_name, &s3).await?;
                    let _lock = acquire_lock("delete", Some(&name))?;
                    list::delete_remote(&s3, &name).await?;
                    info!(backup_name = %name, "Delete command complete");
                }
            }
        }

        Command::Clean { name } => {
            let _lock = acquire_lock("clean", None)?;
            let ch = ChClient::new(&config.clickhouse)?;
            let data_path = &config.clickhouse.data_path;
            let count = list::clean_shadow(&ch, data_path, name.as_deref()).await?;
            info!(removed = count, "Clean command complete");
        }

        Command::CleanBroken { location } => {
            let _lock = acquire_lock("clean_broken", None)?;
            let loc = map_cli_location(location);

            // Only initialize S3 for remote clean_broken.
            match loc {
                list::Location::Local => {
                    let count = list::clean_broken_local(&config.clickhouse.data_path)?;
                    info!(count = count, "CleanBroken local complete");
                }
                list::Location::Remote => {
                    let s3 = S3Client::new(&config.s3).await?;
                    let count = list::clean_broken_remote(&s3).await?;
                    info!(count = count, "CleanBroken remote complete");
                }
            }

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
            // Re-validate after CLI overrides; catches invalid interval combinations.
            config.validate()?;

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
            chbackup::spawn_sighup_handler(reload_tx.clone());

            // Spawn SIGQUIT handler for stack dump (Unix only)
            #[cfg(unix)]
            chbackup::spawn_sigquit_handler();

            let config_path = resolve_config_path(cli.config.as_deref())?;
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
                manifest_cache: None, // No cache in standalone watch mode
                // Standalone watch has no API server; use a dummy WatchStatus
                watch_status: std::sync::Arc::new(tokio::sync::Mutex::new(
                    chbackup::server::state::WatchStatus::default(),
                )),
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

        Command::Server {
            watch,
            watch_interval,
            full_interval,
        } => {
            let mut config = config;
            if let Some(v) = watch_interval {
                config.watch.watch_interval = v;
            }
            if let Some(v) = full_interval {
                config.watch.full_interval = v;
            }
            // Re-validate after CLI overrides; catches invalid interval combinations.
            config.validate()?;
            let ch = ChClient::new(&config.clickhouse)?;
            let s3 = S3Client::new(&config.s3).await?;
            let config_path = resolve_config_path(cli.config.as_deref())?;
            chbackup::server::start_server(Arc::new(config), ch, s3, watch, config_path).await?;
        }

        // default-config and print-config handled above (early return).
        Command::DefaultConfig | Command::PrintConfig => unreachable!(),
    }

    // 5. Per-branch locks are released automatically when _lock is dropped at end of branch.
    Ok(())
}

/// Generate a backup name from the current UTC timestamp if none is provided.
///
/// Format: `YYYY-MM-DDTHHMMSS` (e.g. `2024-01-15T143052`).
/// When a name is provided, it is validated for path traversal safety.
fn resolve_backup_name(name: Option<String>) -> Result<String> {
    match name {
        Some(n) => {
            validate_backup_name(&n)
                .map_err(|e| anyhow::anyhow!("invalid backup name '{}': {}", n, e))?;
            if n == "latest" || n == "previous" {
                anyhow::bail!(
                    "'latest' and 'previous' are reserved shortcut names and cannot be used as backup names"
                );
            }
            Ok(n)
        }
        None => Ok(chbackup::generate_backup_name()),
    }
}

/// Require a backup name, returning an error if not provided.
/// Also validates the name for path traversal safety.
fn backup_name_required(name: Option<String>, command: &str) -> Result<String> {
    match name {
        Some(n) => {
            validate_backup_name(&n)
                .map_err(|e| anyhow::anyhow!("invalid backup name '{}': {}", n, e))?;
            Ok(n)
        }
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

/// Map the CLI `ListFormat` enum to the list module's `ListFormat` enum.
fn map_cli_list_format(fmt: cli::ListFormat) -> list::ListFormat {
    match fmt {
        cli::ListFormat::Default => list::ListFormat::Default,
        cli::ListFormat::Json => list::ListFormat::Json,
        cli::ListFormat::Yaml => list::ListFormat::Yaml,
        cli::ListFormat::Csv => list::ListFormat::Csv,
        cli::ListFormat::Tsv => list::ListFormat::Tsv,
    }
}

/// Resolve "latest" or "previous" backup name shortcuts against local backups.
///
/// If the name is "latest" or "previous", scans local backup directories and
/// resolves to the actual backup name. Otherwise returns the name unchanged.
fn resolve_local_shortcut(name: &str, data_path: &str) -> Result<String> {
    if name == "latest" || name == "previous" {
        let backups = list::list_local(data_path)?;
        let resolved = list::resolve_backup_shortcut(name, &backups)?;
        info!(original = name, resolved = %resolved, "Resolved local backup name shortcut");
        Ok(resolved)
    } else {
        Ok(name.to_string())
    }
}

/// Resolve "latest" or "previous" backup name shortcuts against remote backups.
///
/// If the name is "latest" or "previous", queries S3 for remote backups and
/// resolves to the actual backup name. Otherwise returns the name unchanged.
async fn resolve_remote_shortcut(name: &str, s3: &S3Client) -> Result<String> {
    if name == "latest" || name == "previous" {
        let backups = list::list_remote(s3).await?;
        let resolved = list::resolve_backup_shortcut(name, &backups)?;
        info!(original = name, resolved = %resolved, "Resolved remote backup name shortcut");
        Ok(resolved)
    } else {
        Ok(name.to_string())
    }
}

/// Merge the CLI `--skip-projections` flag with `config.backup.skip_projections`.
///
/// If the CLI flag is provided, its comma-separated patterns are used.
/// Otherwise, the config list is used. If both are empty, an empty Vec is returned.
fn merge_skip_projections(cli_flag: Option<&str>, config_list: &[String]) -> Vec<String> {
    match cli_flag {
        Some(patterns) => patterns
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        None => config_list.to_vec(),
    }
}

/// Resolve the config file path using the following fallback chain:
/// 1. CLI `-c` flag (explicit)
/// 2. `CHBACKUP_CONFIG` env var
/// 3. `CLICKHOUSE_BACKUP_CONFIG` env var (Go compat)
/// 4. `/etc/chbackup/config.yml` (if exists)
/// 5. `/etc/clickhouse-backup/config.yml` (Go compat, if exists)
/// 6. Default path `/etc/chbackup/config.yml` (Config::load will create default config)
fn resolve_config_path(cli_config: Option<&str>) -> anyhow::Result<PathBuf> {
    // 1. CLI flag takes priority — error if file doesn't exist
    if let Some(path) = cli_config {
        let p = PathBuf::from(path);
        anyhow::ensure!(
            p.exists(),
            "Config file not found: {} (specified via -c flag)",
            p.display()
        );
        return Ok(p);
    }

    // 2. CHBACKUP_CONFIG env var
    if let Ok(path) = std::env::var("CHBACKUP_CONFIG") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }

    // 3. CLICKHOUSE_BACKUP_CONFIG env var (Go compat)
    if let Ok(path) = std::env::var("CLICKHOUSE_BACKUP_CONFIG") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }

    // 4. Default chbackup path
    let chbackup_path = PathBuf::from("/etc/chbackup/config.yml");
    if chbackup_path.exists() {
        return Ok(chbackup_path);
    }

    // 5. Go clickhouse-backup path (compat)
    let go_path = PathBuf::from("/etc/clickhouse-backup/config.yml");
    if go_path.exists() {
        return Ok(go_path);
    }

    // 6. Empty path — Config::load will create default config
    Ok(PathBuf::from("/etc/chbackup/config.yml"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::TimeZone;

    use super::*;

    // -----------------------------------------------------------------------
    // backup_name_from_command tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_backup_name_from_command_create() {
        let cmd = Command::Create {
            backup_name: Some("daily".to_string()),
            tables: None,
            partitions: None,
            diff_from: None,
            diff_from_remote: None,
            skip_projections: None,
            schema: false,
            rbac: false,
            configs: false,
            named_collections: false,
            skip_check_parts_columns: false,
        };
        assert_eq!(backup_name_from_command(&cmd), Some("daily"));
    }

    #[test]
    fn test_backup_name_from_command_create_none() {
        let cmd = Command::Create {
            backup_name: None,
            tables: None,
            partitions: None,
            diff_from: None,
            diff_from_remote: None,
            skip_projections: None,
            schema: false,
            rbac: false,
            configs: false,
            named_collections: false,
            skip_check_parts_columns: false,
        };
        assert_eq!(backup_name_from_command(&cmd), None);
    }

    #[test]
    fn test_backup_name_from_command_upload() {
        let cmd = Command::Upload {
            backup_name: Some("upload-2024".to_string()),
            delete_local: false,
            diff_from_remote: None,
            resume: false,
        };
        assert_eq!(backup_name_from_command(&cmd), Some("upload-2024"));
    }

    #[test]
    fn test_backup_name_from_command_download() {
        let cmd = Command::Download {
            backup_name: Some("dl-backup".to_string()),
            hardlink_exists_files: false,
            resume: false,
        };
        assert_eq!(backup_name_from_command(&cmd), Some("dl-backup"));
    }

    #[test]
    fn test_backup_name_from_command_restore() {
        let cmd = Command::Restore {
            backup_name: Some("restore-test".to_string()),
            tables: None,
            rename_as: None,
            database_mapping: None,
            partitions: None,
            schema: false,
            data_only: false,
            rm: false,
            resume: false,
            rbac: false,
            configs: false,
            named_collections: false,
            skip_empty_tables: false,
        };
        assert_eq!(backup_name_from_command(&cmd), Some("restore-test"));
    }

    #[test]
    fn test_backup_name_from_command_create_remote() {
        let cmd = Command::CreateRemote {
            backup_name: Some("cr-backup".to_string()),
            tables: None,
            diff_from_remote: None,
            delete_source: false,
            rbac: false,
            configs: false,
            named_collections: false,
            skip_check_parts_columns: false,
            skip_projections: None,
            resume: false,
        };
        assert_eq!(backup_name_from_command(&cmd), Some("cr-backup"));
    }

    #[test]
    fn test_backup_name_from_command_restore_remote() {
        let cmd = Command::RestoreRemote {
            backup_name: Some("rr-backup".to_string()),
            tables: None,
            rename_as: None,
            database_mapping: None,
            rm: false,
            rbac: false,
            configs: false,
            named_collections: false,
            skip_empty_tables: false,
            resume: false,
        };
        assert_eq!(backup_name_from_command(&cmd), Some("rr-backup"));
    }

    #[test]
    fn test_backup_name_from_command_delete() {
        let cmd = Command::Delete {
            backup_name: Some("del-backup".to_string()),
            location: cli::Location::Local,
        };
        assert_eq!(backup_name_from_command(&cmd), Some("del-backup"));
    }

    #[test]
    fn test_backup_name_from_command_list_returns_none() {
        let cmd = Command::List {
            location: None,
            format: cli::ListFormat::Default,
        };
        assert_eq!(backup_name_from_command(&cmd), None);
    }

    #[test]
    fn test_backup_name_from_command_tables_returns_none() {
        let cmd = Command::Tables {
            tables: None,
            all: false,
            remote_backup: None,
        };
        assert_eq!(backup_name_from_command(&cmd), None);
    }

    // -----------------------------------------------------------------------
    // resolve_backup_name tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_resolve_backup_name_with_valid_name() {
        let result = resolve_backup_name(Some("daily-2024".to_string()));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "daily-2024");
    }

    #[test]
    fn test_resolve_backup_name_generates_when_none() {
        let result = resolve_backup_name(None);
        assert!(result.is_ok());
        let name = result.unwrap();
        assert!(!name.is_empty(), "Generated name should not be empty");
    }

    #[test]
    fn test_resolve_backup_name_rejects_latest() {
        let result = resolve_backup_name(Some("latest".to_string()));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("reserved"),
            "Error should mention 'reserved', got: {}",
            err
        );
    }

    #[test]
    fn test_resolve_backup_name_rejects_previous() {
        let result = resolve_backup_name(Some("previous".to_string()));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("reserved"),
            "Error should mention 'reserved', got: {}",
            err
        );
    }

    #[test]
    fn test_resolve_backup_name_rejects_path_traversal() {
        let result = resolve_backup_name(Some("../evil".to_string()));
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // backup_name_required tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_backup_name_required_with_name() {
        let result = backup_name_required(Some("daily".to_string()), "upload");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "daily");
    }

    #[test]
    fn test_backup_name_required_none_fails() {
        let result = backup_name_required(None, "upload");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("required"),
            "Error should mention 'required', got: {}",
            err
        );
    }

    #[test]
    fn test_backup_name_required_rejects_invalid() {
        let result = backup_name_required(Some("../bad".to_string()), "upload");
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // map_cli_location tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_map_cli_location_local() {
        assert_eq!(
            map_cli_location(cli::Location::Local),
            list::Location::Local
        );
    }

    #[test]
    fn test_map_cli_location_remote() {
        assert_eq!(
            map_cli_location(cli::Location::Remote),
            list::Location::Remote
        );
    }

    // -----------------------------------------------------------------------
    // map_cli_list_format tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_map_cli_list_format_all_variants() {
        assert_eq!(
            map_cli_list_format(cli::ListFormat::Default),
            list::ListFormat::Default
        );
        assert_eq!(
            map_cli_list_format(cli::ListFormat::Json),
            list::ListFormat::Json
        );
        assert_eq!(
            map_cli_list_format(cli::ListFormat::Yaml),
            list::ListFormat::Yaml
        );
        assert_eq!(
            map_cli_list_format(cli::ListFormat::Csv),
            list::ListFormat::Csv
        );
        assert_eq!(
            map_cli_list_format(cli::ListFormat::Tsv),
            list::ListFormat::Tsv
        );
    }

    // -----------------------------------------------------------------------
    // merge_skip_projections tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_merge_skip_projections_cli_takes_precedence() {
        let config_list = vec!["c".to_string()];
        let result = merge_skip_projections(Some("a,b"), &config_list);
        assert_eq!(result, vec!["a", "b"]);
    }

    #[test]
    fn test_merge_skip_projections_falls_back_to_config() {
        let config_list = vec!["x".to_string()];
        let result = merge_skip_projections(None, &config_list);
        assert_eq!(result, vec!["x"]);
    }

    #[test]
    fn test_merge_skip_projections_empty() {
        let result = merge_skip_projections(None, &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_merge_skip_projections_trims_whitespace() {
        let result = merge_skip_projections(Some(" a , b "), &[]);
        assert_eq!(result, vec!["a", "b"]);
    }

    #[test]
    fn test_merge_skip_projections_filters_empty_parts() {
        let result = merge_skip_projections(Some("a,,b,"), &[]);
        assert_eq!(result, vec!["a", "b"]);
    }

    #[test]
    fn test_acquire_lock_read_only_command_returns_none() {
        let guard = acquire_lock("list", None).expect("acquire_lock should not fail");
        assert!(guard.is_none(), "read-only command should not acquire lock");
    }

    #[test]
    fn test_acquire_lock_scopes_to_backup_name() {
        let name = format!("main-test-lock-{}", std::process::id());
        let guard = acquire_lock("create", Some(&name))
            .expect("acquire_lock should succeed")
            .expect("create command should acquire a lock");

        let path = guard.path().to_path_buf();
        assert!(
            path.to_string_lossy()
                .ends_with(&format!("/tmp/chbackup.{name}.pid")),
            "unexpected lock path: {}",
            path.display()
        );
        assert!(path.exists(), "lock file should exist while guard is held");

        drop(guard);
        assert!(!path.exists(), "lock file should be removed on drop");
    }

    fn write_local_manifest(
        data_path: &std::path::Path,
        backup_name: &str,
        year: i32,
        month: u32,
        day: u32,
    ) {
        let manifest = BackupManifest {
            manifest_version: 1,
            name: backup_name.to_string(),
            timestamp: chrono::Utc
                .with_ymd_and_hms(year, month, day, 12, 0, 0)
                .single()
                .expect("valid timestamp"),
            clickhouse_version: "25.1".to_string(),
            chbackup_version: env!("CARGO_PKG_VERSION").to_string(),
            data_format: "lz4".to_string(),
            compressed_size: 0,
            metadata_size: 0,
            disks: BTreeMap::new(),
            disk_types: BTreeMap::new(),
            disk_remote_paths: BTreeMap::new(),
            tables: BTreeMap::new(),
            databases: Vec::new(),
            functions: Vec::new(),
            named_collections: Vec::new(),
            rbac: None,
            rbac_size: 0,
            config_size: 0,
        };

        let metadata_path = data_path
            .join("backup")
            .join(backup_name)
            .join("metadata.json");
        manifest
            .save_to_file(&metadata_path)
            .expect("manifest should be written");
    }

    #[test]
    fn test_resolve_local_shortcut_passthrough_for_explicit_name() {
        let tmp = tempfile::tempdir().expect("tempdir should be created");
        let resolved = resolve_local_shortcut("explicit-backup", tmp.path().to_str().unwrap())
            .expect("shortcut resolution should succeed");
        assert_eq!(resolved, "explicit-backup");
    }

    #[test]
    fn test_resolve_local_shortcut_latest_and_previous() {
        let tmp = tempfile::tempdir().expect("tempdir should be created");
        let data_path = tmp.path();

        write_local_manifest(data_path, "daily-older", 2025, 1, 1);
        write_local_manifest(data_path, "daily-newer", 2025, 1, 2);

        let latest = resolve_local_shortcut("latest", data_path.to_str().unwrap())
            .expect("latest shortcut should resolve");
        assert_eq!(latest, "daily-newer");

        let previous = resolve_local_shortcut("previous", data_path.to_str().unwrap())
            .expect("previous shortcut should resolve");
        assert_eq!(previous, "daily-older");
    }
}
