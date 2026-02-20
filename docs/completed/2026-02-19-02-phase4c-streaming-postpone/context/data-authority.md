# Data Authority Analysis

This plan adds DDL ordering logic, not data tracking/calculation. Data authority analysis is minimal.

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Engine name | TableManifest | `engine: String` | USE EXISTING - from `system.tables.engine` at backup time |
| DDL text | TableManifest | `ddl: String` | USE EXISTING - from `system.tables.create_table_query` at backup time |
| metadata_only flag | TableManifest | `metadata_only: bool` | USE EXISTING - set during backup by `is_metadata_only_engine()` |
| Streaming engine detection | TableManifest.engine | Engine name matches known set | MUST IMPLEMENT - simple match against known engine names |
| Refreshable MV detection | TableManifest.ddl | DDL contains "REFRESH" keyword | MUST IMPLEMENT - string search in DDL text |

## Analysis Notes

- Engine names come from ClickHouse `system.tables.engine` column, stored in `TableManifest.engine` at backup time. No additional data source needed.
- The DDL text for refreshable MV detection comes from `system.tables.create_table_query`, stored in `TableManifest.ddl` at backup time. No additional data source needed.
- No new data collection, tracking, or calculation is proposed. All detection is based on existing manifest fields.
- The known streaming engine names (`Kafka`, `NATS`, `RabbitMQ`, `S3Queue`) are the complete set per ClickHouse documentation and the design doc.
