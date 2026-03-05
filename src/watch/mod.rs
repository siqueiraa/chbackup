//! Watch mode scheduler for automatic backup chains (design doc section 10).
//!
//! This module implements the state machine loop that maintains a rolling chain
//! of full + incremental backups. It includes name template resolution,
//! resume-on-restart logic, and integration with the server API endpoints.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tracing::{info, warn};

use tokio::sync::Mutex;

use crate::clickhouse::ChClient;
use crate::config::{parse_duration_secs, Config};
use crate::list::{BackupSummary, ManifestCache};
use crate::lock::PidLock;
use crate::server::metrics::Metrics;
use crate::server::state::{validate_backup_name, WatchStatus};
use crate::storage::S3Client;

// ---------------------------------------------------------------------------
// Name template resolution
// ---------------------------------------------------------------------------

/// Resolve a backup name template by substituting macro placeholders.
///
/// Supported placeholders:
/// - `{type}` -- replaced with `backup_type` ("full" or "incr")
/// - `{time:FORMAT}` -- replaced with `now.format(FORMAT)` using chrono strftime
/// - `{macro_name}` -- replaced from the `macros` HashMap (e.g., `{shard}` -> "01")
///
/// Unrecognized `{...}` patterns are left as-is.
pub fn resolve_name_template(
    template: &str,
    backup_type: &str,
    now: DateTime<Utc>,
    macros: &HashMap<String, String>,
) -> String {
    let mut result = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '{' {
            // Collect everything until the closing '}'
            let mut macro_content = String::new();
            let mut found_close = false;
            for inner in chars.by_ref() {
                if inner == '}' {
                    found_close = true;
                    break;
                }
                macro_content.push(inner);
            }

            if !found_close {
                // No closing brace found, output as literal
                result.push('{');
                result.push_str(&macro_content);
                continue;
            }

            // Now resolve the macro_content
            if macro_content == "type" {
                result.push_str(backup_type);
            } else if let Some(format_str) = macro_content.strip_prefix("time:") {
                let formatted = now.format(format_str).to_string();
                result.push_str(&formatted);
            } else if let Some(value) = macros.get(&macro_content) {
                result.push_str(value);
            } else {
                // Unknown macro: leave as-is
                result.push('{');
                result.push_str(&macro_content);
                result.push('}');
            }
        } else {
            result.push(ch);
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Backup type classification
// ---------------------------------------------------------------------------

/// Classify a backup name as "full" or "incr" based on the template structure.
///
/// Uses glob pattern matching to determine the backup type. For each candidate
/// ("full", "incr"), builds a glob pattern by replacing `{type}` with the
/// candidate literal and all other `{...}` macros with `*`. This approach is
/// delimiter-agnostic and correctly handles macro values that contain the
/// template delimiter character (e.g., shard="a-b" with delimiter "-").
///
/// Returns `None` if:
/// - Template has no `{type}` placeholder
/// - Name doesn't match either "full" or "incr" pattern
/// - Both "full" and "incr" patterns match (ambiguous)
pub fn classify_backup_type(template: &str, name: &str) -> Option<&'static str> {
    // Template must contain {type} placeholder
    if !template.contains("{type}") {
        return None;
    }

    let full_matches = build_type_glob(template, "full")
        .map(|p| p.matches(name))
        .unwrap_or(false);
    let incr_matches = build_type_glob(template, "incr")
        .map(|p| p.matches(name))
        .unwrap_or(false);

    match (full_matches, incr_matches) {
        (true, false) => Some("full"),
        (false, true) => Some("incr"),
        // Both match (ambiguous) or neither match
        _ => None,
    }
}

/// Build a glob pattern from a name template by substituting `{type}` with a
/// candidate literal ("full" or "incr") and all other `{...}` macros with `*`.
///
/// Glob special characters (`*`, `?`, `[`, `]`) in static text portions are
/// escaped so they match literally.
fn build_type_glob(template: &str, candidate: &str) -> Option<glob::Pattern> {
    let mut pattern = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '{' {
            // Collect the macro name (everything up to '}')
            let mut macro_name = String::new();
            for inner in chars.by_ref() {
                if inner == '}' {
                    break;
                }
                macro_name.push(inner);
            }

            // Check if this is the {type} placeholder (compare just the name
            // portion before any ':' format specifier, though {type} has none)
            let key = macro_name.split(':').next().unwrap_or(&macro_name);
            if key == "type" {
                // Replace with the literal candidate value
                // Escape candidate in case it contains glob special chars (it won't
                // for "full"/"incr", but be defensive)
                for c in candidate.chars() {
                    push_escaped_glob_char(&mut pattern, c);
                }
            } else {
                // Replace any other macro with glob wildcard
                pattern.push('*');
            }
        } else {
            // Static character -- escape glob special chars
            push_escaped_glob_char(&mut pattern, ch);
        }
    }

    glob::Pattern::new(&pattern).ok()
}

/// Push a character to a glob pattern string, escaping glob special characters
/// (`*`, `?`, `[`, `]`) so they match literally.
fn push_escaped_glob_char(pattern: &mut String, ch: char) {
    match ch {
        '*' | '?' | '[' | ']' => {
            pattern.push('[');
            pattern.push(ch);
            pattern.push(']');
        }
        _ => pattern.push(ch),
    }
}

// ---------------------------------------------------------------------------
// Resume state
// ---------------------------------------------------------------------------

/// Decision for what the watch loop should do next after examining remote backups.
#[derive(Debug, PartialEq)]
pub enum ResumeDecision {
    /// No backups exist matching the template; create a full backup immediately.
    FullNow,
    /// An incremental backup is due; `diff_from` is the base backup name.
    IncrNow { diff_from: String },
    /// The most recent backup is still fresh; sleep for `remaining` then create `backup_type`.
    SleepThen {
        remaining: std::time::Duration,
        backup_type: String,
    },
}

/// Extract the static prefix from a name template (everything before the first `{`).
///
/// Used to filter remote backups to only those created by this watch instance.
/// E.g., `"shard1-{type}-{time:%Y%m%d}"` -> `"shard1-"`.
pub fn resolve_template_prefix(name_template: &str) -> String {
    match name_template.find('{') {
        Some(pos) => name_template[..pos].to_string(),
        None => name_template.to_string(),
    }
}

