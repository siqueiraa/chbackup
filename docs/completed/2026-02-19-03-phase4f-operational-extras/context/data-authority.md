# Data Authority Analysis

## Data Requirements and Sources

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Table list (live) | ChClient::list_tables() | Vec<TableRow> with database, name, engine, total_bytes | USE EXISTING |
| Table list (remote backup) | BackupManifest.tables | HashMap<String, TableManifest> with ddl, engine, total_bytes | USE EXISTING |
| Table filter pattern | TableFilter::new(pattern) | matches(db, table) -> bool | USE EXISTING |
| Backup size (uncompressed) | BackupSummary.size | u64 | USE EXISTING |
| Backup size (compressed) | BackupSummary.compressed_size | u64 | USE EXISTING |
| Table count | BackupSummary.table_count | usize | USE EXISTING |
| Per-table sizes | TableManifest.total_bytes | u64 | USE EXISTING - already in manifest |
| Per-table part count | TableManifest.parts | HashMap<String, Vec<PartInfo>> | USE EXISTING - count from parts map |
| Compression format | BackupConfig.compression | String "lz4|zstd|gzip|none" | USE EXISTING - config field exists |
| Compression level | BackupConfig.compression_level | u32 | USE EXISTING |
| Data format in manifest | BackupManifest.data_format | String | USE EXISTING |
| JSON/Object column types | system.columns | NEW QUERY | MUST IMPLEMENT - no existing query covers column type detection |
| Column type info | system.columns table | type column | MUST IMPLEMENT - need new ChClient method to query system.columns for JSON/Object types |

## Analysis Notes

- The `tables` command needs NO new data sources -- all data is available from existing `list_tables()` and remote manifest parsing
- Per-table size info for `list` enhancement is already in the manifest (`TableManifest.total_bytes`) -- just needs display formatting
- Compression format support needs NO new config fields -- `compression` and `compression_level` already exist with validation
- JSON/Object detection is the ONLY feature requiring a new data source (system.columns query)
- The existing `check_parts_columns()` queries `system.parts_columns` (column consistency) -- the new JSON detection queries `system.columns` (column types). These are different tables with different purposes. Cannot reuse.

## MUST IMPLEMENT Justification

1. **JSON/Object column detection query**: No existing ChClient method queries `system.columns` for column type information. The `check_parts_columns()` method queries `system.parts_columns` for type consistency across parts (different purpose). A new method `check_json_columns()` is needed to query `system.columns WHERE type LIKE '%Object%' OR type LIKE '%JSON%'`.
