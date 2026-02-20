# Plan: Go Parity Gaps - Phase 7 (Revised)

## Goal

Fix genuine gaps and revert incorrect Phase 6 "Go parity" changes that overrode intentional design doc decisions. This plan distinguishes between:

- **Reverts**: Phase 6 changes that copied Go defaults over design doc decisions
- **Genuine fixes**: Bugs, missing features, and design doc contradictions
- **Design doc updates**: Where Phase 6 improvements should be documented rather than reverted

## Architecture Overview

This plan modifies existing code across 4 modules and updates the design doc. No new architectural patterns are introduced. All changes follow established patterns.

## Architecture Assumptions (VALIDATED)

### What This Plan Does

1. **Reverts config defaults** that Phase 6 changed away from design doc values
2. **Fixes ch_port** to resolve a design doc internal contradiction (§12 vs roadmap)
3. **Expands env var overlay** to fulfill design doc §2 requirement
4. **Adds PutObject retry** as a genuine missing feature
5. **Updates design doc** for Phase 6 changes that were genuine improvements
6. **Updates CLAUDE.md** documentation

### What This Plan CANNOT Do

- Cannot make manifests wire-compatible with Go (INTENTIONAL difference per user instruction)
- Cannot change ClickHouse protocol from HTTP to native TCP
- Cannot add Go-style `/backup/*` routes (not in design doc §9)
- Cannot change CLI flag types/additions beyond design doc §2

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| Config default reverts change behavior for Phase 6 users | YELLOW | These revert TO design doc values; anyone following the design doc already expects these |
| ch_port change from 9000 to 8123 | GREEN | Correct for HTTP protocol; roadmap already specifies 8123 |
| Env var overlay is mechanical | GREEN | Each var follows identical pattern |
| PutObject retry adds latency on failure | GREEN | Only retries transient errors; happy path unchanged |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `cargo test` passing | yes | All unit tests pass |
| `cargo check` clean | yes | Zero compilation errors |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| STS credential auto-refresh (1h expiry) | Requires credential provider refactor | Phase 8 |
| IRSA/Web Identity Token for EKS | Requires different AWS credential chain | Phase 8 |
| SSE-C (customer-provided keys) | 3 new config fields + plumbing | Phase 8 |
| POST /api/v1/actions is a stub | Requires full command dispatch implementation | Phase 8 |
| Prometheus metric naming drift | Breaking change for monitoring | Phase 8 |
| Missing Prometheus metrics (s3_copy_object_total, failed_backups_total) | Requires new instrumentation | Phase 8 |
| Manifest wire compatibility | INTENTIONAL difference | Never |

## Dependency Groups

```
Group A (Sequential - Config Fixes):
  - Task 1: Revert Phase 6 config defaults to design doc values
  - Task 2: Fix ch_port default (resolve design doc contradiction)

Group B (Independent - Env Var Overlay):
  - Task 3: Expand env var overlay coverage

Group C (Independent - S3 Retry):
  - Task 4: Add PutObject/UploadPart retry wrapper

Group D (Sequential - Documentation):
  - Task 5: Update design doc for genuine Phase 6 improvements
  - Task 6: Update CLAUDE.md for all modified modules
```

## Tasks

### Task 1: Revert Phase 6 config defaults to design doc values

**Description:** Phase 6 changed several config defaults to match Go clickhouse-backup, overriding intentional design doc decisions. Revert these to the values specified in `docs/design.md` §12.

**Changes:**

| Config Field | Current (Phase 6) | Revert To (Design §12) | Why |
|-------------|-------------------|----------------------|-----|
| `clickhouse.timeout` | `"30m"` | `"5m"` | §11.7: "applies to individual queries" — 5m is generous for any single query |
| `clickhouse.max_connections` | `NumCPU/2` | `1` | §12: conservative sequential default, avoids resource contention on restore |
| `clickhouse.default_replica_path` | `{cluster}/{shard}/{database}/{table}` | `{shard}/{database}/{table}` | §12: extra `{cluster}` segment changes ZK path structure |
| `s3.acl` | `"private"` | `""` | §12: empty = don't send ACL header, safest for non-AWS S3 stores |
| `clickhouse.check_parts_columns` | `true` | `false` | §12: explicit design choice; opt-in for pre-flight check |

**TDD Steps:**
1. Write failing test: `test_config_defaults_match_design_doc` — verify each default function returns the design doc value
2. Implement: Change each `default_*()` function to match design doc §12
3. Verify test passes
4. Update existing tests that rely on Phase 6 default values

**Files:** `src/config.rs`
**Acceptance:** F001

