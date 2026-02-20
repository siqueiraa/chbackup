# Type Verification Table

## Types Used in This Plan

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `config.backup.compression` | `String` | `String` | config.rs:336 - `pub compression: String` |
| `config.backup.compression_level` | `u32` | `u32` | config.rs:339 - `pub compression_level: u32` |
| `manifest.data_format` | `String` | `String` | manifest.rs:40 - `pub data_format: String` |
| `BackupSummary.size` | `u64` | `u64` | list.rs:34 - `pub size: u64` |
| `BackupSummary.compressed_size` | `u64` | `u64` | list.rs:36 - `pub compressed_size: u64` |
| `BackupSummary.table_count` | `usize` | `usize` | list.rs:38 - `pub table_count: usize` |
| `BackupSummary.is_broken` | `bool` | `bool` | list.rs:40 - `pub is_broken: bool` |
| `TableRow.database` | `String` | `String` | client.rs:25 (approx) |
| `TableRow.name` | `String` | `String` | client.rs:26 (approx) |
| `TableRow.engine` | `String` | `String` | client.rs:27 (approx) |
| `TableRow.total_bytes` | `u64` | `u64` | client.rs:33 (approx) |
| `TableManifest.ddl` | `String` | `String` | manifest.rs:88 |
| `TableManifest.engine` | `String` | `String` | manifest.rs:96 |
| `TableManifest.total_bytes` | `u64` | `u64` | manifest.rs:100 |
| `TableManifest.parts` | `HashMap<String, Vec<PartInfo>>` | `HashMap<String, Vec<PartInfo>>` | manifest.rs:104 |
| `TableManifest.metadata_only` | `bool` | `bool` | manifest.rs:112 |
| `ColumnInconsistency.database` | `String` | `String` | client.rs:68 (approx) |
| `ColumnInconsistency.table` | `String` | `String` | client.rs:69 (approx) |
| `ColumnInconsistency.column` | `String` | `String` | client.rs:70 (approx) |
| `ColumnInconsistency.types` | `Vec<String>` | `Vec<String>` | client.rs:71 (approx) |
| `PartInfo.name` | `String` | `String` | manifest.rs:123 |
| `PartInfo.size` | `u64` | `u64` | manifest.rs:127 |
| `compress_part` return | `Result<Vec<u8>>` | `Result<Vec<u8>>` | upload/stream.rs:16, download/stream.rs:36 |
| `decompress_part` signature | `(data: &[u8], output_dir: &Path) -> Result<()>` | `(data: &[u8], output_dir: &Path) -> Result<()>` | download/stream.rs:16 |
| `list_tables` return | `Result<Vec<TableRow>>` | `Result<Vec<TableRow>>` | client.rs:276 |
| `TableFilter` | struct | struct with `patterns: Vec<Pattern>` | table_filter.rs:18-20 |

## Anti-Pattern Checks

- No `.as_str()` on enums (all compression-related types are String)
- No implicit String-to-Enum conversions
- `data_format` is String everywhere (config, manifest, runtime)
- `compression_level` is `u32` not `i32` or `u8`
