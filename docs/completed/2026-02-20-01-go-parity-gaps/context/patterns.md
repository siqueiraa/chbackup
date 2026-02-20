# Patterns

**Plan:** 2026-02-20-01-go-parity-gaps

## Pattern References

This plan modifies existing code across many modules. No new architectural patterns are introduced. All changes follow existing patterns documented in the per-module CLAUDE.md files.

### Existing Patterns Used

1. **Config defaults** (`src/config.rs`): `default_*()` functions for serde default values. New defaults follow exact same pattern.
2. **CLI flags** (`src/cli.rs`): clap derive API with `#[arg(...)]` attributes. New flags follow existing flag patterns.
3. **Env var overlay** (`src/config.rs`): `apply_env_overlay()` function with `if let Ok(v) = std::env::var(...)` pattern.
4. **API route registration** (`src/server/routes.rs`): axum Router with `.route()` calls. Compatibility routes follow same pattern.
5. **S3 operations** (`src/storage/s3.rs`): Retry with exponential backoff pattern from `copy_object_with_retry_jitter()`.
6. **ClickHouse queries** (`src/clickhouse/client.rs`): `ChClient` methods with `.query()` and `.fetch_all()`.

### No New Patterns

N/A - modifying existing code only.
