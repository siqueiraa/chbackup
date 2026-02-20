# Diagnostics

**Plan:** 2026-02-20-01-go-parity-gaps

## Compilation Baseline

The project compiles without errors as of the current master branch (commit 93f55e28).

```
cargo check: PASS (0 errors, 0 warnings expected)
cargo test: PASS (unit tests)
```

## Key Crate Versions

- `clap` = derive API (v4.x)
- `serde` + `serde_yaml` for config
- `clickhouse` = v0.13 (HTTP protocol)
- `aws-sdk-s3` + `aws-config` + `aws-sdk-sts`
- `axum` for HTTP server
- `tokio` for async runtime
- `tracing` + `tracing-subscriber` for logging

## No MCP Server Available

This project does not have an MCP server configured. Symbol verification done via grep/glob tools on the source code directly.
