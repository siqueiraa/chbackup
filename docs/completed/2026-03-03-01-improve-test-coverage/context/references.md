# Symbol and Reference Analysis

## Phase 1: Verified Function Signatures (via LSP hover)

### src/main.rs -- Pure helper functions (0% coverage, 549 missed lines)

| Function | Verified Signature | Line | Testable | Existing Tests |
|----------|-------------------|------|----------|----------------|
| `backup_name_from_command` | `fn(cmd: &Command) -> Option<&str>` | 24 | YES | NONE |
| `resolve_backup_name` | `fn(name: Option<String>) -> Result<String>` | 726 | YES - calls validate_backup_name | NONE |
| `backup_name_required` | `fn(name: Option<String>, command: &str) -> Result<String>` | 744 | YES | NONE |
| `map_cli_location` | `fn(loc: cli::Location) -> list::Location` | 756 | YES - pure enum mapping | NONE |
| `map_cli_list_format` | `fn(fmt: cli::ListFormat) -> list::ListFormat` | 764 | YES - pure enum mapping | NONE |
| `merge_skip_projections` | `fn(cli_flag: Option<&str>, config_list: &[String]) -> Vec<String>` | 808 | YES - pure string split/merge | NONE |

**Note:** main.rs has NO `#[cfg(test)]` module yet. A new test module must be added at the end of the file.

**Note:** `resolve_backup_name` depends on `validate_backup_name` (from `server/state.rs`, pub fn) and `generate_backup_name` (from `lib.rs`, pub fn). Both are already tested elsewhere. The function is testable because it only does validation and timestamp generation -- no I/O.

**Note:** `backup_name_from_command` depends on `cli::Command` enum. Tests must construct Command variants with ALL required fields (clap structs have many fields). The `cli` module is `mod cli;` in main.rs -- private to the binary crate. Tests MUST be inline in main.rs.

### cli::Command Variant Constructors Required for Tests

Each variant has many fields. To construct test values, ALL fields must be provided:

```rust
// Example: Create variant has 9 fields
Command::Create {
    tables: None,
    partitions: None,
    diff_from: None,
    skip_projections: None,
    schema: false,
    rbac: false,
    configs: false,
    named_collections: false,
    skip_check_parts_columns: false,
    backup_name: Some("test".to_string()),
}
```

Variants with `backup_name`:
- `Create` (10 fields: tables, partitions, diff_from, skip_projections, schema, rbac, configs, named_collections, skip_check_parts_columns, backup_name)
- `Upload` (4 fields: delete_local, diff_from_remote, resume, backup_name)
- `Download` (3 fields: hardlink_exists_files, resume, backup_name)
- `Restore` (13 fields: tables, rename_as, database_mapping, partitions, schema, data_only, rm, resume, rbac, configs, named_collections, skip_empty_tables, backup_name)
- `CreateRemote` (10 fields: tables, diff_from_remote, delete_source, rbac, configs, named_collections, skip_check_parts_columns, skip_projections, resume, backup_name)
- `RestoreRemote` (11 fields: tables, rename_as, database_mapping, rm, rbac, configs, named_collections, skip_empty_tables, resume, backup_name)
- `Delete` (2 fields: location, backup_name)

Variants WITHOUT `backup_name` (return None):
- `List` (2 fields: location, format)
- `Tables` (3 fields: tables, all, remote_backup)
- `Clean` (1 field: name)
- `CleanBroken` (1 field: location)
- `DefaultConfig` (no fields)
- `PrintConfig` (no fields)
- `Server` (3 fields: host, port, watch)
- `Watch` (no fields)
- `Version` (no fields)
- `CleanShadow` (1 field: name)

### src/backup/mod.rs -- Utility functions (46.41% coverage)

