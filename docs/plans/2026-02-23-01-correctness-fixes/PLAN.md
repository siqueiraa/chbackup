# Plan: Fix 7 Correctness Issues from Security/Quality Audit

## Goal

Fix 7 correctness issues (P1-P3) found during a security/quality audit of chbackup: path traversal sanitization, broken TLS config, hermetic unit tests, dead config wiring, strict-fail for column checks, env-style CLI overrides, and DRY path encoding consolidation.

## Architecture Overview

This plan modifies 7 modules across the codebase:
- **src/path_encoding.rs** (NEW): Canonical path component encoder with sanitization (Issues 1+7)
- **src/storage/s3.rs**: Fix TLS disable_cert_verification and disable_ssl wiring (Issues 2+4), hermetic unit tests (Issue 3)
- **src/backup/mod.rs**: Strict-fail for check_parts_columns (Issue 5)
- **src/config.rs**: Env-style key support for --env flag (Issue 6)
- **src/download/mod.rs**, **src/upload/mod.rs**, **src/restore/attach.rs**, **src/restore/mod.rs**, **src/backup/collect.rs**: Replace duplicated url_encode with canonical module (Issues 1+7)

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **path_encoding.rs**: Created by this plan, used by backup/collect.rs, download/mod.rs, upload/mod.rs, restore/attach.rs, restore/mod.rs
- **S3Client::new()**: Owned by src/storage/s3.rs, called from 13 sites (main.rs, server/routes.rs, restore/mod.rs, watch/mod.rs)
- **check_parts_columns control flow**: Owned by src/backup/mod.rs:192-226, called only from create()
- **apply_cli_env_overrides**: Owned by src/config.rs:1100, called from Config::load()
- **mock_s3_client**: Owned by src/storage/s3.rs:1520, used by 8 tests (5 sync, 3 async)

### What This Plan CANNOT Do
- Cannot add "skip TLS verification" via the AWS SDK's public TlsContext API (no such API exists in aws-smithy-http-client v1.1.10)
- Cannot make the 3 async retry tests in s3.rs fully offline (they test real S3 error paths)
- Cannot change ClickHouse behavior when check_parts_columns finds issues
- Cannot modify the `set_field()` match arms (those are correct; only the caller needs translation)

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| Path encoding replacement breaks existing backup/restore paths | YELLOW | Canonical encoder produces identical output for all non-adversarial inputs; test with same inputs as existing tests |
| disable_cert_verification demotion to HTTP-only | GREEN | Feature was already broken (set_var approach never worked); HTTP-only is strictly better than false sense of security |
| check_parts_columns strict-fail breaks existing workflows | YELLOW | Only triggers when `check_parts_columns=true` (default false) AND unfiltered inconsistencies exist; `--skip-check-parts-columns` override available |
| env-key translation table maintenance burden | GREEN | Static lookup table derived from existing apply_env_overlay(); tested exhaustively |
| mock_s3_client change breaks test expectations | GREEN | Sync tests only use bucket/prefix fields; struct construction is unchanged |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `S3 disable_cert_verification=true` | yes (if enabled) | Warning about insecure mode now using HTTP fallback |
| `S3 disable_ssl=true: forcing HTTP endpoint` | yes (if enabled) | Info about http:// scheme enforcement |
| `Parts column consistency check found.*inconsistencies` | no (forbidden when strict) | Should NOT appear; replaced by error return |
| `Translated env-style key` | yes (if --env with env-style key used) | Debug log showing key translation |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| Full HTTPS cert skip verification | AWS SDK Rust has no public API for this in v1.1.10 | Monitor aws-smithy-http-client for TlsContext::danger_accept_invalid_certs() API |
| 3 async S3 retry tests offline mode | Tests intentionally exercise real error paths | Could use aws-smithy-http-client test_util mock in future |
| url_encode multi-byte UTF-8 correctness | collect.rs uses byte-level encoding while others use char-level; unified module uses byte-level (correct) | N/A -- fixed by this plan |

## Dependency Groups

```
Group A (Foundation -- no dependencies):
  - Task 1: Create src/path_encoding.rs with canonical encoder + sanitization
  - Task 4: Wire s3.disable_ssl into S3Client construction
  - Task 5: check_parts_columns strict-fail
  - Task 6: --env supports env-style keys

Group B (Depends on Task 1):
  - Task 7: Replace all 4 url_encode implementations with path_encoding module

Group C (Depends on Task 7):
  - Task 2: Fix s3.disable_cert_verification

Group D (Independent -- test-only):
  - Task 3: Hermetic S3 unit tests

Group E (Final -- depends on all):
  - Task 8: Update CLAUDE.md for all modified modules
```

