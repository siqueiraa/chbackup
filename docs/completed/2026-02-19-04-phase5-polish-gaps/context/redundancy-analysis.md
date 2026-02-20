# Redundancy Analysis

## Proposed New Public Components

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|---------------|----------|-----------------|---------------|
| `routes::tables()` handler | `routes::tables_stub()` (server/routes.rs:1192) | REPLACE | Task 1 / A_TABLES_IMPL | Stub returns 501; real implementation will query CH or manifest |
| `routes::restart()` handler | `routes::restart_stub()` (server/routes.rs:1187) | REPLACE | Task 2 / A_RESTART_IMPL | Stub returns 501; real implementation will re-init connections |
| `TablesParams` query struct | None | N/A | Task 1 | New request type for tables query params |
| `TablesResponse` response struct | None | N/A | Task 1 | New response type (or Vec of a table entry struct) |
| Exit code mapping logic | None | N/A | Task 6 | New mapping in main.rs |
| Projection filter in collect_parts | None | N/A | Task 3 | New filter logic in existing function |
| Hardlink dedup in download | None | N/A | Task 4 | New dedup logic in existing function |
| `BackupSummary.metadata_size` field | None | N/A | Task 7 | Extends existing struct |

## REPLACE Decisions

### `tables_stub` -> `tables` (Task 1)
- **Removal**: `tables_stub` function deleted, route updated to point to new `tables` handler
- **Test migration**: Existing test at routes.rs:1587 that asserts 501 status will be updated to test real functionality
- **Acceptance**: Route `/api/v1/tables` returns 200 with table data

### `restart_stub` -> `restart` (Task 2)
- **Removal**: `restart_stub` function deleted, route updated to point to new `restart` handler
- **Test migration**: Existing test at routes.rs:1584 that asserts 501 status will be updated
- **Acceptance**: Route `/api/v1/restart` returns 200 and re-initializes connections

## Notes

- No COEXIST decisions needed
- All items modify existing code or replace stubs; no risk of duplicating functionality
- The `tables` CLI command (main.rs:384-481) already has full implementation; the API endpoint just needs to mirror it
