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
    // When RUST_LOG is not set, cap noisy dependency crates at warn to prevent
    // AWS SDK from leaking access_key_id and provider_name at debug level.
    let env_filter = match EnvFilter::try_from_default_env() {
        Ok(f) => f, // user-supplied RUST_LOG — honor it fully
        Err(_) => EnvFilter::new(format!(
            "{log_level},\
             aws_config=warn,aws_sdk_s3=warn,aws_sdk_sts=warn,\
             aws_smithy_runtime=warn,aws_smithy_runtime_api=warn,\
             aws_smithy_http_client=warn,aws_runtime=warn,\
             aws_sigv4=warn,\
             hyper=warn,hyper_util=warn,h2=warn,rustls=warn,tower=warn"
        )),
    };

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the env filter string for a given log level (same logic as init_logging
    /// when RUST_LOG is not set).
    fn build_default_filter(log_level: &str) -> String {
        format!(
            "{log_level},\
             aws_config=warn,aws_sdk_s3=warn,aws_sdk_sts=warn,\
             aws_smithy_runtime=warn,aws_smithy_runtime_api=warn,\
             aws_smithy_http_client=warn,aws_runtime=warn,\
             aws_sigv4=warn,\
             hyper=warn,hyper_util=warn,h2=warn,rustls=warn,tower=warn"
        )
    }

    #[test]
    fn test_logging_filter_suppresses_aws_targets() {
        let filter_str = build_default_filter("debug");

        // Verify the string parses into a valid EnvFilter
        let _filter = EnvFilter::try_new(&filter_str)
            .expect("filter string should parse as valid EnvFilter");

        // Verify the filter string contains the expected target suppressions
        assert!(
            filter_str.contains("aws_sdk_s3=warn"),
            "Should suppress aws_sdk_s3"
        );
        assert!(
            filter_str.contains("aws_config=warn"),
            "Should suppress aws_config"
        );
        assert!(
            filter_str.contains("aws_smithy_runtime=warn"),
            "Should suppress aws_smithy_runtime"
        );
        assert!(
            filter_str.contains("hyper=warn"),
            "Should suppress hyper"
        );
        assert!(
            filter_str.contains("rustls=warn"),
            "Should suppress rustls"
        );
        assert!(
            filter_str.starts_with("debug,"),
            "Should start with the configured level"
        );

        // Verify the filter is usable (doesn't panic when used)
        drop(_filter);

        // Also test with info level
        let info_filter_str = build_default_filter("info");
        assert!(info_filter_str.starts_with("info,"));
        EnvFilter::try_new(&info_filter_str).expect("info filter should also parse");
    }
}
