# Data Authority Analysis

Phase 3e is infrastructure-only (Docker, CI, K8s manifests). No new tracking fields, accumulators, or calculations are introduced.

## Data Requirements

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| CH version for CI matrix | Design doc section 1.4.6 | 23.8, 24.3, 24.8, 25.1 | USE EXISTING spec |
| Env var names for K8s | `src/config.rs:apply_env_overlay()` | S3_BUCKET, S3_ACCESS_KEY, etc. | USE EXISTING code |
| CLI flags for server mode | `src/cli.rs:Command::Server` | `--watch` | USE EXISTING code |
| Default API port | `config.example.yml` | `localhost:7171` | USE EXISTING config |
| ClickHouse uid/gid | Design doc section 1.2 | uid=101, gid=101 | USE EXISTING spec |
| Watch config params | `src/config.rs:WatchConfig` | watch_interval, full_interval | USE EXISTING -- but env overlay gap exists |

## Analysis Notes

- All data sources are either the design doc specification or existing source code.
- No custom data tracking or calculations needed.
- The only gap is `WATCH_INTERVAL`/`FULL_INTERVAL` env vars not being in the overlay (see knowledge_graph.json findings).
- This gap can be resolved in the plan by either adding env vars to the overlay or using `--env` CLI flags in K8s manifests.
