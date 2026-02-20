# Data Authority Analysis -- Phase 2b Incremental Backups

## Data Requirements

| Data Needed | Source Type | Field Available | Decision |
|---|---|---|---|
| Part identity (name) | PartInfo | `name: String` | USE EXISTING -- part name is already collected |
| Part CRC64 checksum | PartInfo | `checksum_crc64: u64` | USE EXISTING -- computed during `collect_parts()` in backup/collect.rs |
| Part S3 key | PartInfo | `backup_key: String` | USE EXISTING -- set during upload, stored in manifest |
| Part source tracking | PartInfo | `source: String` | USE EXISTING -- values "uploaded" or "carried:{base_name}" |
| Part disk grouping | TableManifest | `parts: HashMap<String, Vec<PartInfo>>` | USE EXISTING -- parts grouped by disk name |
| Base manifest (local) | BackupManifest | `load_from_file(path)` | USE EXISTING -- loads from {data_path}/backup/{name}/metadata.json |
| Base manifest (remote) | S3Client + BackupManifest | `get_object(key)` + `from_json_bytes()` | USE EXISTING -- pattern used in download module |
| Compression format | Config | `backup.compression` | USE EXISTING |

## Analysis Notes

- **Zero new tracking fields needed**: The incremental diff comparison uses `PartInfo.name` + `PartInfo.checksum_crc64` which are already populated during `collect_parts()`.
- **No shadow state**: The manifest IS the source of truth. Carried parts point to the original backup's S3 key. No separate tracking structure needed.
- **CRC64 is computed at collect time**: In `backup/collect.rs:199`, `compute_crc64(&checksums_path)` is called for every part. This gives us the current CRC64 to compare against the base manifest.
- **Source field format**: `"uploaded"` for new parts, `"carried:{base_backup_name}"` for carried parts. This is already defined in the manifest design (src/manifest.rs:128-131).

## MUST IMPLEMENT (Justification Required)

| What | Why |
|---|---|
| `diff_parts()` comparison function | No existing function compares two manifests' parts by name+CRC64. Must iterate base manifest parts and match against current parts. Simple HashMap lookup. |
| `load_base_manifest()` helper | Need a helper that loads from local path or S3 depending on whether `--diff-from` or `--diff-from-remote` is used. Thin wrapper around existing `load_from_file` / `get_object+from_json_bytes`. |
| Upload filtering for carried parts | Upload currently processes ALL parts. Must skip parts where `source.starts_with("carried:")`. Modification to existing `upload()` flow. |
| `create_remote` orchestration | New command that calls `create()` then `upload()`. Composition of existing functions. |