## Tasks

### Task 1: Create src/path_encoding.rs with canonical encoder + sanitization

**TDD Steps:**
1. Write test `test_encode_path_component_basic` in new module: encode ASCII letters, digits, `-`, `_`, `.` are preserved; spaces and special chars are percent-encoded
2. Write test `test_encode_path_component_no_slash_preservation`: verify `/` is percent-encoded (unlike old url_encode_path)
3. Write test `test_encode_path_component_multibyte_utf8`: verify multi-byte chars use byte-level percent-encoding (e.g., `"cafe\u{0301}"` encodes each UTF-8 byte)
4. Write test `test_sanitize_path_component_blocks_dotdot`: input `".."` returns `""` (rejected, not encoded)
5. Write test `test_sanitize_path_component_blocks_dot`: input `"."` returns `""` (rejected)
6. Write test `test_sanitize_path_component_strips_leading_slash`: input `"/foo"` returns `"foo"` (leading slash stripped)
7. Write test `test_sanitize_path_component_normal_names`: normal db/table names pass through encoding unchanged
8. Implement `pub fn encode_path_component(s: &str) -> String` -- percent-encodes non-safe chars, does NOT preserve `/`; uses byte-level encoding for multi-byte chars (matching collect.rs pattern)
9. Implement `pub fn sanitize_path_component(s: &str) -> String` -- returns `""` for `""`, `"."`, `".."` (explicit rejection); strips leading `/` chars; then delegates to `encode_path_component()`
9. Add `pub mod path_encoding;` to src/lib.rs (after `pub mod object_disk;` alphabetically)
10. Verify all tests pass

**Implementation Notes:**
- Safe chars: `is_alphanumeric() || c == '-' || c == '_' || c == '.'`
- NOT safe: `/` (this is the key difference from 3 of the 4 old implementations)
- Byte-level encoding: `for byte in c.to_string().as_bytes() { format!("%{:02X}", byte) }` (matches collect.rs:36-38)
- `sanitize_path_component` MUST explicitly reject `""`, `"."`, and `".."` by returning `""` — NOT by encoding them. A `"."` in safe chars means `".."` would survive as `".."` and still traverse when rejoined. Explicit rejection is required.
- Callers that split on `/` and iterate components must skip empty returns from `sanitize_path_component`
- All callers pass individual db or table names (not full paths), so not preserving `/` is correct

**Files:** src/path_encoding.rs (NEW), src/lib.rs
**Acceptance:** F001

### Task 2: Fix s3.disable_cert_verification

**TDD Steps:**
1. Write test `test_disable_cert_verification_removes_env_var_approach`: verify the `std::env::set_var("AWS_CA_BUNDLE", "")` line is gone (structural grep)
2. Write test `test_disable_cert_verification_forces_http`: when `disable_cert_verification=true` AND endpoint is `https://...`, the effective endpoint is rewritten to `http://...`
3. Remove the broken `std::env::set_var("AWS_CA_BUNDLE", "")` code block at s3.rs:150-162
4. Add logic: when `config.disable_cert_verification` is true, rewrite endpoint URL scheme from `https://` to `http://` (same approach as disable_ssl in Task 4), log warning about insecure HTTP mode
5. If endpoint is empty (default AWS), log error and bail: "disable_cert_verification requires an explicit endpoint URL (cannot downgrade default AWS HTTPS)"
6. Verify compilation and test pass

**Implementation Notes:**
- This task depends on Task 7 completing (not for technical reasons but for clean ordering since Task 4 also modifies s3.rs endpoint handling)
- Actually depends on Task 4 being done first so we can share the endpoint rewriting logic
- The `disable_cert_verification` block should run AFTER the `disable_ssl` block (both may rewrite endpoint)
- If both `disable_ssl` and `disable_cert_verification` are true, only one rewrite happens (idempotent)
- The AWS SDK for Rust does NOT expose a public API to skip TLS certificate verification in aws-smithy-http-client v1.1.10's TlsContext -- only TrustStore configuration. Therefore, the pragmatic fix is to force HTTP when cert verification is disabled, which matches the Go clickhouse-backup behavior (which also degrades to HTTP for self-signed cert scenarios).

**Files:** src/storage/s3.rs
**Acceptance:** F002

### Task 3: Hermetic S3 unit tests

