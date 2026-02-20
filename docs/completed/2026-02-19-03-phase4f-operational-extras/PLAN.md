# Plan: Phase 4f -- Operational Extras

## Goal

Implement four operational features: `tables` command (list tables from ClickHouse or remote backup), JSON/Object column detection during backup pre-flight, enhanced `list` output with compressed size, and additional compression formats (gzip, zstd, none) beyond the current lz4-only pipeline.

## Architecture Overview

This plan modifies 8 existing source files and adds 2 new crate dependencies. No new modules, structs, or public types are created -- all features extend existing code:

- **Tables command:** Replace stub in `main.rs` with dispatch to `ChClient::list_tables()` (live) or manifest download (remote). Uses existing `TableFilter` for `-t` glob.
- **JSON/Object detection:** New `ChClient::check_json_columns()` method querying `system.columns`, called from backup pre-flight in `backup/mod.rs`.
- **List enhancement:** Add `compressed_size` column to `print_backup_table()` in `list.rs`.
- **Compression formats:** Add `format: &str` and `level: u32` params to `compress_part()` / `decompress_part()` in both `upload/stream.rs` and `download/stream.rs`. Add `data_format: &str` param to `s3_key_for_part()`. Pass `manifest.data_format` through download pipeline. Add `flate2` and `zstd` crate dependencies.

## Architecture Assumptions (VALIDATED)

### Component Ownership
- `compress_part()`: Owned by `upload/stream.rs` and `download/stream.rs` (duplicate for test use). Called from `upload/mod.rs:481` and download tests.
- `decompress_part()`: Owned by `download/stream.rs`. Called from `download/mod.rs:457`.
- `s3_key_for_part()`: Private function in `upload/mod.rs:68`. Called within same file only.
- `list_tables()`: Method on `ChClient` in `client.rs:276`. Called from `backup/mod.rs` and will be called from tables command.
- `print_backup_table()`: Private function in `list.rs:907`. Called from `list()` at lines 61, 72.
- `manifest.data_format`: Set in `backup/mod.rs:529` from `config.backup.compression`. Set again in `upload/mod.rs:726`. Read during download but NOT used for decompressor selection (key gap).

### What This Plan CANNOT Do
- Cannot test compression formats end-to-end without real ClickHouse + S3 (integration test infra)
- Cannot add streaming compression (would require multipart redesign)
- The `tables` command for remote backup requires S3 connectivity (cannot mock)
- `check_json_columns` query requires a ClickHouse instance with JSON-type columns to truly test

### Key Type Facts (Verified from Source)
- `TableRow.total_bytes` is `Option<u64>` (NOT `u64`) -- client.rs:32
- `ColumnInconsistency.types` is `Vec<String>` -- client.rs:75
- `BackupConfig.compression_level` is `u32` -- config.rs:339
- `BackupManifest.data_format` is `String` with default "lz4" -- manifest.rs:40
- `TableFilter::matches()` always excludes system databases -- table_filter.rs:48
- `TableFilter.patterns` is **private** (not `pub`) -- table_filter.rs:19. Need `matches_including_system()` method for `--all` flag.
- `format_size()` is **private** (`fn`, not `pub fn`) in list.rs:887. Need `pub(crate)` for tables command.
- `BackupManifest::from_json_bytes()` exists at manifest.rs:233 -- can use directly.

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| Compression format mismatch between upload and download | YELLOW | Unit tests verify roundtrip for all 4 formats. Download reads `manifest.data_format` to select decompressor. |
| S3 key extension mismatch | GREEN | `s3_key_for_part()` and `compress_part()` both derive extension from same `data_format` parameter |
| `list_tables()` system DB filter bypass for `--all` | GREEN | New `list_all_tables()` method reuses same SQL pattern minus WHERE clause |
| JSON column detection false positives | GREEN | Warning only, never blocks backup. Users can evaluate and exclude tables manually. |
| Existing tests hardcode `.tar.lz4` | GREEN | Tests use hardcoded backup_key values in test manifests -- these remain valid as test data format is still "lz4" unless explicitly changed |
| zstd/flate2 crate API stability | GREEN | Both are mature, widely-used crates with stable APIs |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `tables_count=` | yes (tables cmd) | Number of tables listed |
| `Tables command complete` | yes (tables cmd) | Command completion marker |
| `JSON/Object columns detected` | yes (backup with JSON cols) | Warning when JSON columns found |
| `JSON/Object column type check passed` | yes (backup without JSON cols) | Clean check message |
| `Compressing.*format=` | yes (upload) | Compression format logged per part |
| `ERROR:` | no (forbidden) | Should NOT appear in normal operation |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| Streaming multipart compression | Requires full pipeline redesign | Phase 5+ |
| Compression ratio statistics in list | Would need per-part ratio tracking | Future enhancement |
| `tables` command with `--local-backup` flag | Not in CLI definition; design only has `--remote-backup` | Deferred |
| `zstd` dictionary training | Advanced optimization not needed for MVP | Deferred |

