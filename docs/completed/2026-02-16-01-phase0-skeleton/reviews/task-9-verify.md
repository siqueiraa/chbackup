# Task 9 Verification: config.example.yml and final wiring

## Commit: b70c455

## Checklist

- [x] `config.example.yml` exists with all ~106 params documented across 7 sections
- [x] All commands route through config -> logging -> lock flow
- [x] `default-config` and `print-config` are special-cased (no logging/lock needed)
- [x] `list` command connects to ClickHouse and S3, prints connection status
- [x] All other stub commands log "not implemented yet" via `tracing::info!`
- [x] Lock scope determined by `lock_for_command()` per design doc section 2
- [x] Zero compiler warnings (`cargo check --all-targets --all-features`)
- [x] Zero clippy warnings (`cargo clippy --all-targets -- -D warnings`)
- [x] All 14 unit tests pass (9 lib + 5 integration)
- [x] `cargo run -- default-config` prints valid YAML
- [x] `cargo run -- create --help` shows all flags
- [x] `cargo test --no-run` compiles test targets

## Architecture Notes

- `main.rs` now imports from the library crate (`chbackup::`) instead of
  re-declaring `mod clickhouse`, `mod storage`, etc. This eliminates dead_code
  warnings that would arise from the binary having its own module tree separate
  from the library's.
- `cli.rs` remains a binary-only module (`mod cli;` in `main.rs`) since it
  defines the CLI parser which is not needed by the library.
- Helper functions `command_name()` and `backup_name_from_command()` extract
  metadata from the clap `Command` enum for lock scope determination.

## QA Result

```json
{
  "status": "PASS",
  "clippy_warnings": 0,
  "test_results": "14 passed, 0 failed",
  "commit_hash": "b70c455",
  "issues": []
}
```
