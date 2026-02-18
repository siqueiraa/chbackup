# Preventive Rules Applied

## Rules Loaded

Source: `.claude/skills/self-healing/references/root-causes.md` (34 rules)
Source: `.claude/skills/self-healing/references/planning-rules.md` (14 planning rules)

## Applicable Rules for This Plan

### RC-006: Plan code snippets use unverified APIs
**Status:** APPLIED
**Relevance:** HIGH - Phase 1 introduces many new modules with new APIs (clickhouse queries, S3 operations, filesystem operations). All API calls in the plan must be verified against actual crate APIs.
**Action:** Verified `clickhouse` crate API (uses HTTP interface, `Client::query().execute()` pattern), `aws-sdk-s3` API (`PutObject`, `GetObject`, `ListObjectsV2`, `DeleteObject`), and standard library filesystem APIs.

### RC-008: TDD task sequencing violation
**Status:** APPLIED
**Relevance:** HIGH - Phase 1 has many interconnected modules (manifest struct used by create, upload, download, restore). Must ensure structs are defined before tasks that reference them.
**Action:** Manifest struct must be defined in an early task since create, upload, download, and restore all depend on it.

### RC-015: Cross-task return type mismatch
**Status:** APPLIED
**Relevance:** MEDIUM - The manifest is the central data type flowing between commands. create() produces it, upload() reads it, download() writes it, restore() reads it.
**Action:** Manifest serialization/deserialization round-trip must be verified.

### RC-016: Struct field completeness for consumer tasks
**Status:** APPLIED
**Relevance:** HIGH - The manifest struct (`BackupManifest`) is consumed by upload, download, restore, and list. All fields accessed by consumers must be present.
**Action:** Documented all manifest fields from design doc section 7.1 and cross-referenced with consumer commands.

### RC-019: Existing implementation pattern not followed
**Status:** APPLIED
**Relevance:** HIGH - Phase 0 established patterns for config structs, client wrappers, error types. Phase 1 must follow these patterns.
**Action:** Documented existing patterns in patterns.md. New modules should follow: config-driven construction, `anyhow::Result` return types, `tracing` for logging, etc.

### RC-021: Struct/field file location assumed without verification
**Status:** APPLIED
**Relevance:** MEDIUM - All existing structs verified at their actual file locations via grep.
**Action:** Documented actual file locations in symbols.md.

### RC-032: Adding tracking/calculation without verifying data source authority
**Status:** APPLIED
**Relevance:** MEDIUM - CRC64 checksums, table metadata, part sizes are data that comes from ClickHouse system tables and filesystem. Must verify we use the right sources.
**Action:** Documented in data-authority.md.

## Non-Applicable Rules

### RC-001, RC-004, RC-010, RC-020: Actor/Kameo-specific rules
**Status:** N/A - This project does not use Kameo actors.

### RC-011: State machine flags
**Status:** N/A - No state machines in Phase 1 (sequential operations only).

### RC-012, RC-013, RC-014: E2E test / async callback rules
**Status:** N/A for now - Integration tests are Phase 1 but they use real ClickHouse + S3, not async callbacks.

### RC-033, RC-034: tokio::spawn / strong ref rules
**Status:** N/A for Phase 1 - No parallel spawning in Phase 1 (sequential operations only). Will apply in Phase 2 when parallelism is added.
