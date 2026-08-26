use std::collections::HashSet;
use std::io::IsTerminal;
use tracing_subscriber::EnvFilter;

/// Dependency targets capped at `warn` unless the operator opts in explicitly.
///
/// The AWS SDK logs `access_key_id` and `provider_name` at debug level, so leaving these
/// uncapped turns a routine `RUST_LOG=debug` into a credential leak.
const SUPPRESSED_TARGETS: &[&str] = &[
    "aws_config",
    "aws_sdk_s3",
    "aws_sdk_sts",
    "aws_smithy_runtime",
    "aws_smithy_runtime_api",
    "aws_smithy_http_client",
    "aws_runtime",
    "aws_sigv4",
    "hyper",
    "hyper_util",
    "h2",
    "rustls",
    "tower",
];

/// Extract the target names a `RUST_LOG` value mentions explicitly.
///
/// A directive is `target[span{field=value}]=level`, `target=level`, or a bare `level`.
/// Only the first two name a target; a bare level does not, which is what lets a blanket
/// `RUST_LOG=debug` stay suppressed while `RUST_LOG=debug,aws_sdk_s3=debug` is honoured.
fn explicit_targets(rust_log: &str) -> HashSet<&str> {
    rust_log
        .split(',')
        .filter_map(|directive| {
            let lhs = directive.split('=').next()?;
            // Strip any span/field selector: `target[span]` -> `target`.
            let target = lhs.split('[').next()?.trim();
            // A bare level (`debug`, `info`, ...) names no target.
            (!target.is_empty() && directive.contains('=')).then_some(target)
        })
        .collect()
}

/// Build the ordered env-filter directive list.
///
/// Directives are emitted base-first so the suppressions land **last** and win, which is what
/// stops `RUST_LOG=debug` from re-enabling AWS SDK debug output. A target the operator named
/// explicitly is left alone -- a blanket level is the footgun being closed, a per-target opt-in
/// is a deliberate choice.
///
/// When `s3_debug` is set the AWS targets are *raised* to debug instead, which is the supported
/// way to get real SDK request/response logging.
pub(crate) fn build_env_filter_directives(
    log_level: &str,
    rust_log: Option<&str>,
    s3_debug: bool,
) -> Vec<String> {
    let rust_log = rust_log.filter(|rl| !rl.trim().is_empty());

    let mut directives = vec![rust_log.unwrap_or(log_level).to_string()];

    if s3_debug {
        for target in SUPPRESSED_TARGETS.iter().filter(|t| t.starts_with("aws_")) {
            directives.push(format!("{target}=debug"));
        }
        return directives;
    }

    let named = rust_log.map(explicit_targets).unwrap_or_default();
    for target in SUPPRESSED_TARGETS {
        if !named.contains(target) {
            directives.push(format!("{target}=warn"));
        }
    }
    directives
}