---

### Task 2: Fix ch_port default (resolve design doc contradiction)

**Description:** Design doc §12 says `port: 9000` but the roadmap Phase 0 says "Default port 8123." The `clickhouse` crate uses HTTP protocol (port 8123), not native TCP (port 9000). Port 9000 was copied from Go which uses the native protocol. Fix the default to match the actual protocol.

**Changes:**
1. `default_ch_port()`: `9000` -> `8123`

**TDD Steps:**
1. Write failing test: `test_ch_port_default_http` — verify `default_ch_port()` returns 8123
2. Implement: Change `default_ch_port()` return value
3. Verify test passes

**Files:** `src/config.rs`
**Acceptance:** F002

---

### Task 3: Expand env var overlay coverage

**Description:** Design doc §2 states: "Every config parameter can be overridden via an environment variable." Rust currently supports only 18 env vars. Add the missing env vars to `apply_env_overlay()` following the existing pattern.

**Priority env vars to add (grouped by section):**

**General (6 new):**
- `CHBACKUP_BACKUPS_TO_KEEP_LOCAL` -> `general.backups_to_keep_local`
- `CHBACKUP_BACKUPS_TO_KEEP_REMOTE` -> `general.backups_to_keep_remote`
- `CHBACKUP_UPLOAD_CONCURRENCY` -> `general.upload_concurrency`
- `CHBACKUP_DOWNLOAD_CONCURRENCY` -> `general.download_concurrency`
- `CHBACKUP_RETRIES_ON_FAILURE` -> `general.retries_on_failure`
- `CHBACKUP_RETRIES_PAUSE` -> `general.retries_pause`

**ClickHouse (10 new):**
- `CLICKHOUSE_SECURE` -> `clickhouse.secure`
- `CLICKHOUSE_SKIP_VERIFY` -> `clickhouse.skip_verify`
- `CLICKHOUSE_TLS_KEY` -> `clickhouse.tls_key`
- `CLICKHOUSE_TLS_CERT` -> `clickhouse.tls_cert`
- `CLICKHOUSE_TLS_CA` -> `clickhouse.tls_ca`
- `CLICKHOUSE_SYNC_REPLICATED_TABLES` -> `clickhouse.sync_replicated_tables`
- `CLICKHOUSE_MAX_CONNECTIONS` -> `clickhouse.max_connections`
- `CLICKHOUSE_TIMEOUT` -> `clickhouse.timeout`
- `CLICKHOUSE_CONFIG_DIR` -> `clickhouse.config_dir`
- `CLICKHOUSE_DEBUG` -> `clickhouse.debug`

**S3 (8 new):**
- `S3_ACL` -> `s3.acl`
- `S3_STORAGE_CLASS` -> `s3.storage_class`
- `S3_SSE` -> `s3.sse`
- `S3_SSE_KMS_KEY_ID` -> `s3.sse_kms_key_id`
- `S3_DISABLE_SSL` -> `s3.disable_ssl`
- `S3_DISABLE_CERT_VERIFICATION` -> `s3.disable_cert_verification`
- `S3_CONCURRENCY` -> `s3.concurrency`
- `S3_OBJECT_DISK_PATH` -> `s3.object_disk_path`

**Backup (6 new):**
- `CHBACKUP_BACKUP_COMPRESSION` -> `backup.compression`
- `CHBACKUP_BACKUP_UPLOAD_CONCURRENCY` -> `backup.upload_concurrency`
- `CHBACKUP_BACKUP_DOWNLOAD_CONCURRENCY` -> `backup.download_concurrency`
- `CHBACKUP_BACKUP_RETRIES_ON_FAILURE` -> `backup.retries_on_failure`
- `CHBACKUP_BACKUP_RETRIES_DURATION` -> `backup.retries_duration`
- `CHBACKUP_BACKUP_TABLES` -> `backup.tables`

**API (4 new):**
- `API_SECURE` -> `api.secure`
- `API_USERNAME` -> `api.username`
- `API_PASSWORD` -> `api.password`
- `API_CREATE_INTEGRATION_TABLES` -> `api.create_integration_tables`

**Watch (2 new):**
- `WATCH_ENABLED` -> `watch.enabled`
- `WATCH_MAX_CONSECUTIVE_ERRORS` -> `watch.max_consecutive_errors`

**TDD Steps:**
1. Write failing test: `test_env_overlay_coverage` — set each new env var, load config, verify field populated
2. Implement: Add env var reads to `apply_env_overlay()` following existing pattern
3. Verify test passes

**Files:** `src/config.rs`
**Acceptance:** F003