## Dependency Groups

```
Group A (Independent -- Feature 3: List Enhancement):
  - Task 1: Add compressed size to print_backup_table()

Group B (Independent -- Feature 2: JSON Column Detection):
  - Task 2: Add check_json_columns() to ChClient
  - Task 3: Integrate JSON column check into backup pre-flight (depends on Task 2)

Group C (Independent -- Feature 1: Tables Command):
  - Task 4: Implement tables command

Group D (Independent -- Feature 4: Compression Formats):
  - Task 5: Add zstd and flate2 crate dependencies
  - Task 6: Add multi-format compress_part() and decompress_part() (depends on Task 5)
  - Task 7: Wire format through upload and download pipelines (depends on Task 6)

Group E (Final -- Documentation):
  - Task 8: Update CLAUDE.md for all modified modules (depends on all above)
```

## Tasks

### Task 1: Add compressed size column to list output

**Description:** Enhance `print_backup_table()` in `list.rs` to show the `compressed_size` field already present in `BackupSummary`. Add a column between the existing `size` and `table_count` columns.

**TDD Steps:**
1. Write unit test `test_print_backup_table_shows_compressed_size`:
   - Create a `BackupSummary` with `size: 1048576` (1 MB) and `compressed_size: 524288` (512 KB)
   - Capture stdout output
   - Assert output contains both `1.00 MB` and `512.00 KB`
2. Modify `print_backup_table()` to add compressed size column:
   - After `let size_str = format_size(s.size);` add `let compressed_str = format_size(s.compressed_size);`
   - Update `println!` format to: `"  {}{}\t{}\t{}\t{}\t{} tables"` with `compressed_str` after `size_str`
3. Verify test passes
4. Run `cargo fmt` and `cargo clippy`

**Files:**
- `src/list.rs` (modify `print_backup_table()` at line 907)

**Acceptance:** F001

---

### Task 2: Add check_json_columns() to ChClient

**Description:** Add a new method to `ChClient` that queries `system.columns` for columns with Object or JSON types. Follows the exact pattern of `check_parts_columns()` at `client.rs:604-661`.

**TDD Steps:**
1. Write unit test `test_json_column_row_struct`:
   - Verify the inner row struct can be deserialized from expected query output format
   - Test with sample data: `{ database: "default", table: "events", column: "metadata", type: "Object('json')" }`
2. Implement `check_json_columns()`:
   - Signature: `pub async fn check_json_columns(&self, targets: &[(String, String)]) -> Result<Vec<JsonColumnInfo>>`
   - Define `JsonColumnInfo` struct: `pub struct JsonColumnInfo { pub database: String, pub table: String, pub column: String, pub column_type: String }`
   - Build IN clause from `targets` (same pattern as `check_parts_columns`)
   - SQL: `SELECT database, table, name AS column, type AS column_type FROM system.columns WHERE (database, table) IN ({in_clause}) AND (type LIKE '%Object%' OR type LIKE '%JSON%')`
   - Inner row struct with `#[derive(clickhouse::Row, serde::Deserialize)]`
   - Conditional SQL logging (`self.log_sql_queries`)
   - Map rows to `JsonColumnInfo`
3. Verify test passes
4. Run `cargo fmt` and `cargo clippy`

**Files:**
- `src/clickhouse/client.rs` (add `JsonColumnInfo` struct near line 76, add `check_json_columns()` method after `check_parts_columns()` at ~line 662)
- `src/clickhouse/mod.rs` (add `pub use client::JsonColumnInfo;` if needed for external use)