/// Initialize the global tracing subscriber.
///
/// - `log_format`: `"text"` for human-readable (default), `"json"` for structured JSON lines.
/// - `log_level`: default log level string (e.g. `"info"`, `"debug"`).
/// - `s3_debug`: raise the AWS SDK targets to `debug` instead of suppressing them.
///
/// `RUST_LOG`, when set, replaces `log_level` -- but see `build_env_filter_directives`: it does
/// **not** lift the dependency-target suppressions unless it names those targets itself.
///
/// JSON mode is activated when `log_format == "json"`. The `server` command does **not** imply
/// JSON; that behaviour was removed deliberately.
///
/// Log level and format are start-time-only. There is no reload layer, because a reload layer
/// swaps the filter but not the formatter, so `log_format` would still need a restart.
pub fn init_logging(log_format: &str, log_level: &str, s3_debug: bool) {
    let use_json = log_format.eq_ignore_ascii_case("json");

    let rust_log = std::env::var("RUST_LOG").ok();
    let directives = build_env_filter_directives(log_level, rust_log.as_deref(), s3_debug);
    let env_filter = EnvFilter::new(directives.join(","));

    // ANSI only when stdout is a terminal. Unconditional colour injects escape bytes into
    // `kubectl logs` and `docker logs`, matching the TTY check in `progress.rs`.
    let ansi = std::io::stdout().is_terminal();

    let result = if use_json {
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_env_filter(env_filter)
            .with_target(true)
            .with_thread_ids(false)
            .with_thread_names(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber)
    } else {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(true)
            .with_ansi(ansi)
            .finish();
        tracing::subscriber::set_global_default(subscriber)
    };

    // Report via eprintln, not warn!: the error means either no subscriber is installed (in
    // which case a warn! is discarded, i.e. the very problem being reported) or another one
    // already is. Never panic -- a second call must not abort the process.
    if let Err(e) = result {
        eprintln!("chbackup: failed to install tracing subscriber: {e}");
    }

    if s3_debug {
        tracing::warn!(
            "s3.debug is enabled: AWS SDK request/response logging is on and may print \
             credentials (access_key_id, provider_name) to the log stream"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The suppressions must be usable as an EnvFilter, and must come after the base level so
    /// they win.
    #[test]
    fn test_logging_filter_suppresses_aws_targets() {
        let directives = build_env_filter_directives("debug", None, false);
        let filter_str = directives.join(",");

        EnvFilter::try_new(&filter_str).expect("filter string should parse as valid EnvFilter");

        assert_eq!(directives[0], "debug", "base level comes first");
        for target in SUPPRESSED_TARGETS {
            assert!(
                directives.contains(&format!("{target}=warn")),
                "should suppress {target}"
            );
        }

        let info = build_env_filter_directives("info", None, false);
        assert_eq!(info[0], "info");
        EnvFilter::try_new(info.join(",")).expect("info filter should also parse");
    }

    /// A blanket `RUST_LOG=debug` must NOT re-enable AWS SDK debug output -- that is the
    /// credential-leak footgun.
    #[test]
    fn test_blanket_rust_log_stays_suppressed() {
        let directives = build_env_filter_directives("info", Some("debug"), false);

        assert_eq!(directives[0], "debug", "RUST_LOG replaces the base level");
        assert!(directives.contains(&"aws_sdk_s3=warn".to_string()));
        assert!(directives.contains(&"aws_config=warn".to_string()));
        EnvFilter::try_new(directives.join(",")).expect("should parse");
    }

    /// A per-target opt-in is a deliberate choice and must survive.
    #[test]
    fn test_named_target_optin_is_honoured() {
        let directives = build_env_filter_directives("info", Some("debug,aws_sdk_s3=debug"), false);

        assert!(
            !directives.contains(&"aws_sdk_s3=warn".to_string()),
            "an explicitly named target must not be re-suppressed"
        );
        // Unnamed targets stay capped.
        assert!(directives.contains(&"aws_config=warn".to_string()));
        EnvFilter::try_new(directives.join(",")).expect("should parse");
    }

    /// `s3.debug` raises the AWS targets rather than suppressing them.
    #[test]
    fn test_s3_debug_raises_aws_targets() {
        let directives = build_env_filter_directives("info", None, true);

        assert!(directives.contains(&"aws_sdk_s3=debug".to_string()));
        assert!(directives.contains(&"aws_smithy_runtime=debug".to_string()));
        assert!(
            !directives.iter().any(|d| d.ends_with("=warn")),
            "s3_debug must not also suppress"
        );
        // Non-AWS noise is not raised.
        assert!(!directives.contains(&"hyper=debug".to_string()));
        EnvFilter::try_new(directives.join(",")).expect("should parse");
    }

    #[test]
    fn test_explicit_targets_parsing() {
        assert!(
            explicit_targets("debug").is_empty(),
            "bare level names no target"
        );
        assert!(explicit_targets("").is_empty());

        let t = explicit_targets("info,aws_sdk_s3=debug,hyper=trace");
        assert!(t.contains("aws_sdk_s3"));
        assert!(t.contains("hyper"));
        assert!(!t.contains("info"));

        // Span/field selectors are stripped down to the target name.
        let t = explicit_targets("aws_sdk_s3[request]=debug");
        assert!(t.contains("aws_sdk_s3"));

        // Whitespace tolerated.
        let t = explicit_targets("info, aws_config = warn ");
        assert!(t.contains("aws_config"));
    }

    /// An empty or whitespace-only RUST_LOG must fall back to the configured level rather than
    /// producing an empty base directive.
    #[test]
    fn test_empty_rust_log_falls_back() {
        for raw in ["", "   "] {
            let directives = build_env_filter_directives("warn", Some(raw), false);
            assert_eq!(directives[0], "warn");
            EnvFilter::try_new(directives.join(",")).expect("should parse");
        }
    }
}