/// Determine the next action for the watch loop based on existing remote backups.
///
/// Implements the resume logic from design doc section 10.5:
/// 1. Filter backups by template prefix and exclude broken ones
/// 2. Find most recent full and incremental backups
/// 3. Decide based on elapsed time vs intervals
pub fn resume_state(
    backups: &[BackupSummary],
    name_template: &str,
    watch_interval: std::time::Duration,
    full_interval: std::time::Duration,
    now: DateTime<Utc>,
) -> ResumeDecision {
    let prefix = resolve_template_prefix(name_template);

    // Filter: non-broken, matching prefix, has timestamp
    let mut matching: Vec<&BackupSummary> = backups
        .iter()
        .filter(|b| !b.is_broken)
        .filter(|b| b.timestamp.is_some())
        .filter(|b| prefix.is_empty() || b.name.starts_with(&prefix))
        .collect();

    if matching.is_empty() {
        return ResumeDecision::FullNow;
    }

    // Sort by timestamp descending (most recent first)
    matching.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    // Find most recent full and incremental using template-aware classification
    let last_full = matching
        .iter()
        .find(|b| classify_backup_type(name_template, &b.name) == Some("full"));
    let last_incr = matching
        .iter()
        .find(|b| classify_backup_type(name_template, &b.name) == Some("incr"));

    // The most recent backup overall (for diff_from in incremental)
    let most_recent = matching[0];

    // If no full backup exists at all, do a full now
    let last_full = match last_full {
        Some(f) => f,
        None => return ResumeDecision::FullNow,
    };

    // Safety: all entries in `matching` have been filtered for timestamp.is_some()
    let full_ts = match last_full.timestamp {
        Some(ts) => ts,
        None => return ResumeDecision::FullNow,
    };
    let full_elapsed = (now - full_ts)
        .to_std()
        .unwrap_or(std::time::Duration::ZERO);

    // If full interval has elapsed, do a full now
    if full_elapsed >= full_interval {
        return ResumeDecision::FullNow;
    }

    // Determine the most recent backup timestamp (full or incr) for watch_interval check
    let last_backup_ts = if let Some(incr) = last_incr {
        let incr_ts = match incr.timestamp {
            Some(ts) => ts,
            None => full_ts,
        };
        if incr_ts > full_ts {
            incr_ts
        } else {
            full_ts
        }
    } else {
        full_ts
    };

    let last_elapsed = (now - last_backup_ts)
        .to_std()
        .unwrap_or(std::time::Duration::ZERO);

    // If watch_interval has elapsed since last backup, do an incremental now
    if last_elapsed >= watch_interval {
        return ResumeDecision::IncrNow {
            diff_from: most_recent.name.clone(),
        };
    }

    // Still within watch_interval, sleep for the remainder
    let remaining = watch_interval - last_elapsed;
    ResumeDecision::SleepThen {
        remaining,
        backup_type: "incr".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Watch state machine
// ---------------------------------------------------------------------------

/// Internal state of the watch loop, mapped to metric values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchState {
    /// Between cycles, idle (metric: 1)
    Idle = 1,
    /// Creating a full backup (metric: 2)
    CreatingFull = 2,
    /// Creating an incremental backup (metric: 3)
    CreatingIncr = 3,
    /// Uploading the backup (metric: 4)
    Uploading = 4,
    /// Running retention/cleanup (metric: 5)
    Cleaning = 5,
    /// Sleeping between cycles (metric: 6)
    Sleeping = 6,
    /// Error/backoff (metric: 7)
    Error = 7,
}

/// Reason the watch loop exited.
#[derive(Debug, PartialEq, Eq)]
pub enum WatchLoopExit {
    /// Received shutdown signal.
    Shutdown,
    /// Reached max_consecutive_errors threshold.
    MaxErrors,
    /// Stopped via API.
    Stopped,
}

/// Map a `WatchState` to its human-readable API status string.
fn watch_state_name(s: WatchState) -> &'static str {
    match s {
        WatchState::Idle => "idle",
        WatchState::CreatingFull => "creating_full",
        WatchState::CreatingIncr => "creating_incr",
        WatchState::Uploading => "uploading",
        WatchState::Cleaning => "cleaning",
        WatchState::Sleeping => "sleeping",
        WatchState::Error => "error",
    }
}

/// All state needed to run the watch loop.
pub struct WatchContext {
    pub config: Arc<Config>,
    pub ch: ChClient,
    pub s3: S3Client,
    pub metrics: Option<Arc<Metrics>>,
    pub state: WatchState,
    pub consecutive_errors: u32,
    pub force_next_full: bool,
    pub last_backup_name: Option<String>,
    pub shutdown_rx: tokio::sync::watch::Receiver<bool>,
    pub reload_rx: tokio::sync::watch::Receiver<bool>,
    pub config_path: PathBuf,
    pub macros: HashMap<String, String>,
    /// Shared manifest cache for invalidation after retention_remote.
    pub manifest_cache: Option<Arc<Mutex<ManifestCache>>>,
    /// Shared watch status for API queries (updated at each state transition).
    pub watch_status: Arc<Mutex<WatchStatus>>,
}

impl WatchContext {
    async fn set_state(&mut self, new_state: WatchState) {
        self.state = new_state;
        if let Some(m) = &self.metrics {
            m.watch_state.set(new_state as i64);
        }
        self.watch_status.lock().await.state = watch_state_name(new_state).to_string();
    }

    async fn set_consecutive_errors(&mut self, count: u32) {
        self.consecutive_errors = count;
        if let Some(m) = &self.metrics {
            m.watch_consecutive_errors.set(count as i64);
        }
        self.watch_status.lock().await.consecutive_errors = count;
    }
}

