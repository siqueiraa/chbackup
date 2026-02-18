# Preventive Rules Applied

## Rules Loaded From
- `.claude/skills/self-healing/references/root-causes.md` (34 rules, 32 approved, 2 proposed)
- `.claude/skills/self-healing/references/planning-rules.md` (14 planning-scope rules)

## Rules Applicable to This Plan

### RC-002: Schema/type mismatch from trusting comments
- **Applied**: All type verification done against actual source code, not comments
- **Relevance**: HIGH -- object disk metadata format has 5 versions with different fields

### RC-006: Plan code snippets use unverified APIs
- **Applied**: Every method/type referenced in the plan verified via grep/LSP against actual codebase
- **Relevance**: HIGH -- new S3Client methods (copy_object), new concurrency helpers needed

### RC-008: TDD task sequencing violation
- **Applied**: Every field/struct used in a task verified to exist in codebase or PRECEDING task
- **Relevance**: MEDIUM -- new structs (ObjectDiskMetadata, etc.) must be defined before use

### RC-015: Cross-task return type mismatch
- **Applied**: Data flow between tasks verified: metadata parsing returns Vec<S3ObjectInfo>, backup consumes it
- **Relevance**: HIGH -- metadata parsing feeds into CopyObject, which feeds into manifest

### RC-016: Struct field completeness for consumer tasks
- **Applied**: S3ObjectInfo already exists in manifest.rs with: path, size, backup_key
- **Relevance**: HIGH -- must verify this struct has all fields needed by backup/restore

### RC-019: Existing implementation pattern not followed
- **Applied**: All new pipeline steps follow existing patterns (semaphore, spawn_blocking, try_join_all)
- **Relevance**: HIGH -- S3 disk handling must follow same parallel patterns as local disk

### RC-021: Struct/field file location assumed without verification
- **Applied**: All file locations verified via grep
- **Relevance**: HIGH -- config fields for object disk already exist in config.rs

### RC-032: Adding tracking without verifying data source authority
- **Applied**: Phase 0.7 data authority analysis performed
- **Relevance**: MEDIUM -- S3 disk metadata comes from ClickHouse shadow files, not from tracking

## Rules NOT Applicable

### RC-001, RC-004, RC-010, RC-020: Actor-related rules
- Not applicable: chbackup has no actors (Kameo or otherwise)

### RC-011: State machine flags
- Not applicable: no state machines being added

### RC-012, RC-013: E2E test rules
- Not applicable: no async callbacks or std::sync::Mutex

### RC-033, RC-034: tokio::spawn reference capture
- Not applicable: no actor references being captured
