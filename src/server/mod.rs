//! HTTP API server module for chbackup.
//!
//! Provides an axum-based HTTP server with all API endpoints from design doc
//! section 9, enabling Kubernetes sidecar operation.
//!
//! # Architecture
//!
//! - `build_router()` assembles all routes and optional auth middleware
//! - `start_server()` creates `AppState`, builds the router, and starts
//!   listening on the configured address (with optional TLS)
//! - Graceful shutdown via Ctrl+C drops integration tables

pub mod actions;
pub mod auth;
pub mod metrics;
pub mod routes;
pub mod state;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::middleware;
use axum::routing::{delete, get, post};
use axum::Router;
use tracing::{info, warn};

use crate::clickhouse::ChClient;
use crate::config::Config;
use crate::storage::S3Client;
use crate::watch;

use self::state::AppState;

/// Build the axum `Router` with all API endpoints.
///
/// If `config.api.username` and `config.api.password` are both non-empty,
/// the auth middleware is applied to all routes.
pub fn build_router(state: AppState) -> Router {
    let router = Router::new()
        // Health check
        .route("/health", get(routes::health))
        // Read-only endpoints
        .route("/api/v1/version", get(routes::version))
        .route("/api/v1/status", get(routes::status))
        .route(
            "/api/v1/actions",
            get(routes::get_actions).post(routes::post_actions),
        )
        .route("/api/v1/list", get(routes::list_backups))
        // Backup operation endpoints
        .route("/api/v1/create", post(routes::create_backup))
        .route("/api/v1/upload/{name}", post(routes::upload_backup))
        .route("/api/v1/download/{name}", post(routes::download_backup))
        .route("/api/v1/restore/{name}", post(routes::restore_backup))
        .route("/api/v1/create_remote", post(routes::create_remote))
        .route(
            "/api/v1/restore_remote/{name}",
            post(routes::restore_remote),
        )
        // Delete endpoints
        .route(
            "/api/v1/delete/{location}/{name}",
            delete(routes::delete_backup),
        )
        // Clean endpoints
        .route(
            "/api/v1/clean/remote_broken",
            post(routes::clean_remote_broken),
        )
        .route(
            "/api/v1/clean/local_broken",
            post(routes::clean_local_broken),
        )
        // Kill
        .route("/api/v1/kill", post(routes::kill_op))
        // Clean endpoint
        .route("/api/v1/clean", post(routes::clean))
        // Watch lifecycle endpoints
        .route("/api/v1/reload", post(routes::reload))
        .route("/api/v1/watch/start", post(routes::watch_start))
        .route("/api/v1/watch/stop", post(routes::watch_stop))
        .route("/api/v1/watch/status", get(routes::watch_status))
        // Restart endpoint
        .route("/api/v1/restart", post(routes::restart))
        // Tables endpoint
        .route("/api/v1/tables", get(routes::tables))
        .route("/metrics", get(routes::metrics));

    // Conditionally apply auth middleware
    let config = state.config.load();
    let has_auth = !config.api.username.is_empty() && !config.api.password.is_empty();

    let router = if has_auth {
        info!("API authentication enabled");
        router.layer(middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ))
    } else {
        router
    };

    router.with_state(state)
}

/// Wait for either SIGINT (Ctrl+C) or SIGTERM to trigger graceful shutdown.
///
/// On Unix, both signals are handled; on non-Unix only SIGINT (Ctrl+C) is available.
/// This enables Kubernetes `kubectl delete pod` (which sends SIGTERM) to trigger
/// the same graceful shutdown as Ctrl+C.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let ctrl_c = tokio::signal::ctrl_c();
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
    }
}