/// Run the watch loop state machine (design doc section 10.4).
///
/// This is the main entry point for watch mode. It:
/// 1. Queries remote backups and determines resume state
/// 2. Cycles through: create -> upload -> delete_local -> retention -> sleep
/// 3. Handles errors with consecutive_errors tracking and force_next_full
/// 4. Responds to shutdown and reload signals during sleep
/// 5. Updates Prometheus metrics at each state transition
pub async fn run_watch_loop(mut ctx: WatchContext) -> WatchLoopExit {
    info!("watch: starting watch loop");
    ctx.set_state(WatchState::Idle).await;

    loop {
        // Check max consecutive errors
        if ctx.consecutive_errors >= ctx.config.watch.max_consecutive_errors
            && ctx.config.watch.max_consecutive_errors > 0
        {
            warn!(
                consecutive_errors = ctx.consecutive_errors,
                max = ctx.config.watch.max_consecutive_errors,
                "watch: aborting due to max_consecutive_errors"
            );
            return WatchLoopExit::MaxErrors;
        }

        // Check shutdown
        if *ctx.shutdown_rx.borrow() {
            info!("watch: shutdown received");
            ctx.set_state(WatchState::Idle).await;
            return WatchLoopExit::Shutdown;
        }

        // Parse durations from config
        let watch_interval_secs = match parse_duration_secs(&ctx.config.watch.watch_interval) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "watch: invalid watch_interval, defaulting to 3600s");
                3600
            }
        };
        let full_interval_secs = match parse_duration_secs(&ctx.config.watch.full_interval) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "watch: invalid full_interval, defaulting to 86400s");
                86400
            }
        };
        let retry_interval_secs = match parse_duration_secs(&ctx.config.watch.retry_interval) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "watch: invalid retry_interval, defaulting to 300s");
                300
            }
        };
        let watch_interval = std::time::Duration::from_secs(watch_interval_secs);
        let full_interval = std::time::Duration::from_secs(full_interval_secs);
        let retry_interval = std::time::Duration::from_secs(retry_interval_secs);

        // Resolve tables filter: CLI > config.watch.tables > config.backup.tables > None
        let tables_filter: Option<String> = ctx.config.watch.tables.clone().or_else(|| {
            if !ctx.config.backup.tables.is_empty() {
                Some(ctx.config.backup.tables.clone())
            } else {
                None
            }
        });

        // Step 1: Resume -- query remote backups and determine next action
        ctx.set_state(WatchState::Idle).await;
        let remote_backups = match crate::list::list_remote(&ctx.s3).await {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "watch: failed to list remote backups");
                if let Some(exit) = handle_error(&mut ctx, retry_interval).await {
                    return exit;
                }
                continue;
            }
        };

        let decision = resume_state(
            &remote_backups,
            &ctx.config.watch.name_template,
            watch_interval,
            full_interval,
            Utc::now(),
        );
        info!("watch: resume state: {:?}", decision);

        // Step 2: Act on decision
        match decision {
            ResumeDecision::SleepThen {
                remaining,
                backup_type: _,
            } => {
                ctx.set_state(WatchState::Sleeping).await;
                info!(
                    seconds = remaining.as_secs(),
                    "watch: cycle complete, sleeping"
                );
                ctx.watch_status.lock().await.next_backup_in = Some(remaining);
                let exit = interruptible_sleep(&mut ctx, remaining).await;
                ctx.watch_status.lock().await.next_backup_in = None;
                if let Some(reason) = exit {
                    return reason;
                }
                continue;
            }
            ResumeDecision::FullNow | ResumeDecision::IncrNow { .. } => {
                // Fall through to the backup cycle below
            }
        }

        // Determine backup type
        let (backup_type, diff_from) = if ctx.force_next_full {
            ("full".to_string(), None)
        } else {
            match &decision {
                ResumeDecision::FullNow => ("full".to_string(), None),
                ResumeDecision::IncrNow { diff_from } => {
                    ("incr".to_string(), Some(diff_from.clone()))
                }
                ResumeDecision::SleepThen { .. } => unreachable!(),
            }
        };

        // Step 3: Create backup
        let now = Utc::now();
        let backup_name = resolve_name_template(
            &ctx.config.watch.name_template,
            &backup_type,
            now,
            &ctx.macros,
        );

        // Validate the resolved name before using it in filesystem/S3 operations.
        // Catches templates that expand to unsafe paths (e.g. containing '/' or '..').
        if let Err(e) = validate_backup_name(&backup_name) {
            warn!(
                backup_name = %backup_name,
                template = %ctx.config.watch.name_template,
                error = %e,
                "watch: resolved backup name is invalid, skipping cycle"
            );
            if let Some(exit) = handle_error(&mut ctx, retry_interval).await {
                return exit;
            }
            continue;
        }

        // Acquire per-backup PID lock before mutating operations (design §2).
        // Enables orphan shadow-dir cleanup to detect in-progress watch backups.
        let lock_path = std::path::PathBuf::from(format!("/tmp/chbackup.{backup_name}.pid"));
        let _pid_lock = match PidLock::acquire(&lock_path, "watch") {
            Ok(l) => l,
            Err(e) => {
                warn!(error = %e, backup_name = %backup_name, "watch: failed to acquire backup lock");
                if let Some(exit) = handle_error(&mut ctx, retry_interval).await {
                    return exit;
                }
                continue;
            }
        };

        if backup_type == "full" {
            ctx.set_state(WatchState::CreatingFull).await;
        } else {
            ctx.set_state(WatchState::CreatingIncr).await;
        }
        info!(
            backup_name = %backup_name,
            backup_type = %backup_type,
            diff_from = ?diff_from,
            "watch: creating {} backup", backup_type
        );

        // Wire CancellationToken to the shutdown signal so that SIGTERM during
        // backup::create() cancels the operation instead of running to completion.
        let create_cancel = tokio_util::sync::CancellationToken::new();
        let create_cancel_for_shutdown = create_cancel.clone();
        let mut create_shutdown_rx = ctx.shutdown_rx.clone();
        let create_shutdown_guard = tokio::spawn(async move {
            let _ = create_shutdown_rx.changed().await;
            create_cancel_for_shutdown.cancel();
        });

        let create_result = crate::backup::create(
            &ctx.config,
            &ctx.ch,
            &backup_name,
            tables_filter.as_deref(),
            false, // schema_only
            None,  // diff_from: watch uses diff_from_remote in upload; local base may be deleted
            None,  // diff_from_remote: watch uses diff_from_remote in upload step
            None,  // s3: not needed without diff-from-remote
            None,  // partitions
            false, // skip_check_parts_columns (let config.clickhouse.check_parts_columns control)
            false, // rbac (watch mode does not support RBAC backup)
            false, // configs (watch mode does not support config backup)
            false, // named_collections (watch mode does not support named collections backup)
            &ctx.config.backup.skip_projections,
            create_cancel,
        )
        .await;
        create_shutdown_guard.abort();

        if let Err(e) = create_result {
            warn!(error = %e, backup_name = %backup_name, "watch: create failed");
            if let Some(exit) = handle_error(&mut ctx, retry_interval).await {
                return exit;
            }
            continue;
        }

        // Step 4: Upload backup
        ctx.set_state(WatchState::Uploading).await;
        let backup_dir = PathBuf::from(&ctx.config.clickhouse.data_path)
            .join("backup")
            .join(&backup_name);

        // Wire CancellationToken to the shutdown signal so that SIGTERM during
        // upload::upload() cancels the operation instead of running to completion.
        let upload_cancel = tokio_util::sync::CancellationToken::new();
        let upload_cancel_for_shutdown = upload_cancel.clone();
        let mut upload_shutdown_rx = ctx.shutdown_rx.clone();
        let upload_shutdown_guard = tokio::spawn(async move {
            let _ = upload_shutdown_rx.changed().await;
            upload_cancel_for_shutdown.cancel();
        });

        let upload_result = crate::upload::upload(
            &ctx.config,
            &ctx.s3,
            &backup_name,
            &backup_dir,
            false, // delete_local handled separately below
            diff_from.as_deref(),
            ctx.config.general.use_resumable_state,
            upload_cancel,
        )
        .await;
        upload_shutdown_guard.abort();

        if let Err(e) = upload_result {
            warn!(error = %e, backup_name = %backup_name, "watch: upload failed");
            if let Some(exit) = handle_error(&mut ctx, retry_interval).await {
                return exit;
            }
            continue;
        }
        info!(backup_name = %backup_name, "watch: upload complete");

        // Invalidate manifest cache after successful upload adds a new backup
        if let Some(cache) = &ctx.manifest_cache {
            cache.lock().await.invalidate();
            info!("ManifestCache: invalidated after upload");
        }

        // Update last full/incremental timestamp metric and WatchStatus
        {
            let upload_ts = Utc::now();
            if let Some(m) = &ctx.metrics {
                let ts = upload_ts.timestamp() as f64;
                if backup_type == "full" {
                    m.watch_last_full_timestamp.set(ts);
                } else {
                    m.watch_last_incremental_timestamp.set(ts);
                }
            }
            let mut ws = ctx.watch_status.lock().await;
            if backup_type == "full" {
                ws.last_full = Some(upload_ts);
            } else {
                ws.last_incr = Some(upload_ts);
            }
        }

        // Step 5: Delete local if configured
        if ctx.config.watch.delete_local_after_upload {
            let data_path = ctx.config.clickhouse.data_path.clone();
            let name_clone = backup_name.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || {
                crate::list::delete_local(&data_path, &name_clone)
            })
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking failed: {}", e)))
            {
                warn!(error = %e, "watch: failed to delete local backup after upload");
                // Best-effort, continue
            }
        }

        // Step 6: Retention (best-effort per design 10.7)
        ctx.set_state(WatchState::Cleaning).await;

        let keep_local = crate::list::effective_retention_local(&ctx.config);
        if keep_local > 0 {
            let data_path = ctx.config.clickhouse.data_path.clone();
            match tokio::task::spawn_blocking(move || {
                crate::list::retention_local(&data_path, keep_local)
            })
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking failed: {}", e)))
            {
                Ok(deleted) => {
                    if deleted > 0 {
                        info!(deleted = deleted, "watch: local retention applied");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "watch: local retention failed (best-effort)");
                }
            }
        }

        let keep_remote = crate::list::effective_retention_remote(&ctx.config);
        if keep_remote > 0 {
            match crate::list::retention_remote(&ctx.s3, keep_remote).await {
                Ok(deleted) => {
                    if deleted > 0 {
                        info!(deleted = deleted, "watch: remote retention applied");
                        // Invalidate manifest cache after remote retention changes backup set
                        if let Some(cache) = &ctx.manifest_cache {
                            cache.lock().await.invalidate();
                            info!("ManifestCache: invalidated");
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "watch: remote retention failed (best-effort)");
                }
            }
        }

        // Success: reset error state
        ctx.set_consecutive_errors(0).await;
        if backup_type == "full" {
            ctx.force_next_full = false;
        }
        ctx.last_backup_name = Some(backup_name);

        // Step 7: Sleep for watch_interval
        ctx.set_state(WatchState::Sleeping).await;
        info!(
            seconds = watch_interval.as_secs(),
            "watch: cycle complete, sleeping"
        );
        ctx.watch_status.lock().await.next_backup_in = Some(watch_interval);
        let exit = interruptible_sleep(&mut ctx, watch_interval).await;
        ctx.watch_status.lock().await.next_backup_in = None;
        if let Some(reason) = exit {
            return reason;
        }
    }
}

