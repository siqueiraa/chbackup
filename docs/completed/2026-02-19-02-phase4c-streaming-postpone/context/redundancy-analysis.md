# Redundancy Analysis

## Proposed New Public Components

| Proposed | Description |
|----------|-------------|
| `is_streaming_engine(engine: &str) -> bool` | Detect Kafka/NATS/RabbitMQ/S3Queue engines |
| `is_refreshable_mv(tm: &TableManifest) -> bool` | Detect MVs with REFRESH clause in DDL |

## Search Results

### `is_streaming_engine`

- **Workspace search for "streaming"**: Found `allow_object_disk_streaming` (S3 CopyObject fallback -- unrelated), `streaming copy` (S3 -- unrelated). No existing function that detects streaming table engines.
- **Workspace search for "Kafka"**: Found only in `config.rs:216` (comment), `table_filter.rs:188-190` (test for `is_engine_excluded`). No Kafka engine detection function.
- **`is_engine_excluded`** in `table_filter.rs:78`: Checks if engine is in a user-provided skip list. This is for BACKUP exclusion, not for RESTORE postponement classification. Different purpose: skip_table_engines is a user config, streaming detection is a hardcoded set.

| Proposed | Existing Match | Decision | Justification |
|----------|---------------|----------|---------------|
| `is_streaming_engine()` | `is_engine_excluded()` in table_filter.rs | COEXIST | Different semantics: `is_engine_excluded` is config-driven (user decides what to skip), `is_streaming_engine` is hardcoded (ClickHouse engine behavior). Streaming engines MUST be postponed regardless of user config. Cleanup: N/A -- genuinely different purposes. |

### `is_refreshable_mv`

- **Workspace search for "REFRESH"**: No results in `src/`. Not implemented anywhere.
- **Workspace search for "refreshable"**: No results in `src/`.

| Proposed | Existing Match | Decision | Justification |
|----------|---------------|----------|---------------|
| `is_refreshable_mv()` | (none) | N/A | No existing equivalent. New function required. |

## Summary

- 2 new functions proposed
- 0 REPLACE decisions (no removal needed)
- 1 COEXIST decision (with `is_engine_excluded` -- genuinely different purposes)
- Both functions are small utility functions in `src/restore/topo.rs`
