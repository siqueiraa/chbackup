# Preventive Rules Applied

## Rules Checked

| Rule ID | Title | Applicable? | How Applied |
|---------|-------|-------------|-------------|
| RC-006 | Plan code snippets use unverified APIs | YES | All methods referenced verified via grep: `execute_ddl`, `database_exists`, `table_exists`, `create_databases`, `create_tables`, `restore()`, `download()`. No assumed APIs. |
| RC-007 | Tuple/struct field order assumed | YES | Verified `TableManifest.ddl`, `TableManifest.uuid`, `DatabaseInfo.name/ddl`, `OwnedAttachParams` field list via Read of source files. |
| RC-008 | TDD task sequencing | YES | Plan tasks will need DDL rewriting module before restore integration. Remap struct must be defined before use. |
| RC-015 | Cross-task return type mismatch | YES | `restore()` returns `Result<()>` and `download()` returns `Result<PathBuf>`. `restore_remote` chains them. Types confirmed. |
| RC-016 | Struct field completeness | YES | `RemapConfig` (new) must contain all fields needed by DDL rewriting and manifest mapping. All consumers identified. |
| RC-017 | State field declaration missing | YES | No new actor state. New struct `RemapConfig` fields all enumerated in discovery. |
| RC-019 | Existing pattern not followed | YES | `restore_remote` CLI dispatch follows `create_remote` pattern (download + restore chaining). Server route already exists with this pattern. |
| RC-021 | Struct/field file location assumed | YES | Verified: `restore()` in `src/restore/mod.rs:57`, `create_tables()` in `src/restore/schema.rs:68`, `OwnedAttachParams` in `src/restore/attach.rs:59`, `BackupManifest` in `src/manifest.rs:19`, CLI in `src/cli.rs`. |
| RC-032 | Data source authority not verified | N/A | No tracking/calculation/accumulator being added. This plan modifies DDL strings and table name mappings. |

## Rules Not Applicable

| Rule ID | Title | Reason |
|---------|-------|--------|
| RC-001 | Actor dependency wiring | No actors in this project |
| RC-002 | Schema/type mismatch from comments | No financial data types |
| RC-004 | Message handler without sender | No actors |
| RC-005 | Zero/null division | No division operations |
| RC-010 | Adapter stub methods | No adapter pattern |
| RC-011 | State machine flags | No state machine flags being added |
| RC-012 | E2E test shared mutable state | No E2E test callbacks |
| RC-013 | std::sync::Mutex in async | No new Mutex usage planned |
| RC-014 | Connection loop without assertion | No connection polling |
| RC-020 | Kameo message type derives | No Kameo actors |