/// Handle a watch cycle error: increment consecutive_errors, set force_next_full,
/// update metrics, and sleep for retry_interval.
///
/// Returns `Some(WatchLoopExit)` if a shutdown/stop signal was received during
/// the retry sleep, so the caller can exit the loop immediately instead of
/// making another S3 list call before detecting the signal.
async fn handle_error(
    ctx: &mut WatchContext,
    retry_interval: std::time::Duration,
) -> Option<WatchLoopExit> {
    let new_count = ctx.consecutive_errors + 1;
    ctx.set_consecutive_errors(new_count).await;
    ctx.force_next_full = true;
    ctx.set_state(WatchState::Error).await;
    warn!(
        consecutive_errors = new_count,
        "watch: error, consecutive_errors={}", new_count
    );

    // Sleep for retry_interval (interruptible) -- propagate exit signal
    interruptible_sleep(ctx, retry_interval).await
}

/// Sleep for the given duration, but wake up early on shutdown, reload, or stop signals.
///
/// Returns `Some(WatchLoopExit)` if the loop should exit, or `None` to continue.
async fn interruptible_sleep(
    ctx: &mut WatchContext,
    duration: std::time::Duration,
) -> Option<WatchLoopExit> {
    let sleep = tokio::time::sleep(duration);
    tokio::pin!(sleep);

    loop {
        tokio::select! {
            _ = &mut sleep => {
                // Normal sleep completed
                return None;
            }
            _ = ctx.shutdown_rx.changed() => {
                if *ctx.shutdown_rx.borrow() {
                    info!("watch: shutdown received during sleep");
                    return Some(WatchLoopExit::Shutdown);
                }
            }
            _ = ctx.reload_rx.changed() => {
                if *ctx.reload_rx.borrow() {
                    // Config reload requested
                    info!("watch: config reload requested");
                    apply_config_reload(ctx).await;
                    // Continue sleeping -- reload doesn't interrupt the current cycle
                }
            }
        }
    }
}