/// Start the API server.
///
/// Creates `AppState`, builds the router, optionally creates integration tables,
/// runs auto-resume, then listens on the configured address.
///
/// When `watch` is true, spawns the watch loop as a background task alongside
/// the HTTP server. The watch loop is also started if `config.watch.enabled` is set.
///
/// Graceful shutdown is triggered by Ctrl+C (SIGINT) or SIGTERM (Unix). On shutdown,
/// integration tables are dropped if they were created, and the watch loop is signaled
/// to stop.
pub async fn start_server(
    config: Arc<Config>,
    ch: ChClient,
    s3: S3Client,
    watch: bool,
    config_path: PathBuf,
) -> Result<()> {
    let state = AppState::new(config.clone(), ch.clone(), s3.clone(), config_path.clone());

    // Determine if watch should be enabled (CLI flag or config)
    let watch_enabled = watch || config.watch.enabled;

    // Set up watch loop channels and spawn if enabled
    let watch_shutdown_tx = if watch_enabled {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (reload_tx, reload_rx) = tokio::sync::watch::channel(false);

        // Store senders in AppState for API endpoint access.
        // Write through the Arc<Mutex> so that axum handler clones see the update.
        *state.watch_shutdown_tx.lock().await = Some(shutdown_tx.clone());
        *state.watch_reload_tx.lock().await = Some(reload_tx.clone());

        // Query macros from ClickHouse for template resolution
        let macros = ch.get_macros().await.unwrap_or_default();
        if !macros.is_empty() {
            info!(macros = ?macros, "Resolved ClickHouse macros for watch templates");
        }

        let watch_status = state.watch_status.clone();
        let watch_metrics = state.metrics.clone();

        // Mark watch as active
        {
            let mut ws = watch_status.lock().await;
            ws.active = true;
            ws.state = "idle".to_string();
        }

        let ctx = watch::WatchContext {
            config: config.clone(),
            ch: ch.clone(),
            s3: s3.clone(),
            metrics: watch_metrics,
            state: watch::WatchState::Idle,
            consecutive_errors: 0,
            force_next_full: false,
            last_backup_name: None,
            shutdown_rx,
            reload_rx,
            config_path: config_path.clone(),
            macros,
            manifest_cache: Some(state.manifest_cache.clone()),
        };

        let watch_status_clone = watch_status.clone();
        let watch_is_main = config.api.watch_is_main_process;
        tokio::spawn(async move {
            let exit = watch::run_watch_loop(ctx).await;
            // Mark watch as inactive on exit
            let mut ws = watch_status_clone.lock().await;
            ws.active = false;
            ws.state = "inactive".to_string();
            drop(ws);

            let should_shutdown = watch_is_main
                && !matches!(
                    exit,
                    watch::WatchLoopExit::Shutdown | watch::WatchLoopExit::Stopped
                );

            info!(
                watch_is_main_process = watch_is_main,
                shutting_down = should_shutdown,
                "Watch loop exited, watch_is_main_process={}, shutting down={}",
                watch_is_main,
                should_shutdown
            );

            match exit {
                watch::WatchLoopExit::Shutdown => {
                    info!("Watch loop stopped by shutdown signal");
                }
                watch::WatchLoopExit::MaxErrors => {
                    warn!("Watch loop aborted: max consecutive errors reached");
                }
                watch::WatchLoopExit::Stopped => {
                    info!("Watch loop stopped via API");
                }
            }

            if should_shutdown {
                info!("watch_is_main_process is true, terminating server process");
                std::process::exit(0);
            }
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

        info!("Watch loop started");

        // Keep a reference so we can send shutdown on server exit
        Some(shutdown_tx)
    } else {
        None
    };

    // Spawn SIGQUIT handler for stack dump (Unix only)
    #[cfg(unix)]
    {
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigquit =
                signal(SignalKind::quit()).expect("failed to register SIGQUIT handler");
            loop {
                sigquit.recv().await;
                info!("SIGQUIT received, dumping stack trace to stderr");
                let bt = std::backtrace::Backtrace::force_capture();
                eprintln!("=== SIGQUIT stack dump ===");
                eprintln!("{bt}");
                eprintln!("=== end stack dump ===");
            }
        });
    }

    let router = build_router(state.clone());

    // Parse listen address
    let listen = &config.api.listen;
    let addr: SocketAddr = listen
        .parse()
        .with_context(|| format!("invalid api.listen address: '{}'", listen))?;

    // Create integration tables if configured
    let created_tables = if config.api.create_integration_tables {
        let (host, port) = parse_integration_host_port(&config);
        match ch.create_integration_tables(&host, &port).await {
            Ok(()) => {
                info!("Integration tables created");
                true
            }
            Err(e) => {
                warn!(error = %e, "Failed to create integration tables (continuing anyway)");
                false
            }
        }
    } else {
        false
    };

    // Auto-resume interrupted operations
    state::auto_resume(&state).await;

    // Start server (TLS or plain)
    if config.api.secure {
        info!(listen = %addr, "Starting API server on {} (TLS)", addr);

        let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            &config.api.certificate_file,
            &config.api.private_key_file,
        )
        .await
        .context("failed to load TLS certificate/key")?;

        let handle = axum_server::Handle::new();
        let handle_clone = handle.clone();

        // Spawn shutdown signal handler
        let ch_shutdown = ch.clone();
        let created_tables_shutdown = created_tables;
        let watch_shutdown_tx_clone = watch_shutdown_tx.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            info!("Shutdown signal received");
            if let Some(tx) = watch_shutdown_tx_clone {
                tx.send(true).ok();
            }
            if created_tables_shutdown {
                if let Err(e) = ch_shutdown.drop_integration_tables().await {
                    warn!(error = %e, "Failed to drop integration tables during shutdown");
                }
            }
            handle_clone.shutdown();
        });

        axum_server::bind_rustls(addr, tls_config)
            .handle(handle)
            .serve(router.into_make_service())
            .await
            .context("TLS server error")?;
    } else {
        info!(listen = %addr, "Starting API server on {}", addr);

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("failed to bind to {}", addr))?;

        let ch_shutdown = ch.clone();
        let created_tables_shutdown = created_tables;
        let watch_shutdown_tx_clone = watch_shutdown_tx.clone();

        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                shutdown_signal().await;
                info!("Shutdown signal received");
                if let Some(tx) = watch_shutdown_tx_clone {
                    tx.send(true).ok();
                }
                if created_tables_shutdown {
                    if let Err(e) = ch_shutdown.drop_integration_tables().await {
                        warn!(error = %e, "Failed to drop integration tables during shutdown");
                    }
                }
            })
            .await
            .context("server error")?;
    }

    info!("Server stopped");
    Ok(())
}

