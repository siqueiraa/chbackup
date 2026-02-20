# Data Authority Analysis

## Data Requirements for Phase 3a

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Backup list (local) | `list::list_local()` | Returns `Vec<BackupSummary>` | USE EXISTING |
| Backup list (remote) | `list::list_remote()` | Returns `Vec<BackupSummary>` | USE EXISTING |
| Backup details | `BackupManifest` | All fields (name, size, tables, etc.) | USE EXISTING |
| Operation status tracking | None | N/A | MUST IMPLEMENT -- ActionLog ring buffer is new. No existing source tracks API operation history. The CLI has no concept of action tracking since each command is a separate process. |
| Operation duration | None | N/A | MUST IMPLEMENT -- Must record start/end timestamps per action. Part of ActionLog. |
| Operation errors | `anyhow::Error` | `.to_string()` | USE EXISTING (error messages from command functions) |
| Running operation state | None | N/A | MUST IMPLEMENT -- Need to track which operation is currently running for /status and /kill. CLI has no equivalent. |
| Cancellation support | None | N/A | MUST IMPLEMENT -- CancellationToken per operation. CLI uses Ctrl+C/SIGTERM. |
| ClickHouse version | `ChClient::get_version()` | Returns `String` | USE EXISTING |
| Binary version | `env!("CARGO_PKG_VERSION")` | Compile-time string | USE EXISTING |
| Resume state files | `resume.rs` + filesystem scan | `*.state.json` files | USE EXISTING -- scan local backup dirs for state files |
| Config values | `Config` struct | All fields | USE EXISTING |
| Lock status | `PidLock` | File-based locking | USE EXISTING (but need adaptation for server context) |

## Analysis Notes

- **ActionLog is genuinely new**: The CLI runs one command per process and exits. The server needs an in-memory ring buffer of past operations with timestamps, durations, status, and error messages. This maps to the `GET /api/v1/actions` endpoint and the `system.backup_actions` integration table.
- **CancellationToken is genuinely new**: CLI operations are cancelled via process signals. Server operations need programmatic cancellation via `POST /api/v1/kill`. This requires threading a `CancellationToken` through the operation execution.
- **Running operation state is genuinely new**: The server needs to know if an operation is currently running (for 409 Conflict when `allow_parallel=false`, and for `/api/v1/status`).
- All backup data (manifests, summaries, S3 keys) flows through existing functions. No shadow/duplicate tracking needed.
- The integration table schemas (system.backup_list, system.backup_actions) are defined by the design doc and must match the Go tool's column layout for compatibility.

## Each "MUST IMPLEMENT" Justification

1. **ActionLog**: CLI has no persistent operation history. Server must expose action history via `/api/v1/actions` and the `system.backup_actions` URL table. In-memory ring buffer is the simplest correct approach (matches Go tool behavior).

2. **Operation Duration Tracking**: CLI logs start/end but doesn't aggregate. Server must calculate and expose duration per action in the actions endpoint response.

3. **Running Operation State**: CLI uses process-level isolation (one command = one process). Server handles multiple requests on a single process and must serialize or parallelize operations based on config.

4. **CancellationToken**: CLI relies on OS signals (SIGINT/SIGTERM). Server needs programmatic cancellation via HTTP endpoint. `tokio_util::sync::CancellationToken` is the standard approach.
