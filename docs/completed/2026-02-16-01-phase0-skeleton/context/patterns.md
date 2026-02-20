# Patterns

**Date:** 2026-02-16T22:30:00Z

## Status

Greenfield project — no existing patterns to discover.

## Patterns to Establish

| Pattern | Description | Task |
|---------|-------------|------|
| Error handling | `thiserror` enum + `anyhow` at binary boundary | T1 |
| CLI structure | `clap` derive API with subcommands | T2 |
| Config loading | YAML + env overlay + CLI override | T3 |
| Logging init | `tracing-subscriber` with format selection | T5 |
| Resource locking | PID file with liveness check | T6 |
| Client wrapper | Thin wrapper around external crate client | T7, T8 |