/// Spawn a watch loop from API state (for the watch/start endpoint).
///
/// Creates new channels and a WatchContext, spawns the loop, and stores
/// the shutdown/reload senders in AppState.
pub async fn spawn_watch_from_state(
    state: &mut AppState,
    config_path: PathBuf,
    macros: HashMap<String, String>,
) {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (reload_tx, reload_rx) = tokio::sync::watch::channel(false);

    // Write through the Arc<Mutex> so that all axum handler clones see the update.
    *state.watch_shutdown_tx.lock().await = Some(shutdown_tx);
    *state.watch_reload_tx.lock().await = Some(reload_tx);

    let watch_status = state.watch_status.clone();
    {
        let mut ws = watch_status.lock().await;
        ws.active = true;
        ws.state = "idle".to_string();
    }

    let config = state.config.load_full();
    let ch = state.ch.load_full();
    let s3 = state.s3.load_full();
    let ctx = watch::WatchContext {
        config: Arc::clone(&config),
        ch: ChClient::clone(&ch),
        s3: S3Client::clone(&s3),
        metrics: state.metrics.clone(),
        state: watch::WatchState::Idle,
        consecutive_errors: 0,
        force_next_full: false,
        last_backup_name: None,
        shutdown_rx,
        reload_rx,
        config_path,
        macros,
        manifest_cache: Some(state.manifest_cache.clone()),
    };

    let watch_status_clone = watch_status;
    let watch_is_main = config.api.watch_is_main_process;
    tokio::spawn(async move {
        let exit = watch::run_watch_loop(ctx).await;
        let mut ws = watch_status_clone.lock().await;
        ws.active = false;
        ws.state = "inactive".to_string();
        drop(ws);

        let should_shutdown = watch_is_main
            && !matches!(
                exit,
                watch::WatchLoopExit::Shutdown | watch::WatchLoopExit::Stopped
            );

        info!(
            watch_is_main_process = watch_is_main,
            shutting_down = should_shutdown,
            "Watch loop exited, watch_is_main_process={}, shutting down={}",
            watch_is_main,
            should_shutdown
        );

        match exit {
            watch::WatchLoopExit::Shutdown => {
                info!("Watch loop stopped by shutdown signal");
            }
            watch::WatchLoopExit::MaxErrors => {
                warn!("Watch loop aborted: max consecutive errors reached");
            }
            watch::WatchLoopExit::Stopped => {
                info!("Watch loop stopped via API");
            }
        }

        if should_shutdown {
            info!("watch_is_main_process is true, terminating server process");
            std::process::exit(0);
        }
    });
}

/// Parse the host and port for integration table URLs.
///
/// Uses `integration_tables_host` from config if set, otherwise "localhost".
/// Port is extracted from `api.listen` address.
fn parse_integration_host_port(config: &Config) -> (String, String) {
    let host = if config.api.integration_tables_host.is_empty() {
        "localhost".to_string()
    } else {
        config.api.integration_tables_host.clone()
    };

    // Extract port from listen address (format: "host:port")
    let port = config
        .api
        .listen
        .rsplit(':')
        .next()
        .unwrap_or("7171")
        .to_string();

    (host, port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_integration_host_port_defaults() {
        let config = Config::default();
        let (host, port) = parse_integration_host_port(&config);
        assert_eq!(host, "localhost");
        assert_eq!(port, "7171");
    }

    #[test]
    fn test_parse_integration_host_port_custom() {
        let mut config = Config::default();
        config.api.integration_tables_host = "backup-server".to_string();
        config.api.listen = "0.0.0.0:8080".to_string();
        let (host, port) = parse_integration_host_port(&config);
        assert_eq!(host, "backup-server");
        assert_eq!(port, "8080");
    }
}
