# Preventive Rules Applied

## Rules Checked

| Rule | Applicable? | Check Result | Notes |
|------|-------------|--------------|-------|
| RC-001 | NO | N/A | No Kameo actors in this project. chbackup uses flat async tasks, not actors. |
| RC-002 | YES | CHECKED | Verified `ApiConfig.enable_metrics` is `bool` at config.rs:432. Verified `BackupManifest.compressed_size` is `u64` at manifest.rs:44. |
| RC-003 | YES | WILL APPLY | Must update tracking files after implementation. |
| RC-004 | NO | N/A | No Kameo message types. |
| RC-005 | NO | N/A | No division operations in metrics code. |
| RC-006 | YES | CHECKED | All APIs verified: `prometheus::Registry`, `prometheus::TextEncoder`, `prometheus::Histogram`, `prometheus::HistogramOpts`, `prometheus::IntCounter`, `prometheus::IntGauge`, `prometheus::Encoder`. Using `prometheus` v0.13 crate. |
| RC-007 | NO | N/A | No tuple types involved. |
| RC-008 | YES | WILL APPLY | Will ensure struct fields exist before tests reference them. |
| RC-010 | NO | N/A | No adapter stubs. |
| RC-011 | NO | N/A | No state machine flags. The `in_progress` gauge is set/unset at operation start/end which already has three exit paths (finish, fail, kill). |
| RC-015 | YES | CHECKED | Metrics registry flows through AppState. No cross-task type mismatches. |
| RC-016 | YES | WILL APPLY | Metrics struct fields must be complete for all consumer tasks. |
| RC-017 | YES | WILL APPLY | All self.metrics fields must be declared before use. |
| RC-018 | YES | WILL APPLY | Each task will have explicit test steps. |
| RC-019 | YES | CHECKED | Will follow existing route handler pattern from routes.rs for metrics endpoint. |
| RC-020 | NO | N/A | No Kameo messages. |
| RC-021 | YES | CHECKED | Verified: `ApiConfig` is in `config.rs:426`, `AppState` is in `server/state.rs:23`, metrics_stub is in `server/routes.rs:900`. |
| RC-032 | YES | CHECKED | Backup sizes come from `BackupManifest.compressed_size` (u64). Backup counts from `list_local()`/`list_remote()`. No shadow state needed. See data-authority.md. |

## Key Findings

1. **No actors in this project** -- chbackup uses standard async Rust with tokio::spawn, not Kameo actors. RC-001, RC-004, RC-010, RC-020 do not apply.

2. **Config field already exists** -- `api.enable_metrics: bool` already exists at config.rs:432, defaults to `true`. No config changes needed.

3. **prometheus crate not yet in Cargo.toml** -- Must be added. Roadmap specifies `prometheus = "0.13"`.

4. **Metrics endpoint is a stub** -- `routes::metrics_stub()` returns 501 at routes.rs:900. Must be replaced with real handler.

5. **Metrics data sources verified** -- BackupManifest provides `compressed_size`, DiffResult provides `carried`/`uploaded` counts, list module provides backup counts. See data-authority.md.
