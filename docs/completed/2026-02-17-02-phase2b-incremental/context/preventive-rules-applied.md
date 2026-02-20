# Preventive Rules Applied -- Phase 2b Incremental Backups

## Rules Checked

| Rule ID | Title | Applicable? | How Applied |
|---|---|---|---|
| RC-006 | Plan code snippets use unverified APIs | YES | All function signatures verified against actual source. See symbols.md. |
| RC-007 | Tuple/struct field order assumed | YES | PartInfo fields verified via manifest.rs. No tuple types used. |
| RC-008 | TDD task sequencing | YES | Each task's dependencies verified against preceding tasks or existing codebase. |
| RC-015 | Cross-task return type mismatch | YES | `create()` returns `Result<BackupManifest>`, `upload()` takes `BackupManifest` from file. Data flow verified. |
| RC-016 | Struct field completeness | YES | No new structs proposed. Existing `PartInfo` already has `source`, `backup_key`, `checksum_crc64` fields needed for diff-from. |
| RC-017 | State field declaration missing | N/A | No actor state fields. Pure function-based architecture. |
| RC-018 | TDD task missing test steps | YES | Each implementation task must include explicit test function names and assertions. |
| RC-019 | Existing pattern not followed | YES | All new code follows existing patterns (see patterns.md). `create_remote` composes `create()` + `upload()`. |
| RC-021 | Struct location assumed | YES | All struct locations verified: `PartInfo` in src/manifest.rs:116, `BackupManifest` in src/manifest.rs:18, `Config` in src/config.rs:8. |
| RC-032 | Data source authority not verified | YES | CRC64 comparison uses data already available in PartInfo.checksum_crc64. No new tracking fields needed. See data-authority.md. |
| RC-001 | Actor dependency wiring | N/A | No actors in this codebase. |
| RC-002 | Schema/type mismatch | YES | All types verified via source. PartInfo.source is String (not enum). |
| RC-004 | Message handler without sender | N/A | No message passing. |
| RC-005 | Zero division | N/A | No division operations in this feature. |
| RC-011 | State machine missing exit paths | N/A | No state machines. |

## Key Findings

1. **PartInfo already has all needed fields**: `source`, `backup_key`, `checksum_crc64` -- no manifest schema changes needed.
2. **CLI flags already defined**: `--diff-from` on Create, `--diff-from-remote` on Upload and CreateRemote -- all in cli.rs.
3. **Manifest loading from S3 already implemented**: `S3Client::get_object()` + `BackupManifest::from_json_bytes()`.
4. **No new config params needed**: The feature uses existing CLI flags, not config params.
5. **create_remote command already has a CLI variant**: `Command::CreateRemote` is defined in cli.rs:168. Currently just logs "not implemented".
