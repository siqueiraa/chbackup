# References

## Go Source (compared via 8 parallel agents)

| Domain | Go Package | Rust Module | Agent |
|--------|-----------|-------------|-------|
| Config | pkg/config | src/config.rs | a81d04f |
| Backup/Freeze | pkg/backup | src/backup/ | a63e7b8 |
| Restore | pkg/backup (RestoreBackup) | src/restore/ | a8ad821 |
| S3 Storage | pkg/storage/s3 | src/storage/s3.rs | a4fe565 |
| Server/API | pkg/server | src/server/ | a9c6208 |
| CLI | cmd/clickhouse-backup | src/cli.rs, src/main.rs | acab932 |
| Watch | pkg/backup (watch.go) | src/watch/mod.rs | aeed155 |
| List/Delete/Retention | pkg/backup (list/delete) | src/list.rs | a26ec8b |

## Design Doc Sections Referenced

| Section | Content | Tasks |
|---------|---------|-------|
| §3.1 | FREEZE mutations | 6 |
| §5.2 | Restore modes | 7 |
| §8.2 | Remote retention GC | 9 |
| §9 | API specification | 10 |
| §10 | Watch mode | 11 |
| §11.6 | Error codes | (already done Phase 5) |
| §12 | Full config reference | 1, 2, 3, 4, 5 |

## Gap Tier Mapping to Tasks

| Tier | Description | Items | Tasks |
|------|-------------|-------|-------|
| 1 | Broken contracts (config ignored) | 1-12 | 1,2,4,5,6,7,11 |
| 2 | CLI flags not wired | 13-14 | 7 |
| 3 | Critical behavioral gaps | 15-21 | 3,5,6,8,9 |
| 4 | Config default mismatches | 22-27 | 1 |
| 5 | API/Server gaps | 28-33 | 10 |
| 6 | Missing list features | 34-35 | 12 |