**TDD Steps:**
1. Write test `test_mock_s3_client_no_tls_init` concept: verify that `mock_s3_client` for sync tests does NOT trigger TLS initialization
2. Create `mock_s3_fields(bucket: &str, prefix: &str) -> S3Client` helper: constructs `S3Client` struct with a dummy `inner` via `aws_sdk_s3::Client::from_conf(aws_sdk_s3::config::Builder::new().behavior_version_latest().build())` — the key insight is that TLS native-root init only fires when the runtime first connects; building the client object alone is safe
3. Verify the above by running `cargo test --locked --offline` after the change — if TLS init still fires at build time, use an HTTP-only connector: `aws_sdk_s3::config::Builder::new().behavior_version_latest().http_client(aws_smithy_runtime::client::http::hyper_014::HyperClientBuilder::new().build_http()).build()`
4. Replace 5 sync test calls to `mock_s3_client` with `mock_s3_fields`
5. Add `#[ignore]` attribute to the 3 async retry tests with comment `// Requires network: tests real S3 error paths`
6. Verify `cargo test --locked --offline` passes completely (all non-ignored tests green)

**Implementation Notes:**
- 5 sync tests to update: `test_full_key_with_prefix`, `test_full_key_with_trailing_slash_prefix`, `test_full_key_empty_prefix`, `test_full_key_nested_prefix`, `test_copy_object_builds_correct_source`
- 3 async tests to mark `#[ignore]`: `test_copy_object_with_retry_no_streaming_when_disabled`, `test_put_object_retry_config`, `test_upload_part_retry_config`
- The `mock_s3_fields` function builds `S3Client { inner: ..., bucket, prefix, storage_class, sse, sse_kms_key_id, acl }` — always include `.behavior_version_latest()` (required for compilation); if TLS still fires at object construction, use the HTTP-only connector fallback in step 3
- **Acceptance gate**: `cargo test --locked --offline` must pass in full (not just the 5 sync tests in isolation)

**Files:** src/storage/s3.rs (test section only)
**Acceptance:** F003

### Task 4: Wire s3.disable_ssl into S3Client construction

**TDD Steps:**
1. Write test `test_disable_ssl_forces_http_scheme`: construct S3Config with `disable_ssl=true` and `endpoint="https://minio:9000"`, verify the endpoint used is `"http://minio:9000"`
2. Write test `test_disable_ssl_no_change_when_already_http`: S3Config with `disable_ssl=true` and `endpoint="http://minio:9000"` stays `"http://minio:9000"`
3. Write test `test_disable_ssl_empty_endpoint`: S3Config with `disable_ssl=true` and empty endpoint, verify warning logged and no crash
4. In `S3Client::new()`, after the endpoint check at line 77, add: if `config.disable_ssl && !config.endpoint.is_empty()`, replace `https://` prefix in endpoint with `http://`
5. Use the rewritten endpoint for both `loader.endpoint_url()` and `s3_config_builder.endpoint_url()`
6. Log `info!("S3 disable_ssl=true: forcing HTTP endpoint")` when rewriting
7. Verify compilation and test pass

**Implementation Notes:**
- The endpoint rewriting should happen BEFORE the `loader = loader.endpoint_url(...)` call at s3.rs:78
- Need to create a local `effective_endpoint` variable that starts as `config.endpoint.clone()` and may be mutated
- When `disable_ssl` is true and endpoint is empty (default AWS): log `warn!("S3 disable_ssl is true but no endpoint configured; default AWS endpoints always use HTTPS")` and continue without rewriting (user may intend to set endpoint later via env var)
- The rewrite is simple string replacement: `effective_endpoint.replacen("https://", "http://", 1)`

**Files:** src/storage/s3.rs
**Acceptance:** F004

### Task 5: check_parts_columns strict-fail

