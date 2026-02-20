# Symbol Verification Table

## Types Used in Plan

| Variable/Field | Assumed Type | Actual Type | Verification Source |
|---|---|---|---|
| `RestorePhases` | struct with 3 Vec<String> fields | `struct { data_tables: Vec<String>, postponed_tables: Vec<String>, ddl_only_tables: Vec<String> }` | `src/restore/topo.rs:51-58` |
| `RestorePhases.postponed_tables` | `Vec<String>` | `Vec<String>` | `src/restore/topo.rs:55` -- currently hardcoded to `Vec::new()` at line 89 |
| `BackupManifest.tables` | `HashMap<String, TableManifest>` | `HashMap<String, TableManifest>` | `src/manifest.rs:65` |
| `TableManifest.engine` | `String` | `String` | `src/manifest.rs:95-96` |
| `TableManifest.ddl` | `String` | `String` | `src/manifest.rs:88` |
| `TableManifest.metadata_only` | `bool` | `bool` | `src/manifest.rs:112` |
| `classify_restore_tables` return | `RestorePhases` | `RestorePhases` | `src/restore/topo.rs:61` |
| `classify_restore_tables` params | `(&BackupManifest, &[String])` | `(manifest: &BackupManifest, table_keys: &[String])` | `src/restore/topo.rs:61` |
| `engine_restore_priority` return | `u8` | `u8` | `src/restore/topo.rs:40` |
| `create_tables` params | 5 params | `(ch: &ChClient, manifest: &BackupManifest, table_keys: &[String], data_only: bool, remap: Option<&RemapConfig>)` | `src/restore/schema.rs:113` |
| `create_ddl_objects` params | 4 params | `(ch: &ChClient, manifest: &BackupManifest, ddl_keys: &[String], remap: Option<&RemapConfig>)` | `src/restore/schema.rs:198` |
| `topological_sort` params | 2 params | `(tables: &HashMap<String, TableManifest>, keys: &[String]) -> Result<Vec<String>>` | `src/restore/topo.rs:100` |
| `is_metadata_only_engine` | private fn | `fn(engine: &str) -> bool` (private to backup module) | `src/backup/mod.rs:589` |
| `data_table_priority` | pub fn | `fn(table_key: &str) -> u8` | `src/restore/topo.rs:21` |

## Engine Name Strings (from ClickHouse system.tables)

Literal engine name strings as they appear in `system.tables.engine` and stored in `TableManifest.engine`:

| Engine | metadata_only in backup? | Classification in restore |
|--------|-------------------------|--------------------------|
| `"Kafka"` | Currently not in `is_metadata_only_engine` -- treated as data table | Should be: postponed (Phase 2b) |
| `"NATS"` | Currently not in `is_metadata_only_engine` -- treated as data table | Should be: postponed (Phase 2b) |
| `"RabbitMQ"` | Currently not in `is_metadata_only_engine` -- treated as data table | Should be: postponed (Phase 2b) |
| `"S3Queue"` | Currently not in `is_metadata_only_engine` -- treated as data table | Should be: postponed (Phase 2b) |
| `"MaterializedView"` | YES (in `is_metadata_only_engine`) | Currently: ddl_only. IF has REFRESH clause: should be postponed (Phase 2b) |
| `"MergeTree"` | NO | data table (Phase 2) |
| `"View"` | YES | ddl_only (Phase 3) |
| `"Dictionary"` | YES | ddl_only (Phase 3) |

## DDL Detection Patterns

| Pattern | Detection Method | Example DDL Fragment |
|---------|-----------------|---------------------|
| Streaming engine | `matches!(engine, "Kafka" \| "NATS" \| "RabbitMQ" \| "S3Queue")` | `ENGINE = Kafka()` |
| Refreshable MV | `engine == "MaterializedView"` AND DDL contains ` REFRESH ` (case-insensitive, word boundary) | `CREATE MATERIALIZED VIEW ... REFRESH EVERY 1 HOUR ...` |

## Key Observations

1. **Kafka/NATS/RabbitMQ/S3Queue are NOT in `is_metadata_only_engine()`** in backup. They are treated as data tables (metadata_only=false). However, they typically have no FREEZE-able data. In practice they are usually excluded via `skip_table_engines` config. If they DO appear in a manifest, they currently go to `data_tables`.

2. **In the restore classification**, streaming engines should be moved to `postponed_tables` regardless of their `metadata_only` flag value. The engine name check takes priority.

3. **Refreshable MVs have engine="MaterializedView"** and `metadata_only=true`. They currently go to `ddl_only_tables`. The plan must redirect them to `postponed_tables` when REFRESH clause is detected in the DDL.

4. **The `RestorePhases.postponed_tables` field already exists** at `src/restore/topo.rs:55` and is currently hardcoded to `Vec::new()` at line 89.

5. **The `create_tables()` function** in schema.rs is generic enough to handle postponed tables -- it just needs a slice of table keys. No new schema function is needed.

6. **The insertion point** for Phase 2b in `restore/mod.rs` is between line 432 (after data attachment tally) and line 434 (Phase 3 DDL-only objects). This is AFTER all data tables have their parts attached.