**Acceptance:** F002

---

### Task 3: Integrate JSON column check into backup pre-flight

**Description:** Call `check_json_columns()` from `backup/mod.rs` right after the existing `check_parts_columns` block (line 197). Warning only, never blocks backup. Follows the same try/match pattern.

**TDD Steps:**
1. Write unit test `test_json_column_warning_format`:
   - Create a `JsonColumnInfo` with known values
   - Verify the warning message format matches expected output
2. Implement integration in `backup/mod.rs`:
   - After line 197 (end of check_parts_columns block), add:
   ```rust
   // 5c. JSON/Object column type detection (design 16.4)
   match ch.check_json_columns(&targets).await {
       Ok(json_cols) => {
           if !json_cols.is_empty() {
               for col in &json_cols {
                   warn!(
                       database = %col.database,
                       table = %col.table,
                       column = %col.column,
                       column_type = %col.column_type,
                       "JSON/Object column detected -- may not FREEZE correctly"
                   );
               }
               info!(
                   count = json_cols.len(),
                   "JSON/Object columns detected (proceeding with backup)"
               );
           } else {
               info!("JSON/Object column type check passed");
           }
       }
       Err(e) => {
           warn!(
               error = %e,
               "JSON/Object column type check failed, continuing anyway"
           );
       }
   }
   ```
3. Verify `cargo check` passes
4. Run `cargo fmt` and `cargo clippy`

**Files:**
- `src/backup/mod.rs` (add block after line 197)

**Acceptance:** F002

**Notes:**
- This task depends on Task 2 (check_json_columns must exist first)
- The check uses the same `targets` Vec already built for `check_parts_columns`
- No config gate needed -- always runs (zero-cost query, warning only)

---

### Task 4: Implement tables command

**Description:** Replace the stub in `main.rs:382-384` with a full implementation of the `tables` command. Two modes: live ClickHouse query (default) and remote backup manifest query (`--remote-backup`).

**TDD Steps:**
1. Write unit test `test_table_filter_matches_including_system`:
   - Create `TableFilter::new("system.*")`
   - Verify `matches("system", "tables")` returns `false` (existing behavior)
   - Verify `matches_including_system("system", "tables")` returns `true`
2. Add `matches_including_system()` method to `TableFilter` in `table_filter.rs`:
   - Same as `matches()` but WITHOUT the `is_system_database(db)` early return
   ```rust
   /// Check if the given database.table combination matches any pattern.
   /// Unlike `matches()`, does NOT exclude system databases.
   pub fn matches_including_system(&self, db: &str, table: &str) -> bool {
       let full_name = format!("{db}.{table}");
       self.patterns.iter().any(|p| p.matches(&full_name))
   }
   ```
3. Write unit test `test_list_all_tables_sql_no_system_filter`:
   - Verify `list_all_tables()` SQL does NOT contain `WHERE database NOT IN`
   - (Note: actual DB test requires integration test)
4. Implement `list_all_tables()` in `ChClient`:
   - Signature: `pub async fn list_all_tables(&self) -> Result<Vec<TableRow>>`
   - Same SQL as `list_tables()` but WITHOUT the `WHERE database NOT IN (...)` clause
