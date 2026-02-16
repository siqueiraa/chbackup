use tracing_subscriber::EnvFilter;

/// Initialize the global tracing subscriber.
///
/// - `log_format`: `"text"` for human-readable (default), `"json"` for structured JSON lines.
/// - `log_level`: default log level string (e.g. `"info"`, `"debug"`).
/// - `is_server`: if `true`, forces JSON mode regardless of `log_format`.
///
/// The `RUST_LOG` environment variable, when set, overrides `log_level`.
///
/// JSON mode is activated when `log_format == "json"` **or** `is_server == true`
/// (per design doc section 11.4).
pub fn init_logging(log_format: &str, log_level: &str, is_server: bool) {
    let use_json = log_format.eq_ignore_ascii_case("json") || is_server;

    // Build the env filter: RUST_LOG takes precedence over the config log_level.
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(log_level));

    if use_json {
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_env_filter(env_filter)
            .with_target(true)
            .with_thread_ids(false)
            .with_thread_names(false)
            .finish();

        tracing::subscriber::set_global_default(subscriber)
            .expect("failed to set global tracing subscriber");
    } else {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(true)
            .with_ansi(true)
            .finish();

        tracing::subscriber::set_global_default(subscriber)
            .expect("failed to set global tracing subscriber");
    }
}
