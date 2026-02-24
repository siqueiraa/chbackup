# Pattern Discovery

No global `docs/patterns/` directory exists. Patterns discovered locally from affected files.

## Pattern 1: Request Type for Optional JSON Body (routes.rs)

All API request types follow the same pattern:
```rust
#[derive(Debug, Deserialize, Default)]
pub struct XxxRequest {
    pub field: Option<Type>,
    ...
}
```

Handler signature for optional body:
```rust
pub async fn handler(
    State(state): State<AppState>,
    body: Option<Json<XxxRequest>>,
) -> Result<Json<Response>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|b| b.0).unwrap_or_default();
    // use req.field.unwrap_or(default)
}
```

Reference implementations:
- `CreateRequest` at routes.rs:743 (7 optional fields)
- `UploadRequest` at routes.rs:807 (2 optional fields)
- `DownloadRequest` at routes.rs:850

## Pattern 2: CLI Flag Override of Config (main.rs)

The Watch command pattern for CLI-to-config overrides:
```rust
Command::Watch { watch_interval, full_interval, ... } => {
    let mut config = config;
    if let Some(v) = watch_interval {
        config.watch.watch_interval = v;
    }
    if let Some(v) = full_interval {
        config.watch.full_interval = v;
    }
    // ... then use config normally
}
```
Reference: main.rs:540-559

## Pattern 3: Pure DDL Rewriting Functions (remap.rs)

All remap functions are pure (no async, no I/O):
```rust
fn rewrite_xxx(ddl: &str, ...) -> String {
    // Parse, transform, return new string
    // On any parse failure: return ddl.to_string() unchanged
}
```
Reference: `rewrite_distributed_engine` at remap.rs:599-605

## Pattern 4: Watch Resume State Tests (watch/mod.rs)

Tests use `BackupSummary` with manually constructed timestamps:
```rust
fn make_summary(name: &str, ts: DateTime<Utc>) -> BackupSummary {
    BackupSummary { name: name.to_string(), timestamp: Some(ts), is_broken: false, ... }
}
```
All test functions named `test_resume_*` at lines 764-993.

## Pattern 5: Config Validation (config.rs)

Validation in `Config::validate()` uses `parse_duration_secs()` with `.context()` for error messages and `anyhow::anyhow!()` for constraint violations:
```rust
let secs = parse_duration_secs(&self.xxx)
    .context("Invalid xxx duration")?;
if constraint_violated {
    return Err(anyhow::anyhow!("message with {} values", self.xxx));
}
```
Reference: config.rs:1398-1412