**TDD Steps:**
1. Write test `test_check_parts_columns_strict_fail`: mock scenario where `filter_benign_type_drift` returns non-empty Vec, verify function returns `Err` with descriptive message
2. Write test `test_check_parts_columns_benign_drift_passes`: mock scenario where all inconsistencies are benign (filtered out), verify function does NOT return error
3. Write test `test_check_parts_columns_query_error_continues`: when the check query itself fails (Err from ch.check_parts_columns), verify backup continues (warn only, no error return)
4. Modify src/backup/mod.rs:201-214: when `!actionable.is_empty()`, after logging warnings, return `Err` instead of logging `info!("proceeding anyway")`
5. The error message: `"Parts column consistency check found {count} inconsistencies. Use --skip-check-parts-columns to bypass."`
6. Keep the query-level error handling (line 219-224) as warn-only (don't change Err(e) behavior)
7. Verify existing test `test_parts_columns_check_skip_benign_types` still passes

**Implementation Notes:**
- Change at backup/mod.rs:211-214: replace `info!("Parts column consistency check found inconsistencies (proceeding anyway)")` with `bail!("Parts column consistency check found {} actionable inconsistencies across tables. Use --skip-check-parts-columns to bypass this check.", actionable.len())`
- The `bail!` macro is already imported (used at line 185)
- The warn!() lines for individual inconsistencies (lines 203-209) stay as-is -- they provide detail before the error
- Query-level errors (lines 219-224) remain warn-only per design doc behavior (query failure should not block backup)
- Default behavior unchanged: `check_parts_columns` defaults to `false`, so most users never hit this code path

**Files:** src/backup/mod.rs
**Acceptance:** F005

### Task 6: --env supports env-style keys (S3_BUCKET=val)

**TDD Steps:**
1. Write test `test_env_key_to_dot_notation_known_keys`: verify `"S3_BUCKET"` -> `Some("s3.bucket")`, `"CLICKHOUSE_HOST"` -> `Some("clickhouse.host")`
2. Write test `test_env_key_to_dot_notation_unknown_key`: verify `"UNKNOWN_KEY"` -> `None`
3. Write test `test_env_key_to_dot_notation_chbackup_prefix`: verify `"CHBACKUP_LOG_LEVEL"` -> `Some("general.log_level")`
4. Write test `test_cli_env_override_with_env_style_key`: `Config::default()` then apply `apply_cli_env_overrides(&["S3_BUCKET=test-bucket"])`, verify `config.s3.bucket == "test-bucket"`
5. Write test `test_cli_env_override_dot_notation_still_works`: verify existing dot-notation `"s3.bucket=test"` still works
6. Implement `fn env_key_to_dot_notation(key: &str) -> Option<&'static str>`: static match table mapping uppercase env var names to dot-notation keys, covering the same 54+ mappings from `apply_env_overlay()`
7. Modify `apply_cli_env_overrides()`: before calling `self.set_field(key, value)`, try `env_key_to_dot_notation(key)` first; if `Some(dot_key)`, use `dot_key`; if `None`, pass original `key` to `set_field()` (preserving existing dot-notation behavior)
8. Verify compilation and all tests pass

**Implementation Notes:**
- The mapping table is a `match` on the input key returning `Option<&'static str>`
- Keys from `apply_env_overlay()` use `CHBACKUP_` prefix for some vars (e.g., `CHBACKUP_LOG_LEVEL`) and direct names for others (e.g., `S3_BUCKET`, `CLICKHOUSE_HOST`)
- Must handle BOTH: `S3_BUCKET` and `CHBACKUP_LOG_LEVEL` patterns
- The match must be exhaustive for all 54+ keys documented in apply_env_overlay()
- The translated key is logged at debug level: `debug!(env_key = %key, dot_key = %translated, "Translated env-style key to dot notation")`
- Location: add `env_key_to_dot_notation` as a private function in config.rs, near `apply_cli_env_overrides`

**Files:** src/config.rs
**Acceptance:** F006

### Task 7: Replace all 4 url_encode implementations with path_encoding module

**TDD Steps:**
1. Replace `url_encode_path()` in src/backup/collect.rs:29-42 with `use crate::path_encoding::encode_path_component;` and update all call sites (lines 383-384)
2. Replace `url_encode()` in src/download/mod.rs:41-51 with `use crate::path_encoding::encode_path_component;` and update call sites (lines 176-177, 522-523, 847, 852)
3. Replace `url_encode_component()` in src/upload/mod.rs:55-65 with `use crate::path_encoding::encode_path_component;` and update call sites (lines 81-82, 345-346, 814-815, 1084-1085)
4. Replace `url_encode()` in src/restore/attach.rs:844-854 with `use crate::path_encoding::encode_path_component;` and update call sites (lines 316-317, 556-557)
5. Update src/restore/mod.rs call sites that reference `attach::url_encode` (lines 994-995) to use `crate::path_encoding::encode_path_component`
6. For download/mod.rs path traversal fix (Issue 1): at lines 562-592 (metadata file write), use `sanitize_path_component` on `relative_name` before `shadow_dir.join(relative_name)`
7. Remove all 4 old function definitions (collect.rs, download/mod.rs, upload/mod.rs, attach.rs)
8. Migrate existing tests from old modules to path_encoding.rs test module (or verify coverage via existing integration tests)
9. Verify `cargo test` passes
10. Verify `grep -rn 'fn url_encode' src/` returns NO results

**Implementation Notes:**
- **Critical behavioral difference**: 3 of 4 old implementations preserve `/`; the new `encode_path_component` does NOT preserve `/`. This is CORRECT because all call sites pass individual db or table names (not paths containing `/`). If a db/table name contains `/`, encoding it is the RIGHT thing (preventing path traversal).
- **collect.rs:383-384**: `url_encode_path(&db)` -> `encode_path_component(&db)`. The old `url_encode_path` preserved `/` but db names should never contain `/`.
- **download:522-523,847,852**: `url_encode(&item.db)` -> `encode_path_component(&item.db)`. Same reasoning.
- **upload:81-82,345-346,814-815,1084-1085**: `url_encode_component(&db)` -> `encode_path_component(&db)`. This was already not preserving `/`, so behavior is identical.
- **restore:316-317,556-557,994-995**: `url_encode(&source_db)` -> `encode_path_component(&source_db)`. Same reasoning.
- **Path traversal fix (Issue 1)**: In download/mod.rs at line 592 (`shadow_dir.join(relative_name)`), the `relative_name` comes from S3 object keys after stripping the prefix. A crafted key could contain `..`. Fix: use `std::path::Path::new(relative_name).components()` and collect only `std::path::Component::Normal(c)` components (skipping `ParentDir`, `RootDir`, `CurDir`, `Prefix`). This is the correct approach — `Path::components()` parses at the type level so `..` maps to `ParentDir` (rejected) rather than a string that might slip through encoding. Rejoin with `PathBuf` `push()` calls.

**Files:** src/backup/collect.rs, src/download/mod.rs, src/upload/mod.rs, src/restore/attach.rs, src/restore/mod.rs
**Acceptance:** F007

### Task 8: Update CLAUDE.md for all modified modules (MANDATORY)

**TDD Steps:**
1. Read affected-modules.json for module list
2. For each module, regenerate directory tree
3. Update Key Patterns sections:
   - **src/backup/CLAUDE.md**: Note check_parts_columns now returns error (strict-fail) instead of warn+continue; note url_encode_path replaced by path_encoding::encode_path_component
   - **src/download/CLAUDE.md**: Note path sanitization via Path::components() Normal-only filter for metadata file writes; note url_encode replaced by path_encoding::encode_path_component
   - **src/upload/CLAUDE.md**: Note url_encode_component replaced by path_encoding::encode_path_component
   - **src/restore/CLAUDE.md**: Note url_encode replaced by path_encoding::encode_path_component
   - **src/storage/CLAUDE.md**: Note disable_cert_verification now forces HTTP endpoint (broken env var approach removed); note disable_ssl wiring; note hermetic mock_s3_fields for sync tests
4. Update **docs/design.md** for behavioral contract changes:
   - `--env` section (§2 / around line 936-938): add that env-style keys (`S3_BUCKET=val`) are now accepted in addition to dot-notation (`s3.bucket=val`)
   - `s3.disable_cert_verification` entry (§12 / around line 2539): update semantics to reflect that this now forces the endpoint to HTTP (not a TLS skip); add note that an explicit endpoint URL is required
5. Validate all CLAUDE.md files have required sections

**Files:** src/backup/CLAUDE.md, src/download/CLAUDE.md, src/upload/CLAUDE.md, src/restore/CLAUDE.md, src/storage/CLAUDE.md, docs/design.md
**Acceptance:** FDOC

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-015 | PASS | All symbols verified via knowledge_graph.json and grep |
| RC-016 | PASS | Tests match implementation (test names specified per task) |
| RC-017 | PASS | All F00X IDs map to exactly one task |
| RC-018 | PASS | Task ordering respects dependencies (Task 1 before Task 7, Task 4 before Task 2) |
| RC-006 | PASS | All API calls verified: bail!, encode_path_component, set_field, etc. |
| RC-008 | PASS | encode_path_component defined in Task 1, used in Task 7 (preceding task) |
| RC-019 | PASS | sanitize_path_component follows url_encode_path pattern from collect.rs |
| RC-021 | PASS | All file locations verified via grep/LSP in context/references.md |

## Notes

### Phase 4.5 Skip Justification

Interface skeleton simulation is SKIPPED for this plan because:
- No new public structs or complex type signatures are introduced
- `path_encoding.rs` exports only two simple `fn(&str) -> String` functions
- All modified code is within existing functions (changing behavior, not API signatures)
- The `env_key_to_dot_notation` function is private and returns `Option<&'static str>`

### Anti-Overengineering Notes

- `sanitize_path_component` is a THIN wrapper over `encode_path_component` (strips leading `/`, delegates). Not a separate complex sanitization system.
- `env_key_to_dot_notation` is a flat match statement, not a HashMap or external config file. Zero allocations.
- `disable_cert_verification` -> HTTP fallback is pragmatic, not perfect. Full HTTPS-without-verification requires future AWS SDK support.
