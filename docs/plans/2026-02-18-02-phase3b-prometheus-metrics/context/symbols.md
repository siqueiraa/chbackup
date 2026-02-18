# Type Verification Table

## Types Used in Plan

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `config.api.enable_metrics` | `bool` | `bool` | config.rs:432 - `pub enable_metrics: bool` |
| `config.api.listen` | `String` | `String` | config.rs:429 - `pub listen: String` |
| `AppState` | struct, Clone | struct, Clone | server/state.rs:23 - `#[derive(Clone)] pub struct AppState` |
| `AppState.config` | `Arc<Config>` | `Arc<Config>` | server/state.rs:24 |
| `AppState.ch` | `ChClient` | `ChClient` | server/state.rs:25 |
| `AppState.s3` | `S3Client` | `S3Client` | server/state.rs:26 |
| `AppState.action_log` | `Arc<Mutex<ActionLog>>` | `Arc<Mutex<ActionLog>>` | server/state.rs:27 |
| `AppState.current_op` | `Arc<Mutex<Option<RunningOp>>>` | `Arc<Mutex<Option<RunningOp>>>` | server/state.rs:28 |
| `AppState.op_semaphore` | `Arc<Semaphore>` | `Arc<Semaphore>` | server/state.rs:29 |
| `BackupManifest.compressed_size` | `u64` | `u64` | manifest.rs:44 |
| `BackupSummary.size` | `u64` | `u64` | list.rs:31 |
| `BackupSummary.compressed_size` | `u64` | `u64` | list.rs:33 |
| `DiffResult.carried` | `usize` | `usize` | backup/diff.rs:16 |
| `DiffResult.uploaded` | `usize` | `usize` | backup/diff.rs:18 |
| `PartInfo.source` | `String` | `String` | manifest.rs:136 |
| `list_local()` return | `Result<Vec<BackupSummary>>` | `Result<Vec<BackupSummary>>` | list.rs:81 |
| `list_remote()` return | `Result<Vec<BackupSummary>>` | `Result<Vec<BackupSummary>>` | list.rs:125 |
| `backup::create()` return | `Result<BackupManifest>` | `Result<BackupManifest>` | backup/mod.rs:64-73 |
| `upload::upload()` return | `Result<()>` | `Result<()>` | upload/mod.rs:165-173 |
| `download::download()` return | `Result<PathBuf>` | `Result<PathBuf>` | download/mod.rs:136-141 |
| `restore::restore()` return | `Result<()>` | `Result<()>` | restore/mod.rs:57-65 |
| `ActionStatus::Running` | enum variant | enum variant | server/actions.rs:16 |
| `ActionStatus::Completed` | enum variant | enum variant | server/actions.rs:17 |
| `ActionStatus::Failed(String)` | enum variant | enum variant | server/actions.rs:18 |
| `ActionStatus::Killed` | enum variant | enum variant | server/actions.rs:19 |
| `metrics_stub()` | async fn -> (StatusCode, &str) | async fn -> (StatusCode, &str) | routes.rs:900-902 |

## prometheus crate API (v0.13)

| Type | Import Path | Verified |
|---|---|---|
| `Registry` | `prometheus::Registry` | crate docs |
| `TextEncoder` | `prometheus::TextEncoder` | crate docs |
| `Encoder` (trait) | `prometheus::Encoder` | crate docs |
| `IntCounter` | `prometheus::IntCounter` | crate docs |
| `IntCounterVec` | `prometheus::IntCounterVec` | crate docs |
| `IntGauge` | `prometheus::IntGauge` | crate docs |
| `Gauge` | `prometheus::Gauge` | crate docs |
| `Histogram` | `prometheus::Histogram` | crate docs |
| `HistogramVec` | `prometheus::HistogramVec` | crate docs |
| `HistogramOpts` | `prometheus::HistogramOpts` | crate docs |
| `opts!` | `prometheus::opts!` | crate docs |

## Anti-Patterns Verified

- NO `.as_str()` on enum types
- NO implicit String -> Enum conversions
- NO tuple field order assumptions
- Config types match implementation types (all verified via grep)
