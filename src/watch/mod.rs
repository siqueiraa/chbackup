//! Watch mode scheduler for automatic backup chains (design doc section 10).
//!
//! This module implements the state machine loop that maintains a rolling chain
//! of full + incremental backups. It includes name template resolution,
//! resume-on-restart logic, and integration with the server API endpoints.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::list::BackupSummary;

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

    let full_ts = last_full.timestamp.unwrap();
    let full_elapsed = (now - full_ts)
        .to_std()
        .unwrap_or(std::time::Duration::ZERO);

    // If full interval has elapsed, do a full now
    if full_elapsed >= full_interval {
        return ResumeDecision::FullNow;
    }

    // Determine the most recent backup timestamp (full or incr) for watch_interval check
    let last_backup_ts = if let Some(incr) = last_incr {
        let incr_ts = incr.timestamp.unwrap();
        if incr_ts > full_ts { incr_ts } else { full_ts }
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
}