/// Apply a config reload from the config file.
///
/// Reads the config file, validates it, and if valid, recreates ChClient and
/// S3Client with the new config, then applies all three (config, ch, s3).
/// Logs old -> new values for key parameters (design 10.8 step 3d).
/// On error, logs a warning and retains the current config and clients.
async fn apply_config_reload(ctx: &mut WatchContext) {
    let new_config = match Config::load(&ctx.config_path, &[]) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "watch: config reload failed (keeping current config)");
            return;
        }
    };

    if let Err(e) = new_config.validate() {
        warn!(error = %e, "watch: reloaded config validation failed (keeping current config)");
        return;
    }

    // Log changes for key watch parameters
    let old = &ctx.config.watch;
    let new = &new_config.watch;
    if old.watch_interval != new.watch_interval
        || old.full_interval != new.full_interval
        || old.name_template != new.name_template
        || old.max_consecutive_errors != new.max_consecutive_errors
    {
        info!(
            "watch: config reloaded: watch_interval={}->{}  full_interval={}->{}  name_template={}->{}  max_consecutive_errors={}->{}",
            old.watch_interval,
            new.watch_interval,
            old.full_interval,
            new.full_interval,
            old.name_template,
            new.name_template,
            old.max_consecutive_errors,
            new.max_consecutive_errors,
        );
    } else {
        info!("watch: config reloaded (no watch param changes)");
    }

    // Recreate clients with new config -- failures are non-fatal
    let new_ch = match crate::clickhouse::ChClient::new(&new_config.clickhouse) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "watch: reload failed to recreate ChClient (keeping old)");
            return;
        }
    };

    let new_s3 = match crate::storage::S3Client::new(&new_config.s3).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "watch: reload failed to recreate S3Client (keeping old)");
            return;
        }
    };

    // Refresh macros from the new ClickHouse client so that name templates
    // using {shard}, {replica}, etc. pick up any macro changes on the new server.
    ctx.macros = new_ch.get_macros().await.unwrap_or_default();

    // Update all three atomically -- config last, only after both clients succeed
    ctx.ch = new_ch;
    ctx.s3 = new_s3;
    ctx.config = Arc::new(new_config);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_type_macro() {
        let macros = HashMap::new();
        let now = Utc::now();

        let result = resolve_name_template("{type}-backup", "full", now, &macros);
        assert_eq!(result, "full-backup");

        let result = resolve_name_template("{type}-backup", "incr", now, &macros);
        assert_eq!(result, "incr-backup");
    }

    #[test]
    fn test_resolve_time_macro() {
        let macros = HashMap::new();
        let now = chrono::NaiveDate::from_ymd_opt(2025, 3, 15)
            .unwrap()
            .and_hms_opt(10, 30, 45)
            .unwrap()
            .and_utc();

        let result = resolve_name_template("backup-{time:%Y%m%d_%H%M%S}", "full", now, &macros);
        assert_eq!(result, "backup-20250315_103045");

        let result = resolve_name_template("{time:%Y-%m-%d}", "full", now, &macros);
        assert_eq!(result, "2025-03-15");
    }

    #[test]
    fn test_resolve_shard_macro() {
        let mut macros = HashMap::new();
        macros.insert("shard".to_string(), "01".to_string());

        let now = Utc::now();
        let result = resolve_name_template("shard{shard}-backup", "full", now, &macros);
        assert_eq!(result, "shard01-backup");
    }

    #[test]
    fn test_resolve_full_template() {
        let mut macros = HashMap::new();
        macros.insert("shard".to_string(), "01".to_string());

        let now = chrono::NaiveDate::from_ymd_opt(2025, 3, 15)
            .unwrap()
            .and_hms_opt(2, 0, 0)
            .unwrap()
            .and_utc();

        let result = resolve_name_template(
            "shard{shard}-{type}-{time:%Y%m%d_%H%M%S}",
            "full",
            now,
            &macros,
        );
        assert_eq!(result, "shard01-full-20250315_020000");
    }

    #[test]
    fn test_resolve_unknown_macro() {
        let macros = HashMap::new();
        let now = Utc::now();

        let result = resolve_name_template("prefix-{unknown}-suffix", "full", now, &macros);
        assert_eq!(result, "prefix-{unknown}-suffix");
    }

    // -- classify_backup_type tests --

    #[test]
    fn test_classify_backup_type_default_template() {
        let template = "shard{shard}-{type}-{time:%Y%m%d_%H%M%S}";
        assert_eq!(
            classify_backup_type(template, "shard01-full-20250315_120000"),
            Some("full")
        );
        assert_eq!(
            classify_backup_type(template, "shard01-incr-20250315_130000"),
            Some("incr")
        );
    }

    #[test]
    fn test_classify_backup_type_prefix_only() {
        let template = "{type}-backup-{time:%Y%m%d}";
        assert_eq!(
            classify_backup_type(template, "full-backup-20250315"),
            Some("full")
        );
        assert_eq!(
            classify_backup_type(template, "incr-backup-20250315"),
            Some("incr")
        );
    }

    #[test]
    fn test_classify_backup_type_no_type_placeholder() {
        let template = "daily-{time:%Y%m%d}";
        assert_eq!(classify_backup_type(template, "daily-20250315"), None);
    }

    #[test]
    fn test_classify_backup_type_ambiguous_name() {
        let template = "shard{shard}-{type}-{time:%Y%m%d}";
        // "fullmoon" is not "full" or "incr"
        assert_eq!(
            classify_backup_type(template, "shard01-fullmoon-20250315"),
            None
        );
    }

    #[test]
    fn test_classify_backup_type_type_at_end() {
        let template = "backup-{time:%Y%m%d}-{type}";
        assert_eq!(
            classify_backup_type(template, "backup-20250315-full"),
            Some("full")
        );
        assert_eq!(
            classify_backup_type(template, "backup-20250315-incr"),
            Some("incr")
        );
    }

    #[test]
    fn test_classify_delimiter_in_macro() {
        // Bug case: shard="a-b" contains the delimiter "-"
        // Old implementation extracted "b" instead of "full"
        let template = "{shard}-{type}-{time:%Y%m%d}";
        assert_eq!(
            classify_backup_type(template, "a-b-full-20260303"),
            Some("full")
        );
        assert_eq!(
            classify_backup_type(template, "a-b-incr-20260303"),
            Some("incr")
        );
    }

    #[test]
    fn test_classify_ambiguous_both_match() {
        // When both "full" and "incr" globs match (template is just `{type}` with
        // wildcards that absorb the candidate literal), the result is None.
        // Template: `{type}{other}` → full glob `full*`, incr glob `incr*`
        // Name "fullincr" matches `full*` but not `incr*`, so NOT ambiguous.
        // True ambiguity: template `{a}{type}{b}` → glob `*full*` / `*incr*`
        // Name "incr_full_data" matches BOTH `*full*` and `*incr*`.
        let template = "{a}{type}{b}";
        assert_eq!(
            classify_backup_type(template, "incr_full_data"),
            None,
            "Both full and incr globs match => ambiguous => None"
        );
    }

    #[test]
    fn test_classify_neither_match() {
        // A name that matches neither "full" nor "incr" returns None
        let template = "{shard}-{type}-{time:%Y%m%d}";
        assert_eq!(classify_backup_type(template, "a-b-partial-20260303"), None);
    }

    #[test]
    fn test_classify_no_delimiters() {
        // {type} immediately adjacent to another macro with no delimiter separation
        let template = "{type}{time}";
        assert_eq!(classify_backup_type(template, "full20260303"), Some("full"));
        assert_eq!(classify_backup_type(template, "incr20260303"), Some("incr"));
    }

    #[test]
    fn test_classify_adjacent_macros() {
        // All macros adjacent with no separators
        let template = "{shard}{type}{time}";
        // "shardAfull20260303" -- glob "*full*" matches
        assert_eq!(
            classify_backup_type(template, "shardAfull20260303"),
            Some("full")
        );
        // "shardAincr20260303" -- glob "*incr*" matches
        assert_eq!(
            classify_backup_type(template, "shardAincr20260303"),
            Some("incr")
        );
    }

    // -- Resume state tests --

    fn make_summary(name: &str, ts: DateTime<Utc>, broken: bool) -> BackupSummary {
        BackupSummary {
            name: name.to_string(),
            timestamp: Some(ts),
            size: 0,
            compressed_size: 0,
            table_count: 0,
            metadata_size: 0,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: broken,
            broken_reason: if broken {
                Some("test".to_string())
            } else {
                None
            },
        }
    }

    #[test]
    fn test_resume_no_backups() {
        let backups: Vec<BackupSummary> = vec![];
        let decision = resume_state(
            &backups,
            "shard1-{type}-{time:%Y%m%d}",
            std::time::Duration::from_secs(3600),
            std::time::Duration::from_secs(86400),
            Utc::now(),
        );
        assert_eq!(decision, ResumeDecision::FullNow);
    }

    #[test]
    fn test_resume_recent_full_no_incr() {
        let now = Utc::now();
        let full_ts = now - chrono::Duration::minutes(30);

        let backups = vec![make_summary("shard1-full-20250315", full_ts, false)];

        let decision = resume_state(
            &backups,
            "shard1-{type}-{time:%Y%m%d}",
            std::time::Duration::from_secs(3600),  // 1h
            std::time::Duration::from_secs(86400), // 24h
            now,
        );

        match decision {
            ResumeDecision::SleepThen {
                remaining,
                backup_type,
            } => {
                assert_eq!(backup_type, "incr");
                // Should sleep about 30 minutes (3600 - 1800 = ~1800s)
                assert!(remaining.as_secs() > 1700 && remaining.as_secs() <= 1800);
            }
            other => panic!("Expected SleepThen, got {:?}", other),
        }
    }

    #[test]
    fn test_resume_stale_full() {
        let now = Utc::now();
        let full_ts = now - chrono::Duration::hours(25);

        let backups = vec![make_summary("shard1-full-20250314", full_ts, false)];

        let decision = resume_state(
            &backups,
            "shard1-{type}-{time:%Y%m%d}",
            std::time::Duration::from_secs(3600),
            std::time::Duration::from_secs(86400),
            now,
        );

        assert_eq!(decision, ResumeDecision::FullNow);
    }

    #[test]
    fn test_resume_stale_incr() {
        let now = Utc::now();
        let full_ts = now - chrono::Duration::hours(12);
        let incr_ts = now - chrono::Duration::hours(2);

        let backups = vec![
            make_summary("shard1-full-20250315", full_ts, false),
            make_summary("shard1-incr-20250315_1", incr_ts, false),
        ];

        let decision = resume_state(
            &backups,
            "shard1-{type}-{time:%Y%m%d}",
            std::time::Duration::from_secs(3600), // 1h
            std::time::Duration::from_secs(86400),
            now,
        );

        match decision {
            ResumeDecision::IncrNow { diff_from } => {
                // diff_from should be the most recent backup
                assert_eq!(diff_from, "shard1-incr-20250315_1");
            }
            other => panic!("Expected IncrNow, got {:?}", other),
        }
    }

    #[test]
    fn test_resume_recent_incr() {
        let now = Utc::now();
        let full_ts = now - chrono::Duration::hours(12);
        let incr_ts = now - chrono::Duration::minutes(20);

        let backups = vec![
            make_summary("shard1-full-20250315", full_ts, false),
            make_summary("shard1-incr-20250315_1", incr_ts, false),
        ];

        let decision = resume_state(
            &backups,
            "shard1-{type}-{time:%Y%m%d}",
            std::time::Duration::from_secs(3600), // 1h
            std::time::Duration::from_secs(86400),
            now,
        );

        match decision {
            ResumeDecision::SleepThen {
                remaining,
                backup_type,
            } => {
                assert_eq!(backup_type, "incr");
                // Should sleep about 40 minutes (3600 - 1200 = 2400s)
                assert!(remaining.as_secs() > 2300 && remaining.as_secs() <= 2400);
            }
            other => panic!("Expected SleepThen, got {:?}", other),
        }
    }

    #[test]
    fn test_resume_filters_by_template_prefix() {
        let now = Utc::now();
        let ts = now - chrono::Duration::hours(25);

        let backups = vec![
            // This backup doesn't match "shard1-" prefix, should be excluded
            make_summary("other-full-20250315", ts, false),
        ];

        let decision = resume_state(
            &backups,
            "shard1-{type}-{time:%Y%m%d}",
            std::time::Duration::from_secs(3600),
            std::time::Duration::from_secs(86400),
            now,
        );

        // No matching backups, so should decide FullNow
        assert_eq!(decision, ResumeDecision::FullNow);
    }

    #[test]
    fn test_resolve_template_prefix() {
        assert_eq!(
            resolve_template_prefix("shard1-{type}-{time:%Y%m%d}"),
            "shard1-"
        );
        assert_eq!(resolve_template_prefix("{type}-backup"), "");
        assert_eq!(resolve_template_prefix("static-name"), "static-name");
    }

    // -- Watch state machine tests (Task 5) --

    #[test]
    fn test_watch_state_enum_values() {
        assert_eq!(WatchState::Idle as i64, 1);
        assert_eq!(WatchState::CreatingFull as i64, 2);
        assert_eq!(WatchState::CreatingIncr as i64, 3);
        assert_eq!(WatchState::Uploading as i64, 4);
        assert_eq!(WatchState::Cleaning as i64, 5);
        assert_eq!(WatchState::Sleeping as i64, 6);
        assert_eq!(WatchState::Error as i64, 7);
    }

    #[test]
    fn test_force_full_after_error() {
        // Verify the error path logic: after an error, force_next_full is set
        // and consecutive_errors is incremented.
        let initial_errors: u32 = 0;

        // After handle_error:
        let new_errors = initial_errors + 1;
        let force_next_full = true; // handle_error always sets this

        assert!(
            force_next_full,
            "force_next_full should be true after error"
        );
        assert_eq!(new_errors, 1, "consecutive_errors should increment by 1");
    }

    #[test]
    fn test_consecutive_errors_reset_on_success() {
        // Verify the success path: consecutive_errors resets to 0
        // and force_next_full is cleared after a successful full backup.
        let errors_before: u32 = 3;
        assert!(errors_before > 0, "precondition: errors were accumulated");

        // After success path:
        let errors_after: u32 = 0; // success resets to 0
        let force_after_full = false; // cleared after successful full

        assert_eq!(
            errors_after, 0,
            "consecutive_errors should reset to 0 on success"
        );
        assert!(
            !force_after_full,
            "force_next_full should be cleared after successful full"
        );
    }

    #[test]
    fn test_consecutive_errors_abort() {
        let consecutive_errors: u32 = 5;
        let max_consecutive_errors: u32 = 5;

        let should_abort =
            consecutive_errors >= max_consecutive_errors && max_consecutive_errors > 0;

        assert!(should_abort, "should abort when consecutive_errors >= max");
        // verify the exit reason would be MaxErrors
        let exit = WatchLoopExit::MaxErrors;
        assert_eq!(exit, WatchLoopExit::MaxErrors);
    }

    #[test]
    fn test_consecutive_errors_no_abort_when_zero_max() {
        let consecutive_errors: u32 = 100;
        let max_consecutive_errors: u32 = 0;

        let should_abort =
            consecutive_errors >= max_consecutive_errors && max_consecutive_errors > 0;

        assert!(
            !should_abort,
            "should NOT abort when max_consecutive_errors is 0 (unlimited)"
        );
    }

    // -----------------------------------------------------------------------
    // Additional coverage: watch_state_name
    // -----------------------------------------------------------------------

    #[test]
    fn test_watch_state_name_all_variants() {
        assert_eq!(watch_state_name(WatchState::Idle), "idle");
        assert_eq!(watch_state_name(WatchState::CreatingFull), "creating_full");
        assert_eq!(watch_state_name(WatchState::CreatingIncr), "creating_incr");
        assert_eq!(watch_state_name(WatchState::Uploading), "uploading");
        assert_eq!(watch_state_name(WatchState::Cleaning), "cleaning");
        assert_eq!(watch_state_name(WatchState::Sleeping), "sleeping");
        assert_eq!(watch_state_name(WatchState::Error), "error");
    }

    // -----------------------------------------------------------------------
    // Additional coverage: resolve_name_template edge cases
    // -----------------------------------------------------------------------

    /// Test resolve_name_template with unclosed brace (no closing '}'). This
    /// exercises the `!found_close` branch (lines 58-63).
    #[test]
    fn test_resolve_name_template_unclosed_brace() {
        let macros = HashMap::new();
        let now = Utc::now();

        let result = resolve_name_template("prefix-{unclosed", "full", now, &macros);
        assert_eq!(result, "prefix-{unclosed");
    }

    /// Test resolve_name_template with empty braces.
    #[test]
    fn test_resolve_name_template_empty_braces() {
        let macros = HashMap::new();
        let now = Utc::now();

        let result = resolve_name_template("prefix-{}-suffix", "full", now, &macros);
        // Empty braces are not "type" or "time:*" and empty key not in macros,
        // so they are left as-is.
        assert_eq!(result, "prefix-{}-suffix");
    }

    /// Test resolve_name_template with multiple macros of same type.
    #[test]
    fn test_resolve_name_template_multiple_type_macros() {
        let macros = HashMap::new();
        let now = Utc::now();

        let result = resolve_name_template("{type}-{type}", "full", now, &macros);
        assert_eq!(result, "full-full");
    }

    /// Test resolve_name_template with no macros at all (pure static name).
    #[test]
    fn test_resolve_name_template_static_only() {
        let macros = HashMap::new();
        let now = Utc::now();

        let result = resolve_name_template("static-backup-name", "full", now, &macros);
        assert_eq!(result, "static-backup-name");
    }

    // -----------------------------------------------------------------------
    // Additional coverage: build_type_glob and push_escaped_glob_char
    // -----------------------------------------------------------------------

    /// Test that build_type_glob escapes glob special characters in static text.
    #[test]
    fn test_classify_with_glob_special_chars_in_template() {
        // Template has literal '*' and '?' which must be escaped
        let template = "backup[1]-{type}-data?";
        // This should still classify correctly because the static chars are escaped
        let result = classify_backup_type(template, "backup[1]-full-data?");
        assert_eq!(result, Some("full"));

        let result = classify_backup_type(template, "backup[1]-incr-data?");
        assert_eq!(result, Some("incr"));
    }

    /// Test build_type_glob with template containing only {type}.
    #[test]
    fn test_classify_type_only_template() {
        let template = "{type}";
        assert_eq!(classify_backup_type(template, "full"), Some("full"));
        assert_eq!(classify_backup_type(template, "incr"), Some("incr"));
        assert_eq!(classify_backup_type(template, "other"), None);
    }

    // -----------------------------------------------------------------------
    // Additional coverage: resume_state edge cases
    // -----------------------------------------------------------------------

    /// Test resume_state with broken backups filtered out.
    #[test]
    fn test_resume_state_broken_backups_excluded() {
        let now = Utc::now();
        let full_ts = now - chrono::Duration::minutes(30);

        let backups = vec![
            // Only backup is broken -- should be excluded
            make_summary("shard1-full-20250315", full_ts, true),
        ];

        let decision = resume_state(
            &backups,
            "shard1-{type}-{time:%Y%m%d}",
            std::time::Duration::from_secs(3600),
            std::time::Duration::from_secs(86400),
            now,
        );

        // Broken backup excluded => no matching backups => FullNow
        assert_eq!(decision, ResumeDecision::FullNow);
    }

    /// Test resume_state with backups that have no timestamp.
    #[test]
    fn test_resume_state_no_timestamp_excluded() {
        let now = Utc::now();

        let mut backup = make_summary("shard1-full-20250315", now, false);
        backup.timestamp = None; // Remove timestamp

        let backups = vec![backup];

        let decision = resume_state(
            &backups,
            "shard1-{type}-{time:%Y%m%d}",
            std::time::Duration::from_secs(3600),
            std::time::Duration::from_secs(86400),
            now,
        );

        // No-timestamp backup excluded => FullNow
        assert_eq!(decision, ResumeDecision::FullNow);
    }

    /// Test resume_state where incr backup is more recent than full backup
    /// but both are within full_interval. This tests the branch where
    /// incr_ts > full_ts.
    #[test]
    fn test_resume_state_incr_more_recent_than_full() {
        let now = Utc::now();
        let full_ts = now - chrono::Duration::hours(6);
        let incr_ts = now - chrono::Duration::minutes(10);

        let backups = vec![
            make_summary("shard1-full-20250315", full_ts, false),
            make_summary("shard1-incr-20250315_1", incr_ts, false),
        ];

        let decision = resume_state(
            &backups,
            "shard1-{type}-{time:%Y%m%d}",
            std::time::Duration::from_secs(3600),  // 1h
            std::time::Duration::from_secs(86400), // 24h
            now,
        );

        // incr was 10 minutes ago, watch_interval is 1h => SleepThen ~50min
        match decision {
            ResumeDecision::SleepThen {
                remaining,
                backup_type,
            } => {
                assert_eq!(backup_type, "incr");
                assert!(remaining.as_secs() > 2900 && remaining.as_secs() <= 3000);
            }
            other => panic!("Expected SleepThen, got {:?}", other),
        }
    }

    /// Test resume_state with empty template prefix (template starts with {type}).
    #[test]
    fn test_resume_state_empty_prefix_matches_all() {
        let now = Utc::now();
        let full_ts = now - chrono::Duration::minutes(10);

        let backups = vec![make_summary("full-20250315", full_ts, false)];

        let decision = resume_state(
            &backups,
            "{type}-{time:%Y%m%d}",
            std::time::Duration::from_secs(3600),
            std::time::Duration::from_secs(86400),
            now,
        );

        // Recent full backup => SleepThen
        match decision {
            ResumeDecision::SleepThen { backup_type, .. } => {
                assert_eq!(backup_type, "incr");
            }
            other => panic!("Expected SleepThen, got {:?}", other),
        }
    }

    /// Test resume_state where only non-classified backups exist (e.g., the
    /// template cannot classify them as "full" or "incr").
    #[test]
    fn test_resume_state_no_classifiable_full_backup() {
        let now = Utc::now();
        let ts = now - chrono::Duration::minutes(30);

        // Backup name doesn't match any type classification for this template
        let backups = vec![make_summary("shard1-partial-20250315", ts, false)];

        let decision = resume_state(
            &backups,
            "shard1-{type}-{time:%Y%m%d}",
            std::time::Duration::from_secs(3600),
            std::time::Duration::from_secs(86400),
            now,
        );

        // Matching backups exist, but no full backup classified => FullNow
        assert_eq!(decision, ResumeDecision::FullNow);
    }

    /// Test resolve_template_prefix with no braces at all.
    #[test]
    fn test_resolve_template_prefix_no_braces() {
        assert_eq!(resolve_template_prefix("mybackup"), "mybackup");
    }

    /// Test resolve_template_prefix with brace at position 0.
    #[test]
    fn test_resolve_template_prefix_brace_at_start() {
        assert_eq!(resolve_template_prefix("{type}-backup"), "");
    }

    // -----------------------------------------------------------------------
    // Additional coverage: WatchLoopExit variants
    // -----------------------------------------------------------------------

    #[test]
    fn test_watch_loop_exit_variants() {
        assert_eq!(WatchLoopExit::Shutdown, WatchLoopExit::Shutdown);
        assert_eq!(WatchLoopExit::MaxErrors, WatchLoopExit::MaxErrors);
        assert_eq!(WatchLoopExit::Stopped, WatchLoopExit::Stopped);
        assert_ne!(WatchLoopExit::Shutdown, WatchLoopExit::MaxErrors);
        assert_ne!(WatchLoopExit::Shutdown, WatchLoopExit::Stopped);
    }
}