5. Make `format_size()` in `list.rs` accessible: change `fn format_size` to `pub(crate) fn format_size`
6. Implement tables command dispatch in `main.rs`:
   ```rust
   Command::Tables { tables, all, remote_backup } => {
       if let Some(backup_name) = remote_backup {
           // Remote mode: download manifest and list tables
           let s3 = S3Client::new(&config.s3).await?;
           let manifest_key = format!("{}/metadata.json", backup_name);
           let manifest_data = s3.get_object(&manifest_key).await
               .with_context(|| format!("Failed to download manifest for backup '{}'", backup_name))?;
           let manifest = BackupManifest::from_json_bytes(&manifest_data)
               .context("Failed to parse backup manifest")?;

           let filter = tables.as_deref().map(TableFilter::new);

           for (full_name, tm) in &manifest.tables {
               // Parse "db.table" key
               let parts: Vec<&str> = full_name.splitn(2, '.').collect();
               let (db, tbl) = if parts.len() == 2 {
                   (parts[0], parts[1])
               } else {
                   (full_name.as_str(), "")
               };

               if let Some(ref f) = filter {
                   if !f.matches(db, tbl) { continue; }
               }

               let total: u64 = tm.parts.values()
                   .flat_map(|v| v.iter())
                   .map(|p| p.size)
                   .sum();
               println!("  {}\t{}\t{}", full_name, tm.engine, list::format_size(total));
           }

           info!(
               backup_name = %backup_name,
               tables_count = manifest.tables.len(),
               "Tables command complete (remote)"
           );
       } else {
           // Live mode: query ClickHouse
           let ch = ChClient::new(&config.clickhouse)?;
           ch.ping().await?;

           let rows = if all {
               ch.list_all_tables().await?
           } else {
               ch.list_tables().await?
           };

           let filter = tables.as_deref().map(TableFilter::new);

           let filtered: Vec<&TableRow> = rows.iter()
               .filter(|t| {
                   if let Some(ref f) = filter {
                       if all {
                           f.matches_including_system(&t.database, &t.name)
                       } else {
                           f.matches(&t.database, &t.name)
                       }
                   } else {
                       true
                   }
               })
               .collect();

           for t in &filtered {
               let bytes = t.total_bytes.unwrap_or(0);
               println!(
                   "  {}.{}\t{}\t{}",
                   t.database, t.name, t.engine, list::format_size(bytes)
               );
           }

           info!(
               tables_count = filtered.len(),
               "Tables command complete"
           );
       }
   }
   ```
7. Verify `cargo check` passes
8. Run `cargo fmt` and `cargo clippy`

**Implementation Notes:**
- `format_size` is private in `list.rs` -- make it `pub(crate)` so `main.rs` can use it as `list::format_size()`
- `TableFilter.patterns` is **private** (verified at table_filter.rs:19 -- NO `pub`). Adding `matches_including_system()` method avoids exposing internals.
- `BackupManifest::from_json_bytes()` exists at manifest.rs:233. Returns `Result<Self>`.
- No PID lock needed (read-only command per design section 2)

**Files:**
- `src/main.rs` (replace stub at lines 382-384)
- `src/clickhouse/client.rs` (add `list_all_tables()` method)
- `src/list.rs` (make `format_size()` `pub(crate)` instead of private)
- `src/table_filter.rs` (add `matches_including_system()` method)

**Acceptance:** F003

---

### Task 5: Add zstd and flate2 crate dependencies

**Description:** Add `flate2` and `zstd` crate dependencies to `Cargo.toml`. These are needed for gzip and zstd compression format support.

**TDD Steps:**
1. Add dependencies to `Cargo.toml` under `[dependencies]`:
   ```toml
   # Compression
   lz4_flex = "0.11"
   flate2 = "1"
   zstd = "0.13"
   ```
2. Run `cargo check` to verify dependencies resolve
3. Verify zero compilation errors

**Files:**
- `Cargo.toml` (add 2 lines in compression section)

**Acceptance:** F004

---

### Task 6: Add multi-format compress_part() and decompress_part()

**Description:** Extend `compress_part()` in both `upload/stream.rs` and `download/stream.rs` to accept a `data_format: &str` parameter. Extend `decompress_part()` in `download/stream.rs` to accept `data_format: &str`. Add a helper function `archive_extension(data_format: &str) -> &str` for consistent extension mapping.

**TDD Steps:**
1. Write unit test `test_compress_decompress_zstd_roundtrip`:
   - Create temp directory with test files
   - Call `compress_part(dir, "test_part", "zstd", 3)`
   - Call `decompress_part(compressed, output_dir, "zstd")`
   - Verify files match original content
2. Write unit test `test_compress_decompress_gzip_roundtrip`:
   - Same pattern with format "gzip" and level 6
3. Write unit test `test_compress_decompress_none_roundtrip`:
   - Same pattern with format "none" and level 0
4. Write unit test `test_compress_decompress_lz4_roundtrip_updated`:
   - Same pattern with format "lz4" to verify backward compatibility
5. Write unit test `test_archive_extension_mapping`:
   - Assert `archive_extension("lz4") == ".tar.lz4"`
   - Assert `archive_extension("zstd") == ".tar.zstd"`
   - Assert `archive_extension("gzip") == ".tar.gz"`
   - Assert `archive_extension("none") == ".tar"`
