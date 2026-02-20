# Pattern Discovery

## Global Patterns Registry

No `docs/patterns/` directory exists. Patterns discovered locally from codebase analysis.

## Component Identification

### Components Modified by This Plan

| Component | Type | Location |
|-----------|------|----------|
| compress_part (upload) | Function | src/upload/stream.rs |
| compress_part (download) | Function | src/download/stream.rs |
| decompress_part | Function | src/download/stream.rs |
| s3_key_for_part | Function | src/upload/mod.rs |
| Tables command dispatch | Function | src/main.rs |
| list_tables | Method | src/clickhouse/client.rs |
| check_parts_columns | Method | src/clickhouse/client.rs |
| print_backup_table | Function | src/list.rs |
| BackupConfig | Struct | src/config.rs |

### Components Reused (Not Modified)

| Component | Type | Location | Usage |
|-----------|------|----------|-------|
| TableFilter | Struct | src/table_filter.rs | Used by tables command for --tables glob |
| BackupManifest | Struct | src/manifest.rs | data_format field already exists |
| BackupSummary | Struct | src/list.rs | Already has size/compressed_size fields |
| format_size | Function | src/list.rs | Already exists for human-readable bytes |
| ChClient | Struct | src/clickhouse/client.rs | Used for ClickHouse queries |
| S3Client | Struct | src/storage/ | Used for remote backup manifest download |

## Pattern: Compression Pipeline

**Reference: upload/stream.rs and download/stream.rs**

Both upload and download have a `compress_part` and `decompress_part` function. Currently hardcoded to LZ4:

```
compress:   tar::Builder -> lz4_flex::FrameEncoder -> Vec<u8>
decompress: lz4_flex::FrameDecoder -> tar::Archive -> unpack()
```

Pattern for multi-format support:
1. Accept `data_format: &str` parameter
2. Match on format to select encoder/decoder
3. For "none": tar only (no compression layer)
4. For "lz4": existing behavior
5. For "zstd": zstd::Encoder / zstd::Decoder
6. For "gzip": flate2::write::GzEncoder / flate2::read::GzDecoder

## Pattern: S3 Key Extension

**Reference: upload/mod.rs:68-76**

Current: hardcoded `.tar.lz4`
New pattern: extension derived from data_format:
- "lz4" -> ".tar.lz4"
- "zstd" -> ".tar.zstd"
- "gzip" -> ".tar.gz"
- "none" -> ".tar"

## Pattern: ChClient Query Method

**Reference: check_parts_columns() at client.rs:604-661**

Pattern for adding new system table queries:
1. Build SQL with IN clause for (database, table) pairs
2. Conditional logging (log_sql_queries flag)
3. Define inner row struct with `#[derive(clickhouse::Row, serde::Deserialize)]`
4. fetch_all + .context()
5. Map rows to domain types

## Pattern: Command Dispatch (main.rs)

**Reference: main.rs:382-384**

Pattern for implementing a new command:
1. Match on `Command::Variant { fields }` in the big match block
2. Create clients as needed (ChClient, S3Client)
3. Call domain function
4. Log completion

The tables command is read-only (no PidLock needed).

## Pattern: CLI-to-Config Flow for Compression

**Reference: config.rs BackupConfig, upload/mod.rs line 223**

1. Config loads `backup.compression` from YAML (default "lz4")
2. Validation in `validate()` checks against allowed values
3. Upload reads `config.backup.compression` and sets `manifest.data_format`
4. Download reads `manifest.data_format` to know which decompressor to use