| Function | Verified Signature | Line | Testable | Existing Tests |
|----------|-------------------|------|----------|----------------|
| `normalize_uuid` | `fn(uuid: &str) -> Option<String>` | 43 | YES | NONE |
| `parse_partition_list` | `fn(partitions: Option<&str>) -> Vec<String>` | 56 | YES | 3 tests (parsing, freeze_called, all_trigger) |
| `is_ignorable_freeze_error` | `fn(err_msg: &str) -> bool` | 84 | YES | 1 test (comprehensive, line 1035) |
| `is_metadata_only_engine` | `fn(engine: &str) -> bool` | 868 | YES | 1 test (test_is_metadata_only_engine) |
| `is_benign_type` | `fn(type_str: &str) -> bool` | 890 | YES | Partial (via filter_benign_type_drift tests) |
| `filter_benign_type_drift` | `fn(inconsistencies: Vec<ColumnInconsistency>) -> Vec<ColumnInconsistency>` | 902 | YES | 3+ tests exist |

**Correction from discovery:** `is_ignorable_freeze_error` DOES have a test at line 1035 with good coverage (all 6 match arms + 3 negative cases). The discovery phase missed this.

### src/download/mod.rs -- Security-critical function (44.27% coverage)

| Function | Verified Signature | Line | Testable | Existing Tests |
|----------|-------------------|------|----------|----------------|
| `sanitize_relative_path` | `fn(input: &str) -> PathBuf` | 44 | YES - SECURITY-CRITICAL | NONE |
| `resolve_download_target_dir` | `fn(manifest_disks: &BTreeMap<String, String>, disk_name: &str, backup_name: &str, backup_dir: &Path) -> PathBuf` | 62 | PARTIAL - checks Path::exists() | NONE |

### src/restore/attach.rs -- Core restore functions (45.95% coverage)

| Function | Verified Signature | Line | Testable | Existing Tests |
|----------|-------------------|------|----------|----------------|
| `uuid_s3_prefix` | `pub fn(uuid: &str) -> String` | 132 | YES | 1 test (test_restore_s3_uuid_path_derivation) |
| `is_attach_warning` | `fn(e: &anyhow::Error) -> bool` | 915 | YES | NONE |
| `hardlink_or_copy_dir` | `pub(crate) fn(src: &Path, dst: &Path) -> Result<()>` | 927 | YES - uses tempdir | 1 test |
| `detect_clickhouse_ownership` | `pub fn(data_path: &Path) -> Result<(Option<u32>, Option<u32>)>` | 1024 | YES - uses tempdir | 1 test |
| `get_table_data_path` | `pub fn(data_paths: &[String], data_path_config: &str, db: &str, table: &str) -> PathBuf` | 1058 | YES | 3 tests |

### src/upload/mod.rs -- Upload helpers (39.19% coverage)

| Function | Verified Signature | Line | Testable | Existing Tests |
|----------|-------------------|------|----------|----------------|
| `should_use_multipart` | `fn(compressed_size: u64) -> bool` | 59 | YES | 1 test |
| `s3_key_for_part` | `fn(backup_name: &str, db: &str, table: &str, part_name: &str, data_format: &str) -> String` | 67 | YES | 3 tests |

### src/upload/stream.rs -- Compression helpers (already well tested)

| Function | Verified Signature | Line | Testable | Existing Tests |
|----------|-------------------|------|----------|----------------|
| `archive_extension` | `pub fn(data_format: &str) -> &str` | 28 | YES | 1 test |
| `compress_part` | `pub fn(part_dir: &Path, archive_name: &str, data_format: &str, compression_level: u32) -> Result<Vec<u8>>` | 51 | YES | Multiple roundtrip tests |

### src/restore/rbac.rs -- RBAC helpers (tested already)

| Function | Verified Signature | Line | Testable | Existing Tests |
|----------|-------------------|------|----------|----------------|
| `make_drop_ddl` | `fn(entity_type: &str, name: &str) -> Option<String>` | 339 | YES | 6 tests |

## Phase 1.5: Type Information (LSP hover verified)

### ColumnInconsistency (src/clickhouse/client.rs:79)
```rust
#[derive(Debug, Clone)]
pub struct ColumnInconsistency {
    pub database: String,
    pub table: String,
    pub column: String,
    pub types: Vec<String>,
}
```
Used by: `filter_benign_type_drift()` in `backup/mod.rs` (already has tests using this struct).