6. Implement in `upload/stream.rs`:
   - Add `pub fn archive_extension(data_format: &str) -> &str` helper
   - Change signature to `pub fn compress_part(part_dir: &Path, archive_name: &str, data_format: &str, compression_level: u32) -> Result<Vec<u8>>`
   - Match on `data_format`:
     - `"lz4"`: existing `lz4_flex::frame::FrameEncoder` (ignores level)
     - `"zstd"`: `zstd::Encoder::new(Vec::new(), level as i32)?.auto_finish()` -- tar into encoder, call `finish()`
     - `"gzip"`: `flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level))` -- tar into encoder, call `finish()`
     - `"none"`: tar directly into `Vec<u8>` (no compression)
     - Other: return `Err(anyhow!("Unknown compression format: {}", data_format))`
7. Implement same changes in `download/stream.rs`:
   - `compress_part` (for test roundtrips): same signature change
   - `decompress_part`: change signature to `pub fn decompress_part(data: &[u8], output_dir: &Path, data_format: &str) -> Result<()>`
   - Match on `data_format`:
     - `"lz4"`: existing `lz4_flex::frame::FrameDecoder`
     - `"zstd"`: `zstd::Decoder::new(data)?`
     - `"gzip"`: `flate2::read::GzDecoder::new(data)`
     - `"none"`: `std::io::Cursor::new(data)` (just untar)
     - Other: return error
   - `decompress_lz4()` remains unchanged (used by other code for raw LZ4)
8. Update existing tests in both files to pass `"lz4"` and `1` as format/level params
9. Verify all tests pass
10. Run `cargo fmt` and `cargo clippy`

**Files:**
- `src/upload/stream.rs` (modify `compress_part`, add `archive_extension`)
- `src/download/stream.rs` (modify `decompress_part`, `compress_part`)

**Acceptance:** F004

**Notes:**
- `zstd::Encoder` uses `i32` for level -- cast from `u32` with `level as i32`
- `flate2::Compression::new()` takes `u32` -- matches `compression_level` type directly
- `lz4_flex::FrameEncoder` ignores level (always uses default LZ4 compression)
- Keep `decompress_lz4()` unchanged in download/stream.rs (standalone utility)

---

### Task 7: Wire format through upload and download pipelines

**Description:** Pass `data_format` and `compression_level` through the upload pipeline to `compress_part()` and update `s3_key_for_part()` to use dynamic extension. Pass `manifest.data_format` through the download pipeline to `decompress_part()`.

**TDD Steps:**
1. Write unit test `test_s3_key_for_part_with_format`:
   - `s3_key_for_part("daily", "db", "t", "part1", "lz4")` -> ends with `.tar.lz4`
   - `s3_key_for_part("daily", "db", "t", "part1", "zstd")` -> ends with `.tar.zstd`
   - `s3_key_for_part("daily", "db", "t", "part1", "gzip")` -> ends with `.tar.gz`
   - `s3_key_for_part("daily", "db", "t", "part1", "none")` -> ends with `.tar`
2. Update `s3_key_for_part()` signature:
   - From: `fn s3_key_for_part(backup_name: &str, db: &str, table: &str, part_name: &str) -> String`
   - To: `fn s3_key_for_part(backup_name: &str, db: &str, table: &str, part_name: &str, data_format: &str) -> String`
   - Use `stream::archive_extension(data_format)` for the extension
3. Update all callers of `s3_key_for_part` in `upload/mod.rs` to pass `data_format`
4. Update `compress_part` call site in `upload/mod.rs:481`:
   - From: `stream::compress_part(&part_dir, &part_name_for_compress)`
   - To: `stream::compress_part(&part_dir, &part_name_for_compress, &data_format_clone, compression_level)`
   - Clone `data_format` and `config.backup.compression_level` into the spawn block
5. Update `decompress_part` call site in `download/mod.rs:457`:
   - The manifest is available in the download function scope
   - Pass `&manifest.data_format` to `stream::decompress_part()`
   - From: `stream::decompress_part(&compressed_data, &shadow_dir_clone)`
   - To: `stream::decompress_part(&compressed_data, &shadow_dir_clone, &data_format_clone)`
   - Clone `manifest.data_format` into the spawn block scope
