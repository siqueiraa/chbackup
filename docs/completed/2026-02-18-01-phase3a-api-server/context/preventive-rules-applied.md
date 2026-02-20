# Preventive Rules Applied

## Rules Loaded From
- `.claude/skills/self-healing/references/root-causes.md` (34 rules, 32 approved, 2 proposed)
- `.claude/skills/self-healing/references/planning-rules.md` (14 rules)

## Rules Applicable to This Plan

### RC-006: Plan code snippets use unverified APIs
**Applicability:** HIGH -- Phase 3a introduces a new `server/` module with axum. All axum API calls, state management, and middleware patterns must be verified against actual crate docs, not assumed.
**Action:** Verify all axum types (Router, State, Extension, middleware) exist in axum 0.7. Verify tower-http layer types. Do not assume API signatures.

### RC-008: TDD task sequencing violation
**Applicability:** HIGH -- Server module has many interdependent pieces (routes, actions, state, auth). Tasks must be sequenced so types defined in earlier tasks are available to later tasks.
**Action:** Ensure AppState and ActionLog are defined before route handlers that use them.

### RC-011: State machine flags missing exit path transitions
**Applicability:** MEDIUM -- The action log tracks operation status (running/complete/error). Must ensure all exit paths update action status.
**Action:** Ensure every spawned operation updates ActionLog on success, error, and cancellation (kill).

### RC-015: Cross-task return type mismatch
**Applicability:** MEDIUM -- API handlers must return consistent JSON response types. Ensure serialization types match between list/actions endpoints and the Go tool's expected schema.
**Action:** Document JSON response types explicitly.

### RC-016: Struct field completeness for consumer tasks
**Applicability:** HIGH -- AppState shared across all handlers must have all fields needed by every endpoint.
**Action:** List all AppState fields before writing handlers.

### RC-017: State field declaration missing
**Applicability:** HIGH -- Server state (config, clients, action log, cancellation tokens) must be explicitly declared.
**Action:** Define AppState struct with all fields in the foundational task.

### RC-019: Existing implementation pattern not followed
**Applicability:** HIGH -- API endpoints delegate to existing `backup::create`, `upload::upload`, etc. Must use exact same function signatures and parameter patterns as CLI `main.rs`.
**Action:** Copy delegation pattern directly from main.rs match arms.

### RC-021: Struct/field file location assumed without verification
**Applicability:** MEDIUM -- New module `src/server/`. All existing types are verified in symbols.md.
**Action:** Verify all imported types' actual locations with grep.

### RC-032: Adding tracking/calculation without verifying data source authority
**Applicability:** MEDIUM -- Action log tracking operation status and duration. This is new data the API must provide (not available from existing sources).
**Action:** Document in data-authority.md that action log is new (no existing source provides it).

## Rules Not Applicable

### RC-001, RC-004, RC-010, RC-020: Kameo actor rules
**Reason:** chbackup does not use Kameo actors. This is a pure async Rust project with tokio.

### RC-002: Schema/type mismatch trusting comments
**Reason:** No financial data or complex type hierarchies. Config types are straightforward serde structs.

### RC-005: Zero/null division
**Reason:** No division operations in API server code.

### RC-012, RC-013, RC-014: E2E test rules
**Reason:** Phase 3a unit tests and integration tests do not involve actor callbacks or shared mutable state.

### RC-033, RC-034: tokio::spawn reference management
**Reason:** While the server spawns background tasks, these are simple fire-and-forget operations with CancellationToken, not persistent forwarding loops with shared state. Standard pattern.
