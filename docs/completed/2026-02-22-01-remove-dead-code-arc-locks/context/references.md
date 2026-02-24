# Symbol and Reference Analysis (Phase 1/1.5 -- LSP Verified)

## Analysis Method

All findings verified via:
- LSP `findReferences` on exact symbol positions (line:character)
- Grep text search across entire `src/` tree (cross-validation)
- `cargo check` and `cargo clippy` (zero warnings/errors baseline)
- `RUSTFLAGS="-F dead_code" cargo check` confirming exactly 2 `#[allow(dead_code)]` annotations

---

## Confirmed Dead Code Items (8 items)

### 1. ChClient.debug field

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/clickhouse/client.rs:24`
**Type:** `bool`
**Annotation:** `#[allow(dead_code)]`

**LSP findReferences (line 24, char 5): 2 results, both in client.rs:**
- Line 24: field declaration
- Line 222: assignment in constructor (`debug: config.debug`)

**Grep `self.debug`: 0 matches** in entire src/ tree.

**Context:** Stored "for future use" per comment. Debug behavior already fully handled by `log_sql_queries` (line 219: `log_sql_queries: config.log_sql_queries || config.debug`). The `config.debug` is also used at line 211 for a conditional info log during construction. After construction, the stored field is never read.

**Removal impact:** Remove field declaration + `#[allow(dead_code)]` + constructor assignment. No callers affected.

---

### 2. ChClient::inner() method

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/clickhouse/client.rs:252`
**Signature:** `pub fn inner(&self) -> &clickhouse::Client`

**LSP findReferences (line 252, char 12): 0 results**
**Grep `\.inner()` in src/: 0 matches**

**Context:** The `inner` *field* is heavily used internally (10+ refs in client.rs for `self.inner.query()`). Only the `pub fn inner()` *getter* is dead.

**Removal impact:** Remove method only. The `inner` field is NOT dead.

---

### 3. S3Client::inner() method

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/storage/s3.rs:220`
**Signature:** `pub fn inner(&self) -> &aws_sdk_s3::Client`

**LSP findReferences (line 220, char 12): 0 results**
**Grep `\.inner()` in src/: 0 matches**

**Context:** Same as ChClient::inner(). The `inner` field is used in 7+ places within s3.rs for actual S3 operations.

**Removal impact:** Remove method only. The `inner` field is NOT dead.

---

### 4. S3Client::concurrency() method

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/storage/s3.rs:235`
**Signature:** `pub fn concurrency(&self) -> u32`

**LSP findReferences (line 235, char 12): 0 results**
**Grep `\.concurrency()` in src/: 0 matches**

**Context:** Added in `3beb3d43` ("feat(storage): add concurrency and object_disk_path fields to S3Client"). CLAUDE.md says "controls within-file multipart parallelism" but no code calls this getter.

**Removal impact:** Safe to remove method. Also enables field removal (item 6).

---

### 5. S3Client::object_disk_path() method

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/storage/s3.rs:243`
**Signature:** `pub fn object_disk_path(&self) -> &str`

**LSP findReferences (line 243, char 12): 0 results**
**Grep `\.object_disk_path()` in src/: 0 matches**

**Context:** Added in same commit as `concurrency()`. CLAUDE.md says "provides alternate key prefix for S3 disk objects" but no code reads it.

**Removal impact:** Safe to remove method. Also enables field removal (item 7).

---

### 6. S3Client.concurrency field

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/storage/s3.rs:58`
**Type:** `u32`

**LSP findReferences (line 58, char 5): 4 results, ALL in s3.rs:**
- Line 58: field declaration
- Line 189: assignment in `S3Client::new()` constructor (`concurrency: config.concurrency`)
- Line 238: read in dead `concurrency()` getter (`self.concurrency`)
- Line 1568: assignment in test helper `mock_s3_client()` (`concurrency: 1`)

**External reads:** 0 (only the dead getter reads it)

**Removal impact:** Remove field + dead getter (item 4) + 2 constructor assignments (new() line 189, test helper line 1568). Also remove `concurrency` from `S3Config` if desired (out of scope -- config field stays for forward compat).

---

### 7. S3Client.object_disk_path field

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/storage/s3.rs:60`
**Type:** `String`

**LSP findReferences (line 60, char 5): 4 results, ALL in s3.rs:**
- Line 60: field declaration
- Line 190: assignment in `S3Client::new()` constructor
- Line 246: read in dead `object_disk_path()` getter
- Line 1569: assignment in test helper

**External reads:** 0 (only the dead getter reads it)

**Removal impact:** Remove field + dead getter (item 5) + 2 constructor assignments.

---