### cli::Command (src/cli.rs:50)
Clap-derived `#[derive(Subcommand, Debug)]` enum. See "CLI Command Variant Constructors" section above for full field listings.

### cli::Location and cli::ListFormat (src/cli.rs)
```rust
#[derive(Debug, Clone, ValueEnum)]
pub enum Location { Local, Remote }

#[derive(Debug, Clone, ValueEnum, Default)]
pub enum ListFormat { #[default] Default, Json, Yaml, Csv, Tsv }
```

### list::Location and list::ListFormat (src/list.rs)
```rust
pub enum Location { Local, Remote }
pub enum ListFormat { Default, Json, Yaml, Csv, Tsv }
```

### validate_backup_name (src/server/state.rs:416)
```rust
pub fn validate_backup_name(name: &str) -> Result<(), &'static str>
```
Already has 12+ tests in state.rs. Used by main.rs functions.

### generate_backup_name (src/lib.rs:25)
```rust
pub fn generate_backup_name() -> String
```
Generates `YYYY-MM-DDTHHMMSS.mmm` format name from UTC time.

## Key Reference Chains for Untested Functions

### backup_name_from_command callers
- `main.rs:116` -- early validation before config loading (validates name if present)
- This is a `match` on `Command` variants, extracting `backup_name` field

### resolve_backup_name callers
- `main.rs` -- used in Create, CreateRemote command dispatch (generates name if None)

### backup_name_required callers
- `main.rs` -- used in Upload, Download, Restore, RestoreRemote command dispatch

### sanitize_relative_path callers
- `download/mod.rs` -- used in metadata-only download loop for S3 disk parts
- Security-critical: prevents path traversal via crafted S3 metadata file names

### is_attach_warning callers
- `restore/attach.rs:~860-870` -- in attach_parts_inner loop, decides warn vs error for ATTACH PART failures

## Revised Untested Pure Functions Summary (Priority Order)

1. **sanitize_relative_path** (download/mod.rs:44) -- SECURITY-CRITICAL, 0 tests
2. **is_attach_warning** (restore/attach.rs:915) -- 0 tests, error handling logic
3. **normalize_uuid** (backup/mod.rs:43) -- 0 tests, simple but important
4. **backup_name_from_command** (main.rs:24) -- 0 tests, complex enum matching
5. **resolve_backup_name** (main.rs:726) -- 0 tests, input validation + reserved name check
6. **backup_name_required** (main.rs:744) -- 0 tests, input validation
7. **map_cli_location** (main.rs:756) -- 0 tests, trivial mapping
8. **map_cli_list_format** (main.rs:764) -- 0 tests, trivial mapping
9. **merge_skip_projections** (main.rs:808) -- 0 tests, string processing
10. **is_benign_type** (backup/mod.rs:890) -- only tested indirectly, deserves direct tests

**Removed from list:** `is_ignorable_freeze_error` (already has comprehensive test at line 1035).

## Existing Test Pattern (codebase-wide)

All test modules follow the inline pattern:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    // imports as needed

    #[test]
    fn test_descriptive_name() {
        // arrange
        // act
        // assert
    }
}
```

- 54 test modules across the codebase
- 980 `#[test]` functions
- Uses `tempfile::tempdir()` for filesystem tests
- Uses `anyhow::Error` construction for error tests (e.g., `anyhow::anyhow!("msg")`)
- No mocking framework; async functions requiring ChClient/S3Client are untestable
- Existing backup/mod.rs tests use `ColumnInconsistency` struct construction directly
- Existing restore/attach.rs tests use `tempfile` for filesystem operations
- main.rs has NO test module yet -- needs `#[cfg(test)] mod tests { ... }` at end of file

## CI Coverage Gate Analysis

Current configuration (ci.yml:71):
```python
python3 -c "assert float('${LINE_PCT}') >= 35, f'Coverage ${LINE_PCT}% < 35%'"
```

- Current gate: 35%
- Current actual coverage: 66.68%
- Recommended new gate: 55% (provides ~12% headroom, meaningful quality signal)
- The gate change is a single-line edit in `.github/workflows/ci.yml`
