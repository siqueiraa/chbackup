# Preventive Rules Applied

## Rules Checked

| Rule | Applicable | Finding |
|------|-----------|---------|
| RC-001 | No | No actors in chbackup (pure async/sync architecture) |
| RC-002 | Yes | Verified actual types for BackupManifest, BackupSummary, ListResponse, PartInfo via file reads |
| RC-003 | Yes | Will track in SESSION.md during execution |
| RC-004 | No | No actors/messages in chbackup |
| RC-005 | No | No division-heavy calculations in this plan |
| RC-006 | Yes | All API methods verified via grep/read before documenting in symbols.md |
| RC-007 | Yes | No tuple types assumed -- struct fields verified via source reads |
| RC-008 | Yes | Task sequencing must ensure fields exist before use |
| RC-015 | Yes | Cross-task data flows identified (manifest fields flow to list/API) |
| RC-016 | Yes | New manifest fields (rbac_size, config_size) must be populated before consumption |
| RC-017 | Yes | All new fields must be declared in preceding tasks |
| RC-018 | Yes | Each task should have test steps |
| RC-019 | Yes | Follow existing patterns for: S3Client methods, signal handlers, upload pipeline |
| RC-021 | Yes | All struct locations verified via grep (BackupManifest in src/manifest.rs, BackupSummary in src/list.rs, ListResponse in src/server/routes.rs) |
| RC-022 | Yes | Plan file structure will be validated |
| RC-023 | Yes | Phase checklist will be maintained |
| RC-029 | No | No sync-to-async signature changes planned |
| RC-032 | Yes | Data authority verified -- rbac_size/config_size come from filesystem scan, not from exchange APIs |
| RC-035 | Yes | cargo fmt must be run before committing |

## Key Findings

1. **BackupManifest** (src/manifest.rs): Currently has `metadata_size: u64` and `compressed_size: u64` but NO `rbac_size` or `config_size` fields. These need to be added.

2. **BackupSummary** (src/list.rs): Has `metadata_size: u64` but no `rbac_size` or `config_size`. Needs extension.

3. **ListResponse** (src/server/routes.rs): Already has `rbac_size: u64` and `config_size: u64` fields (hardcoded to 0 in `summary_to_list_response`). Only the data source is missing, not the API shape.

4. **Signal handling** (src/server/mod.rs, src/main.rs): Only SIGINT (ctrl_c) and SIGHUP are handled. No SIGQUIT handler exists anywhere in the codebase.

5. **Upload pipeline** (src/upload/mod.rs + stream.rs): Currently buffers entire compressed part into `Vec<u8>` before uploading. Streaming would require piping tar+compress output directly into multipart upload chunks.

6. **Manifest caching**: `list_remote()` downloads every manifest on every call. No caching layer exists. Design doc 8.2 specifies in-memory caching for server/watch mode.

7. **Tables endpoint**: No pagination support. Returns all results in a single response.
