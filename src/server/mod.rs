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

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::middleware;
use axum::routing::{delete, get, post};
use axum::Router;
use tracing::{info, warn};

use crate::clickhouse::ChClient;
use crate::config::Config;
use crate::storage::S3Client;

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
        // Stub endpoints (not yet implemented)
        .route("/api/v1/clean", post(routes::clean_stub))
        .route("/api/v1/reload", post(routes::reload_stub))
        .route("/api/v1/restart", post(routes::restart_stub))
        .route("/api/v1/tables", get(routes::tables_stub))
        .route("/api/v1/watch/start", post(routes::watch_start_stub))
        .route("/api/v1/watch/stop", post(routes::watch_stop_stub))
        .route("/api/v1/watch/status", get(routes::watch_status_stub))
        .route("/metrics", get(routes::metrics_stub));

    // Conditionally apply auth middleware
    let has_auth = !state.config.api.username.is_empty() && !state.config.api.password.is_empty();

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

/// Start the API server.
///
/// Creates `AppState`, builds the router, optionally creates integration tables,
/// runs auto-resume, then listens on the configured address.
///
/// Graceful shutdown is triggered by Ctrl+C (SIGINT). On shutdown, integration
/// tables are dropped if they were created.
pub async fn start_server(config: Arc<Config>, ch: ChClient, s3: S3Client) -> Result<()> {
    let state = AppState::new(config.clone(), ch.clone(), s3);
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
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            info!("Shutdown signal received");
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

        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                tokio::signal::ctrl_c().await.ok();
                info!("Shutdown signal received");
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
