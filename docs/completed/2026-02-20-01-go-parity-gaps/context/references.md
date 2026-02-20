# References

**Plan:** 2026-02-20-01-go-parity-gaps

## Key Symbol References

### Config Defaults
- `default_backups_to_keep_remote_general()` - referenced in `src/config.rs`, returns `7` (should be `0`)
- `default_retries_jitter_30()` - referenced in `src/config.rs`, returns `30` (Go default is `0`)
- `default_concurrency_4()` - referenced in `src/config.rs` for upload/download concurrency

### CLI Command Dispatch
- `Command::Upload` - matched in `src/main.rs` around line 200+
- `Command::Download` - matched in `src/main.rs` around line 215+
- `Command::Restore` - matched in `src/main.rs` around line 240+
- `Command::CreateRemote` - matched in `src/main.rs`
- `Command::RestoreRemote` - matched in `src/main.rs`

### API Routes
- Route registration in `src/server/mod.rs` via axum Router
- Actions dispatch stub in `src/server/routes.rs` (post_actions function)
- Query param authentication gap in `src/server/auth.rs`

### S3 Client
- `S3Client::new()` in `src/storage/s3.rs` - STS assume_role one-shot call
- `S3Client::put_object()` in `src/storage/s3.rs` - no retry wrapper
- `S3Client::copy_object_with_retry_jitter()` in `src/storage/s3.rs` - existing retry pattern to follow

### ClickHouse Client
- `ChClient::new()` in `src/clickhouse/client.rs` - no timeout configuration
- `query_database_engine()` in `src/clickhouse/client.rs`

### Restore Flow
- Named collections restore at Phase 4b in `src/restore/mod.rs` - ordering bug
- `create_ddl_objects()` in `src/restore/schema.rs`

### Watch Mode
- `run_watch_loop()` in `src/watch/mod.rs` - type string "incr" vs Go's "increment"
- `resume_state()` in `src/watch/mod.rs`