---

### Task 4: Add PutObject/UploadPart retry wrapper

**Description:** S3 PutObject and UploadPart operations have no retry logic. Transient S3 errors (network timeout, 500, 503) fail the entire upload. Add retry wrapper following the existing `copy_object_with_retry_jitter()` pattern.

**Changes:**
1. Add `put_object_with_retry()` method to `S3Client` following `copy_object_with_retry_jitter()` pattern
2. Add `upload_part_with_retry()` method to `S3Client`
3. Wire retry into upload pipeline where `put_object()` and `upload_part()` are called
4. Use `effective_retries()` for retry count and `apply_jitter()` for delay

**TDD Steps:**
1. Write test: `test_put_object_retry_config` — verify retry parameters are plumbed correctly
2. Implement: Add retry wrapper methods to `S3Client`
3. Wire into upload pipeline
4. Verify test passes

**Files:** `src/storage/s3.rs`, `src/upload/mod.rs`
**Acceptance:** F004

---

### Task 5: Update design doc for genuine Phase 6 improvements

**Description:** Some Phase 6 changes are genuine improvements that should be documented in the design doc rather than reverted. Update `docs/design.md` to reflect these.

**Changes to `docs/design.md`:**

1. **§12 skip_tables**: Add `_temporary_and_external_tables.*` as 4th entry (genuine safety improvement — these are ClickHouse internal temporary tables that should never be backed up)

2. **§9 API concurrent rejection**: Change "409 Conflict" to "423 Locked" (HTTP 423 is semantically more accurate for "another operation is running"; 409 implies the request itself conflicts with the resource state)

3. **§3.4 Backup failure cleanup**: Add paragraph documenting that on `backup::create()` failure, the partial backup directory is removed and `clean_shadow()` runs for the backup name

4. **§8.2 Incremental chain protection**: Add paragraph documenting that `retention_remote()` protects backups referenced as incremental bases by surviving backups (backup-level protection on top of key-level GC)

5. **§12 ch_port**: Change `port: 9000` to `port: 8123` to match roadmap and HTTP protocol (resolves internal contradiction)

6. **§7 or §3.6 Multipart CopyObject**: Add note that CopyObject for objects >5GB uses multipart UploadPartCopy API (S3 hard limit)

**TDD Steps:**
1. Read each section being modified
2. Make targeted edits preserving existing structure
3. Verify no unrelated sections were changed

**Files:** `docs/design.md`
**Acceptance:** F005

---

### Task 6: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Specific fixes needed:**
- Root CLAUDE.md: Update Phase 7 status, fix any stale Phase 6 claims
- `src/server/CLAUDE.md`: Fix "409 Conflict" reference to "423 Locked"
- `src/storage/CLAUDE.md`: Document PutObject/UploadPart retry methods

**TDD Steps:**
1. Read affected CLAUDE.md files
2. Update to reflect actual implementation state
3. Validate all CLAUDE.md files have required sections (Parent Context, Directory Structure, Key Patterns, Parent Rules)

**Files:** CLAUDE.md (root), src/server/CLAUDE.md, src/storage/CLAUDE.md
**Acceptance:** FDOC

---

## Notes

### Phase 6 Audit Summary

Phase 6 "Go parity" made changes in two categories:

**Genuine improvements (keeping, updating design doc):**
- `skip_tables` adds `_temporary_and_external_tables.*` — safety improvement
- API 423 instead of 409 — semantically better
- Multipart CopyObject >5GB — fixes real S3 limit
- Backup failure cleanup — prevents broken backup accumulation
- Incremental chain protection — stronger than key-level GC alone
- STS AssumeRole — implements documented but unimplemented config field
- Freeze-by-part with error 218 handling — implements documented config field
- List `--format` flag — additive, default behavior unchanged
- List `desc` query parameter — additive

**Go copies being reverted (conflicted with design doc):**
- `clickhouse.timeout` 5m→30m — weakened safety bound
- `clickhouse.max_connections` 1→NumCPU/2 — changed conservative default
- `clickhouse.default_replica_path` added {cluster} — changed ZK path structure
- `s3.acl` ""→"private" — may break non-AWS S3 stores
- `check_parts_columns` false→true — overrode design doc's opt-in choice

### Consistency Validation

| Check | Status | Notes |
|-------|--------|-------|
| All tasks reference design doc sections | PASS | §12, §2, §9, §3.4, §3.6, §7, §8.2, §11.7 |
| No task copies Go behavior over design doc | PASS | All changes align with or improve design doc |
| Dropped tasks documented | PASS | See audit notes above |