6. Update existing tests in `upload/stream.rs` test `test_s3_key_for_part` to use new signature
7. Verify `cargo check` passes
8. Verify all tests pass
9. Run `cargo fmt` and `cargo clippy`

**Files:**
- `src/upload/mod.rs` (modify `s3_key_for_part()`, update callers, update `compress_part` call)
- `src/download/mod.rs` (update `decompress_part` call to pass format)

**Acceptance:** F004

**Notes:**
- `data_format` is already available in upload as `config.backup.compression` (line 223)
- `compression_level` is `config.backup.compression_level` (u32)
- In download, `manifest.data_format` is loaded from the remote manifest -- this is the authoritative format
- The `download/stream.rs` `compress_part` function is only used in tests; update it for consistency but it does not affect production download path

---

### Task 8: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** src/upload, src/download, src/clickhouse, src/backup

**TDD Steps:**

1. **For src/upload/CLAUDE.md:**
   - Update "Compression Pipeline" section to document multi-format support
   - Add `archive_extension()` to public API list
   - Update `compress_part` signature in docs to include `data_format` and `compression_level`
   - Update S3 Key Format section: `.tar.lz4` -> dynamic extension based on `data_format`
   - Regenerate directory tree

2. **For src/download/CLAUDE.md:**
   - Update "Decompression Pipeline" section for multi-format support
   - Update `decompress_part` signature in docs to include `data_format`
   - Add note about manifest.data_format driving decompressor selection
   - Regenerate directory tree

3. **For src/clickhouse/CLAUDE.md:**
   - Add `JsonColumnInfo` to Row Types section
   - Add `check_json_columns()` to Public API section
   - Add `list_all_tables()` to Public API section
   - Regenerate directory tree

4. **For src/backup/CLAUDE.md:**
   - Add "JSON/Object Column Detection" subsection under Key Patterns
   - Reference design section 16.4
   - Regenerate directory tree

5. **Validate all CLAUDE.md files:**
   - Each has Parent Context, Directory Structure, Key Patterns, Parent Rules sections

**Files:**
- `src/upload/CLAUDE.md`
- `src/download/CLAUDE.md`
- `src/clickhouse/CLAUDE.md`
- `src/backup/CLAUDE.md`

**Acceptance:** FDOC

---

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-015 (symbols match) | PASS | All symbols verified: compress_part, decompress_part, s3_key_for_part, list_tables, check_parts_columns, format_size, print_backup_table, TableFilter::matches, BackupManifest::from_json_bytes |
| RC-016 (tests match impl) | PASS | Test names in TDD steps match implementation functions |
| RC-017 (acceptance IDs match) | PASS | F001, F002, F003, F004, FDOC -- all referenced in tasks |
| RC-018 (dependencies satisfied) | PASS | Task 3 depends on Task 2 (same group). Task 6 depends on Task 5. Task 7 depends on Task 6. Task 8 depends on all. |
| Cross-task types | PASS | `data_format: &str` used consistently in Tasks 6-7. `compression_level: u32` from config used consistently. |
| Cross-task names | PASS | `archive_extension()` defined in Task 6, used in Task 7 (s3_key_for_part). `matches_including_system()` defined in Task 4, used in same task's dispatch code. |
| Verification commands match | PASS | Grep patterns in acceptance.json match actual function declarations |
| Private field access | FIXED | `TableFilter.patterns` is private -- added `matches_including_system()` method instead of direct field access (Phase 7.5 catch) |
| Private function access | FIXED | `format_size()` is private -- plan makes it `pub(crate)` in Task 4 |

## Notes

### Phase 4.5 Skip Justification
Skipped -- all changes are within existing functions (extending signatures) or following exact existing patterns. No new import paths or type definitions that could fail at compile time in novel ways. The `flate2` and `zstd` crates are standard Rust ecosystem crates with well-known APIs.

### Anti-Overengineering Checklist
- [x] No new structs except `JsonColumnInfo` (minimal, follows `ColumnInconsistency` pattern)
- [x] No new modules
- [x] No new public types exported from module boundaries
- [x] `archive_extension()` is a pure function, minimal complexity
- [x] Compression format selection is a simple match, no trait objects or dynamic dispatch
- [x] `matches_including_system()` follows exact pattern of `matches()` minus system exclusion
