# Pattern Discovery

## Global Pattern Registry

No `docs/patterns/` directory exists in this project. All patterns discovered locally.

## Component Identification

### Components This Plan Modifies/Adds

| Component | Type | Location | Action |
|-----------|------|----------|--------|
| `restore()` function | Entry point | `src/restore/mod.rs:57` | MODIFY - add remap params |
| `create_databases()` | Function | `src/restore/schema.rs:17` | MODIFY - remap database names |
| `create_tables()` | Function | `src/restore/schema.rs:68` | MODIFY - rewrite DDL for remap |
| DDL rewriting | New module | `src/restore/remap.rs` (NEW) | CREATE - DDL rewriting logic |
| `Command::Restore` dispatch | CLI match arm | `src/main.rs:218` | MODIFY - pass remap params |
| `Command::RestoreRemote` dispatch | CLI match arm | `src/main.rs:340` | MODIFY - implement download+restore |
| `RestoreRemoteRequest` | Struct | `src/server/routes.rs:821` | MODIFY - add remap fields |
| `restore_remote` server route | Handler | `src/server/routes.rs:730` | MODIFY - pass remap params |

### Components This Plan Does NOT Modify

| Component | Reason |
|-----------|--------|
| `BackupManifest` / `TableManifest` | Remap is a restore-time transformation. Manifest is read-only during restore. |
| `OwnedAttachParams` | Remap changes the db/table names before they reach OwnedAttachParams. The struct already has `db` and `table` fields that will receive remapped values. |
| `backup/` module | Remap is restore-only |
| `upload/` module | Remap is restore-only |
| `download/` module | No changes needed -- download fetches backup as-is |
| `cli.rs` | CLI flags `--as` and `-m` already defined and parsed. No structural changes needed. |

## Reference Implementation: `create_remote` (Compound Command)

The `create_remote` command is the reference pattern for compound commands (chains two operations):

```
// main.rs lines 277-338
Command::CreateRemote { ... } => {
    let name = resolve_backup_name(backup_name);
    let ch = ChClient::new(&config.clickhouse)?;
    let s3 = S3Client::new(&config.s3).await?;

    // Step 1: Create local backup
    let _manifest = backup::create(&config, &ch, &name, ...).await?;

    // Step 2: Upload to S3
    let backup_dir = PathBuf::from(&config.clickhouse.data_path).join("backup").join(&name);
    upload::upload(&config, &s3, &name, &backup_dir, ...).await?;
}
```

`restore_remote` should follow the same pattern: `download() -> restore()`.

## Reference Implementation: Server Route `restore_remote`

The server route at `src/server/routes.rs:730` already chains download + restore:

```rust
// Step 1: Download
let download_result = download::download(&config, &s3, &name, effective_resume).await;
// Step 2: Restore (only if download succeeds)
let restore_result = restore::restore(&config, &ch, &name, tables, schema, data_only, resume).await;
```

This needs to be extended with remap parameters.

## Reference Implementation: DDL Modification (`ensure_if_not_exists_table`)

The `schema.rs` file already has a pattern for DDL string manipulation:

```rust
fn ensure_if_not_exists_table(ddl: &str) -> String {
    if ddl.contains("IF NOT EXISTS") { return ddl.to_string(); }
    let ddl = ddl.replacen("CREATE TABLE", "CREATE TABLE IF NOT EXISTS", 1);
    // ... etc
    ddl
}
```

DDL rewriting for remap will follow a similar string-manipulation approach but needs more sophisticated parsing for:
1. Table name extraction and replacement
2. UUID removal (let ClickHouse assign new)
3. ZooKeeper path rewriting in ReplicatedMergeTree engine params
4. Distributed table underlying table reference update

## Pattern: Restore Function Signature Extension

Current signature:
```rust
pub async fn restore(
    config: &Config, ch: &ChClient, backup_name: &str,
    table_pattern: Option<&str>, schema_only: bool, data_only: bool, resume: bool,
) -> Result<()>
```

Extension pattern (add optional remap params):
```rust
pub async fn restore(
    config: &Config, ch: &ChClient, backup_name: &str,
    table_pattern: Option<&str>, schema_only: bool, data_only: bool, resume: bool,
    rename_as: Option<&str>,
    database_mapping: Option<&HashMap<String, String>>,
) -> Result<()>
```

This follows the existing pattern of adding optional parameters with `Option<>` types.

## Pattern: Manifest Table Key Mapping

The manifest uses `"db.table"` as HashMap keys in `manifest.tables`. Remap needs to:
1. Iterate `manifest.tables` with original keys
2. Build a mapping: `original_key -> (new_db, new_table, rewritten_ddl)`
3. Use the new db/table names in `create_tables()` and when building `OwnedAttachParams`

The `table_filter` already parses `"db.table"` keys with `split_once('.')`.
