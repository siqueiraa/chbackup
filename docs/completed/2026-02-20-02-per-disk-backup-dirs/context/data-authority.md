# Data Authority Analysis

## Data Requirements

This plan does NOT add tracking or calculations. It changes the directory layout for hardlinked backup parts to avoid EXDEV cross-device copies.

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Disk name for each part | CollectedPart | disk_name (String) | USE EXISTING -- already populated by collect_parts |
| Disk path for each disk | DiskRow / manifest.disks | path (String) / HashMap<String, String> | USE EXISTING -- already available from ch.get_disks() and stored in manifest |
| Disk type (local vs S3) | disk_type_map / manifest.disk_types | HashMap<String, String> | USE EXISTING -- already used to route S3 vs local |
| Which disk a part belongs to at upload time | manifest.tables[*].parts | Keys are disk names | USE EXISTING -- parts already grouped by disk name |
| Which disks to clean at delete time | BackupManifest.disks | HashMap<String, String> | USE EXISTING -- manifest persists the disk mapping |

## Analysis Notes

- All data needed for per-disk backup directories is ALREADY available in the existing codebase
- `collect_parts()` already iterates per-disk, already knows `disk_name` and `disk_path` for each part
- `BackupManifest.disks` already stores `disk_name -> disk_path` mapping, persisted in metadata.json
- `TableManifest.parts` is already keyed by disk name
- No new tracking, accumulators, or calculations needed
- The change is purely about using the existing disk path information to compute staging directories

## Over-Engineering Check

- NOT over-engineering: the data already flows through the system, we just need to USE it for directory computation
- No new fields, no new data sources needed