### 8. attach_parts() function

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/restore/attach.rs:487`
**Signature:** `pub async fn attach_parts(params: &AttachParams<'_>) -> Result<u64>`
**Annotation:** `#[allow(dead_code)]`

**LSP findReferences (line 487, char 18): 1 result (self-reference at definition only)**
**Grep `attach_parts[^_]` in src/: 1 match (definition at line 487)**

**Context:** Borrowed-reference version superseded by `attach_parts_owned()` (line 421). All production callers use `attach_parts_owned()` (import at `restore/mod.rs:50`).

**Important safeguards:**
- `AttachParams` struct is NOT dead -- used internally by `attach_parts_owned()` at line 462 as bridge to `attach_parts_inner()`
- `attach_parts_inner()` is NOT dead -- called by both `attach_parts()` (dead) and `attach_parts_owned()` (alive)
- Only the `pub async fn attach_parts()` wrapper function is dead

**Removal impact:** Remove function + `#[allow(dead_code)]` annotation. `AttachParams` and `attach_parts_inner()` must be kept.

---

## NOT Dead -- Verified Still in Use

### Arc/Mutex/ArcSwap in AppState

All 11 concurrency primitives in `AppState` verified as architecturally required:

| Field | Wrapper | Verified Usage |
|-------|---------|----------------|
| `config` | `Arc<ArcSwap<Config>>` | `.load()` in handlers + `.store()` in reload/restart |
| `ch` | `Arc<ArcSwap<ChClient>>` | `.load()` in handlers + `.store()` in reload/restart |
| `s3` | `Arc<ArcSwap<S3Client>>` | `.load()` in handlers + `.store()` in reload/restart |
| `action_log` | `Arc<Mutex<ActionLog>>` | Mutated by try_start_op, finish_op, fail_op, kill_op, routes |
| `running_ops` | `Arc<Mutex<HashMap<...>>>` | Mutated by multiple async tasks |
| `op_semaphore` | `Arc<Semaphore>` | Shared across handlers for concurrency control |
| `metrics` | `Option<Arc<Metrics>>` | Read-only after creation, shared across handlers |
| `watch_shutdown_tx` | `Arc<Mutex<Option<...>>>` | Written by spawn_watch, read by route handlers |
| `watch_reload_tx` | `Arc<Mutex<Option<...>>>` | Same pattern as shutdown_tx |
| `watch_status` | `Arc<Mutex<WatchStatus>>` | Written by watch loop, read by API handlers |
| `manifest_cache` | `Arc<Mutex<ManifestCache>>` | Written by list_remote_cached, invalidated by mutating ops |

### Test-Only Helpers (Keep As-Is)

| Item | Location | Callers | Decision |
|------|----------|---------|----------|
| `ProgressTracker::disabled()` | progress.rs:48 | 2 test-only refs | Keep -- intentional test helper |
| `ProgressTracker::is_active()` | progress.rs:67 | 4 test-only refs | Keep -- intentional test helper |
| `ActionLog::running()` | actions.rs:118 | 7 test-only refs | Keep -- intentional test helper |

### Error Variants (Keep All)

ChBackupError enum provides domain error taxonomy. Removing unused variants weakens the error model. The `exit_code()` match arms for `BackupError`/`ManifestError` with "not found" pattern are deliberately designed even though no production code currently constructs them.

### Prometheus Counters (Recommended: Keep or Remove -- Plan Decision)

`parts_uploaded_total` and `parts_skipped_incremental_total` are registered but never incremented. Two options:
1. Keep as placeholders for future instrumentation (comment them as "not yet wired")
2. Remove to reduce dead surface area (can be re-added when wired)

### ListParams::format Field (Keep)

Deserialized from query params for DDL compatibility. Documented as intentional. Not dead in a functional sense.

---

## Summary Table

| # | Item | File | Line | Dead? | Action |
|---|------|------|------|-------|--------|
| 1 | ChClient.debug | clickhouse/client.rs | 24 | YES | Remove field + allow + constructor assign |
| 2 | ChClient::inner() | clickhouse/client.rs | 252 | YES | Remove method |
| 3 | S3Client::inner() | storage/s3.rs | 220 | YES | Remove method |
| 4 | S3Client::concurrency() | storage/s3.rs | 235 | YES | Remove method |
| 5 | S3Client::object_disk_path() | storage/s3.rs | 243 | YES | Remove method |
| 6 | S3Client.concurrency | storage/s3.rs | 58 | YES | Remove field + constructor assigns (2 sites) |
| 7 | S3Client.object_disk_path | storage/s3.rs | 60 | YES | Remove field + constructor assigns (2 sites) |
| 8 | attach_parts() | restore/attach.rs | 487 | YES | Remove function + allow annotation |
| - | AttachParams struct | restore/attach.rs | 33 | NO | Keep (used by attach_parts_owned) |
| - | attach_parts_inner() | restore/attach.rs | 500 | NO | Keep (used by attach_parts_owned) |
| - | All Arc/Mutex/ArcSwap | server/state.rs | various | NO | Keep (required for concurrency) |
| - | ChBackupError variants | error.rs | various | PARTIAL | Keep (error taxonomy) |
| - | Prometheus counters | server/metrics.rs | 32,35 | UNUSED | Plan decision (keep or remove) |
| - | ListParams::format | server/routes.rs | 74 | INTENTIONAL | Keep (DDL compat) |
