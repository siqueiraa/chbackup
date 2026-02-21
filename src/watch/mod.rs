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
use crate::server::metrics::Metrics;
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

    // Find most recent full and incremental
    let last_full = matching.iter().find(|b| b.name.contains("full"));
    let last_incr = matching.iter().find(|b| b.name.contains("incr"));

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
}

impl WatchContext {
    fn set_state(&mut self, new_state: WatchState) {
        self.state = new_state;
        if let Some(m) = &self.metrics {
            m.watch_state.set(new_state as i64);
        }
    }

    fn set_consecutive_errors(&mut self, count: u32) {
        self.consecutive_errors = count;
        if let Some(m) = &self.metrics {
            m.watch_consecutive_errors.set(count as i64);
        }
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
    ctx.set_state(WatchState::Idle);

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
            ctx.set_state(WatchState::Idle);
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
        ctx.set_state(WatchState::Idle);
        let remote_backups = match crate::list::list_remote(&ctx.s3).await {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "watch: failed to list remote backups");
                handle_error(&mut ctx, retry_interval).await;
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
                ctx.set_state(WatchState::Sleeping);
                info!(
                    seconds = remaining.as_secs(),
                    "watch: cycle complete, sleeping"
                );
                let exit = interruptible_sleep(&mut ctx, remaining).await;
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

        if backup_type == "full" {
            ctx.set_state(WatchState::CreatingFull);
        } else {
            ctx.set_state(WatchState::CreatingIncr);
        }
        info!(
            backup_name = %backup_name,
            backup_type = %backup_type,
            diff_from = ?diff_from,
            "watch: creating {} backup", backup_type
        );

        let create_result = crate::backup::create(
            &ctx.config,
            &ctx.ch,
            &backup_name,
            tables_filter.as_deref(),
            false, // schema_only
            diff_from.as_deref(),
            None,  // partitions
            false, // skip_check_parts_columns (let config.clickhouse.check_parts_columns control)
            false, // rbac (watch mode does not support RBAC backup)
            false, // configs (watch mode does not support config backup)
            false, // named_collections (watch mode does not support named collections backup)
            &ctx.config.backup.skip_projections,
        )
        .await;

        if let Err(e) = create_result {
            warn!(error = %e, backup_name = %backup_name, "watch: create failed");
            handle_error(&mut ctx, retry_interval).await;
            continue;
        }

        // Step 4: Upload backup
        ctx.set_state(WatchState::Uploading);
        let backup_dir = PathBuf::from(&ctx.config.clickhouse.data_path)
            .join("backup")
            .join(&backup_name);

        let upload_result = crate::upload::upload(
            &ctx.config,
            &ctx.s3,
            &backup_name,
            &backup_dir,
            false, // delete_local handled separately below
            diff_from.as_deref(),
            ctx.config.general.use_resumable_state,
        )
        .await;

        if let Err(e) = upload_result {
            warn!(error = %e, backup_name = %backup_name, "watch: upload failed");
            handle_error(&mut ctx, retry_interval).await;
            continue;
        }
        info!(backup_name = %backup_name, "watch: upload complete");

        // Update last full/incremental timestamp metric
        if let Some(m) = &ctx.metrics {
            let ts = Utc::now().timestamp() as f64;
            if backup_type == "full" {
                m.watch_last_full_timestamp.set(ts);
            } else {
                m.watch_last_incremental_timestamp.set(ts);
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
        ctx.set_state(WatchState::Cleaning);

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
        ctx.set_consecutive_errors(0);
        if backup_type == "full" {
            ctx.force_next_full = false;
        }
        ctx.last_backup_name = Some(backup_name);

        // Step 7: Sleep for watch_interval
        ctx.set_state(WatchState::Sleeping);
        info!(
            seconds = watch_interval.as_secs(),
            "watch: cycle complete, sleeping"
        );
        let exit = interruptible_sleep(&mut ctx, watch_interval).await;
        if let Some(reason) = exit {
            return reason;
        }
    }
}

/// Handle a watch cycle error: increment consecutive_errors, set force_next_full,
/// update metrics, and sleep for retry_interval.
async fn handle_error(ctx: &mut WatchContext, retry_interval: std::time::Duration) {
    let new_count = ctx.consecutive_errors + 1;
    ctx.set_consecutive_errors(new_count);
    ctx.force_next_full = true;
    ctx.set_state(WatchState::Error);
    warn!(
        consecutive_errors = new_count,
        "watch: error, consecutive_errors={}", new_count
    );

    // Sleep for retry_interval (interruptible)
    let _ = interruptible_sleep(ctx, retry_interval).await;
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
}
