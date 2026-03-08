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
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::clickhouse::ChClient;
use crate::config::Config;
use crate::storage::S3Client;
use crate::watch;

use self::state::AppState;

/// Build the axum `Router` with all API endpoints.
///
/// Auth middleware is always applied unconditionally. The middleware itself
/// reads live config on every request and passes through when both
/// `config.api.username` and `config.api.password` are empty, so auth can
/// be enabled at runtime via `/api/v1/restart` without rebuilding the router.
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
        .route("/api/v1/upload/:name", post(routes::upload_backup))
        .route("/api/v1/download/:name", post(routes::download_backup))
        .route("/api/v1/restore/:name", post(routes::restore_backup))
        .route("/api/v1/create_remote", post(routes::create_remote))
        .route("/api/v1/restore_remote/:name", post(routes::restore_remote))
        // Delete endpoints
        .route(
            "/api/v1/delete/:location/:name",
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
        .route("/metrics", get(routes::metrics))
        // -----------------------------------------------------------------
        // Go-compatible /backup/* routes (drop-in parity with clickhouse-backup)
        // -----------------------------------------------------------------
        .route("/backup/list", get(routes::go_list_backups))
        .route("/backup/list/:where", get(routes::go_list_by_location))
        .route(
            "/backup/actions",
            get(routes::go_get_actions).post(routes::go_post_actions),
        )
        .route("/backup/create", post(routes::go_create_backup))
        .route("/backup/create_remote", post(routes::go_create_remote))
        .route("/backup/upload/:name", post(routes::go_upload_backup))
        .route("/backup/download/:name", post(routes::go_download_backup))
        .route("/backup/restore/:name", post(routes::go_restore_backup))
        .route(
            "/backup/restore_remote/:name",
            post(routes::go_restore_remote),
        )
        .route(
            "/backup/delete/:where/:name",
            post(routes::go_delete_backup),
        )
        .route("/backup/clean", post(routes::go_clean))
        .route(
            "/backup/clean/remote_broken",
            post(routes::go_clean_remote_broken),
        )
        .route(
            "/backup/clean/local_broken",
            post(routes::go_clean_local_broken),
        )
        .route("/backup/status", get(routes::status))
        .route("/backup/kill", post(routes::kill_op))
        .route("/backup/tables", get(routes::tables))
        .route("/backup/tables/all", get(routes::go_tables_all))
        .route("/backup/version", get(routes::version));

    // Always apply auth middleware unconditionally. The middleware itself
    // reads live config on every request and passes through when credentials
    // are empty, so hot-reload via /api/v1/restart can enable auth at runtime.
    let router = router.layer(middleware::from_fn_with_state(
        state.clone(),
        auth::auth_middleware,
    ));

    // Add HTTP request/response tracing to capture transport-level failures
    // that may never reach the handler (connection resets, body extraction
    // errors, panics). Logs method, path, status, and duration for ALL requests.
    let router = router.layer(TraceLayer::new_for_http());

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
            watch_status: watch_status.clone(),
        };

        let watch_status_clone = watch_status.clone();
        let watch_is_main = config.api.watch_is_main_process;
        tokio::spawn(async move {
            let exit = watch::run_watch_loop(ctx).await;
            handle_watch_exit(exit, watch_status_clone, watch_is_main).await;
        });

        // Spawn SIGHUP handler for config reload (Unix only)
        #[cfg(unix)]
        crate::spawn_sighup_handler(reload_tx.clone());

        info!("Watch loop started");

        // Keep a reference so we can send shutdown on server exit
        Some(shutdown_tx)
    } else {
        None
    };

    // Spawn SIGQUIT handler for stack dump (Unix only)
    #[cfg(unix)]
    crate::spawn_sigquit_handler();

    let router = build_router(state.clone());

    // Parse listen address
    let listen = &config.api.listen;
    let addr: SocketAddr = listen
        .parse()
        .with_context(|| format!("invalid api.listen address: '{}'", listen))?;

    // For the non-TLS path, bind the TCP listener BEFORE creating integration
    // tables.  TcpListener::bind() registers the socket with the OS kernel,
    // which immediately starts accepting TCP SYN packets into the kernel's
    // backlog queue.  This prevents a startup race where ClickHouse tries to
    // reach the integration table URL while the server is not yet listening.
    //
    // TLS path limitation: axum_server::bind_rustls() combines bind+serve, so
    // we keep the current ordering there (integration tables before serve).
    let listener = if !config.api.secure {
        let l = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("failed to bind to {}", addr))?;
        info!(listen = %addr, "TCP listener bound on {}", addr);
        Some(l)
    } else {
        None
    };

    // Wait for ClickHouse to become reachable, then create integration tables.
    // In K8s sidecar deployments, ClickHouse starts alongside chbackup so we
    // retry indefinitely until it's ready.
    let created_tables = if config.api.create_integration_tables {
        let mut attempt = 0u64;
        loop {
            attempt += 1;
            match ch.ping().await {
                Ok(()) => {
                    let (host, port) = parse_integration_host_port(&config);
                    match ch.create_integration_tables(&host, &port).await {
                        Ok(()) => {
                            info!("Integration tables created");
                            break true;
                        }
                        Err(e) => {
                            warn!(
                                error = format_args!("{e:#}"),
                                "Failed to create integration tables (continuing anyway)"
                            );
                            break false;
                        }
                    }
                }
                Err(_) => {
                    let delay = std::cmp::min(attempt * 2, 30);
                    info!(
                        attempt = attempt,
                        retry_in_secs = delay,
                        "Waiting for ClickHouse to become reachable"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                }
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

        // unwrap is safe: listener is always Some when !config.api.secure
        let listener = listener.unwrap();

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

/// Handle the exit of a watch loop by updating status, logging, and
/// conditionally terminating the server process.
///
/// Shared between `start_server()` and `spawn_watch_from_state()` to avoid
/// code duplication (~30 lines of identical logic).
async fn handle_watch_exit(
    exit: watch::WatchLoopExit,
    watch_status: Arc<tokio::sync::Mutex<state::WatchStatus>>,
    watch_is_main: bool,
) {
    // Mark watch as inactive
    let mut ws = watch_status.lock().await;
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
        std::process::exit(1);
    }
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
        watch_status: state.watch_status.clone(),
    };

    let watch_status_clone = watch_status;
    let watch_is_main = config.api.watch_is_main_process;
    tokio::spawn(async move {
        let exit = watch::run_watch_loop(ctx).await;
        handle_watch_exit(exit, watch_status_clone, watch_is_main).await;
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
    use axum::body::Body;
    use axum::http::{self, Request, StatusCode};
    use tower::ServiceExt; // for oneshot()

    /// Build a test AppState with dummy clients (won't connect to anything).
    ///
    /// `ChClient::new()` just builds a clickhouse client struct pointing at localhost.
    /// `S3Client::new()` builds the AWS SDK config. Neither actually connects until
    /// a query is made, so they are safe for testing routes that don't touch backends.
    async fn test_app_state() -> AppState {
        let config = Arc::new(Config::default());
        let ch = crate::clickhouse::ChClient::new(&config.clickhouse)
            .expect("ChClient::new should succeed with default config");
        let s3 = crate::storage::S3Client::new(&config.s3)
            .await
            .expect("S3Client::new should succeed with default config");
        let config_path = std::path::PathBuf::from("/tmp/test-config.yml");
        AppState::new(config, ch, s3, config_path)
    }

    /// Build a test router with dummy state.
    async fn test_router() -> axum::Router {
        let state = test_app_state().await;
        build_router(state)
    }

    /// Build a test AppState with auth credentials configured.
    async fn test_app_state_with_auth(username: &str, password: &str) -> AppState {
        let mut config = Config::default();
        config.api.username = username.to_string();
        config.api.password = password.to_string();
        let config = Arc::new(config);
        let ch = crate::clickhouse::ChClient::new(&config.clickhouse)
            .expect("ChClient::new should succeed");
        let s3 = crate::storage::S3Client::new(&config.s3)
            .await
            .expect("S3Client::new should succeed");
        let config_path = std::path::PathBuf::from("/tmp/test-config.yml");
        AppState::new(config, ch, s3, config_path)
    }

    /// Helper to read the response body as bytes.
    async fn body_bytes(body: Body) -> Vec<u8> {
        use http_body_util::BodyExt;
        let collected = body.collect().await.expect("body collect should succeed");
        collected.to_bytes().to_vec()
    }

    /// Helper to read the response body as a serde_json::Value.
    async fn body_json(body: Body) -> serde_json::Value {
        let bytes = body_bytes(body).await;
        serde_json::from_slice(&bytes).expect("response body should be valid JSON")
    }

    // -----------------------------------------------------------------------
    // Axum integration tests using tower::ServiceExt::oneshot
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_route_health_returns_ok_json() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp.into_body()).await;
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn test_route_actions_empty() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/actions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp.into_body()).await;
        // Should be an empty array since no operations have been started
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_route_status_idle() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp.into_body()).await;
        assert_eq!(json["status"], "idle");
        assert!(json["command"].is_null());
        assert!(json["start"].is_null());
    }

    #[tokio::test]
    async fn test_route_watch_status_default() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/watch/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp.into_body()).await;
        assert_eq!(json["state"], "inactive");
        assert_eq!(json["active"], false);
        assert_eq!(json["consecutive_errors"], 0);
        assert!(json["last_full"].is_null());
        assert!(json["last_incr"].is_null());
        assert!(json["next_in"].is_null());
    }

    #[tokio::test]
    async fn test_route_kill_no_running_returns_404() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/api/v1/kill")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // No operations running, kill should return 404
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_route_kill_with_id_no_running_returns_404() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/api/v1/kill?id=999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_route_metrics_enabled_by_default() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Default config has enable_metrics=true
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = body_bytes(resp.into_body()).await;
        let text = String::from_utf8(bytes).expect("metrics should be UTF-8");
        assert!(
            text.contains("# HELP chbackup_"),
            "Should contain prometheus HELP lines"
        );
        assert!(
            text.contains("chbackup_in_progress"),
            "Should contain in_progress gauge"
        );
    }

    #[tokio::test]
    async fn test_route_metrics_disabled() {
        let mut config = Config::default();
        config.api.enable_metrics = false;
        let config = Arc::new(config);
        let ch = crate::clickhouse::ChClient::new(&config.clickhouse).unwrap();
        let s3 = crate::storage::S3Client::new(&config.s3).await.unwrap();
        let state = AppState::new(
            config,
            ch,
            s3,
            std::path::PathBuf::from("/tmp/test-config.yml"),
        );
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
        let bytes = body_bytes(resp.into_body()).await;
        let text = String::from_utf8(bytes).expect("body should be UTF-8");
        assert_eq!(text, "metrics disabled");
    }

    #[tokio::test]
    async fn test_route_create_invalid_name_path_traversal() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/api/v1/create")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"backup_name":"bad/../name"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp.into_body()).await;
        assert!(
            json["error"]
                .as_str()
                .unwrap()
                .contains("invalid backup name"),
            "Error should mention invalid backup name"
        );
    }

    #[tokio::test]
    async fn test_route_create_invalid_name_slash() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/api/v1/create")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"backup_name":"path/slash"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_route_create_invalid_name_empty() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/api/v1/create")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"backup_name":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_route_create_reserved_name_latest() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/api/v1/create")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"backup_name":"latest"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        // "latest" should be rejected by reject_reserved_backup_name in create
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp.into_body()).await;
        let err = json["error"].as_str().unwrap();
        assert!(
            err.contains("latest") || err.contains("reserved"),
            "Error should mention reserved name: {err}"
        );
    }

    #[tokio::test]
    async fn test_route_post_actions_empty_body() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/api/v1/actions")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"[]"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Empty command list should return 400
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_route_post_actions_unknown_command() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/api/v1/actions")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"[{"command":"nonexistent_cmd"}]"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Unknown command should return 400
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp.into_body()).await;
        assert!(json["error"].as_str().unwrap().contains("unknown command"));
    }

    #[tokio::test]
    async fn test_route_delete_invalid_location() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method(http::Method::DELETE)
                    .uri("/api/v1/delete/invalid_location/my-backup")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_route_watch_stop_not_active() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/api/v1/watch/stop")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Watch is not active so stop should return 404
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_route_version_endpoint() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp.into_body()).await;
        // version should be the cargo package version
        assert!(json["version"].is_string());
        assert!(!json["version"].as_str().unwrap().is_empty());
        // clickhouse_version will be "unknown" since we have no real CH
        assert!(json["clickhouse_version"].is_string());
    }

    #[tokio::test]
    async fn test_route_clean_remote_broken_starts_operation() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/api/v1/clean/remote_broken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // This should start an async operation (200 with action ID),
        // even though the S3 call will fail in the background.
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp.into_body()).await;
        assert!(json["id"].is_number());
        assert_eq!(json["status"], "started");
    }

    #[tokio::test]
    async fn test_route_upload_invalid_name() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/api/v1/upload/bad%2F..%2Fname")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Path traversal in upload name should return 400
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // -----------------------------------------------------------------------
    // Auth middleware integration tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_auth_no_config_passes_through() {
        // Default config has empty username/password -- auth should pass through
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_configured_no_header_returns_401() {
        let state = test_app_state_with_auth("admin", "secret123").await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        // Should include WWW-Authenticate header
        assert!(resp.headers().contains_key("www-authenticate"));
    }

    #[tokio::test]
    async fn test_auth_configured_correct_credentials_passes() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let state = test_app_state_with_auth("admin", "secret123").await;
        let app = build_router(state);
        let creds = STANDARD.encode("admin:secret123");
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header("authorization", format!("Basic {creds}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp.into_body()).await;
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn test_auth_configured_wrong_credentials_returns_401() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let state = test_app_state_with_auth("admin", "secret123").await;
        let app = build_router(state);
        let creds = STANDARD.encode("admin:wrongpassword");
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header("authorization", format!("Basic {creds}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_auth_configured_wrong_username_returns_401() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let state = test_app_state_with_auth("admin", "secret123").await;
        let app = build_router(state);
        let creds = STANDARD.encode("wronguser:secret123");
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header("authorization", format!("Basic {creds}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_auth_non_basic_scheme_returns_401() {
        let state = test_app_state_with_auth("admin", "secret123").await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header("authorization", "Bearer some-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_auth_invalid_base64_returns_401() {
        let state = test_app_state_with_auth("admin", "secret123").await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header("authorization", "Basic not-valid-base64!!!")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_route_nonexistent_returns_404() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_route_wrong_method_returns_405() {
        let app = test_router().await;
        // /health only supports GET, try POST
        let resp = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // axum returns 405 Method Not Allowed for wrong method on existing route
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    // -----------------------------------------------------------------------
    // Original unit tests
    // -----------------------------------------------------------------------

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

    #[test]
    fn test_parse_integration_host_port_ipv4() {
        let mut config = Config::default();
        config.api.listen = "127.0.0.1:9090".to_string();
        let (host, port) = parse_integration_host_port(&config);
        assert_eq!(host, "localhost"); // default when integration_tables_host is empty
        assert_eq!(port, "9090");
    }

    #[test]
    fn test_parse_integration_host_port_no_colon() {
        // Edge case: if listen has no colon, rsplit(':').next() returns the whole string
        let mut config = Config::default();
        config.api.listen = "badformat".to_string();
        let (host, port) = parse_integration_host_port(&config);
        assert_eq!(host, "localhost");
        assert_eq!(port, "badformat"); // entire string when no colon
    }

    #[test]
    fn test_parse_integration_host_port_empty_host_config() {
        let mut config = Config::default();
        config.api.integration_tables_host = String::new();
        config.api.listen = "0.0.0.0:7171".to_string();
        let (host, port) = parse_integration_host_port(&config);
        assert_eq!(host, "localhost");
        assert_eq!(port, "7171");
    }
}
