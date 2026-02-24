# Handoff: Fix 7 Correctness Issues from Security/Quality Audit

## Plan Location
`docs/plans/2026-02-23-01-correctness-fixes/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | 8 task definitions with TDD steps for 7 correctness issues |
| SESSION.md | Status tracking and phase checklists |
| acceptance.json | 8 criteria (F001-F007, FDOC) with 4-layer verification |
| HANDOFF.md | This file - resume context |
| context/patterns.md | 5 discovered patterns (url_encode DRY, config override, etc.) |
| context/symbols.md | Type verification table for all 7 issues |
| context/knowledge_graph.json | 25+ verified symbols with import paths |
| context/affected-modules.json | 7 affected modules (5 update, 1 modify, 1 create) |
| context/affected-modules.md | Human-readable module summary |
| context/diagnostics.md | Compiler baseline (zero errors/warnings) |
| context/redundancy-analysis.md | 4 url_encode functions -> 1 canonical replacement |
| context/references.md | Call site analysis for all modified symbols |
| context/git-history.md | Recent commit history for affected files |
| context/preventive-rules-applied.md | 7 applicable rules with actions taken |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Created
- `src/path_encoding.rs` - Canonical path component encoder with sanitization

### Files Being Modified
- `src/storage/s3.rs` - Issues 2 (disable_cert_verification), 3 (hermetic tests), 4 (disable_ssl)
- `src/backup/mod.rs` - Issue 5 (check_parts_columns strict-fail)
- `src/config.rs` - Issue 6 (--env env-style keys)
- `src/backup/collect.rs` - Issue 7 (replace url_encode_path)
- `src/download/mod.rs` - Issues 1+7 (path traversal + replace url_encode)
- `src/upload/mod.rs` - Issue 7 (replace url_encode_component)
- `src/restore/attach.rs` - Issue 7 (replace url_encode)
- `src/restore/mod.rs` - Issue 7 (update url_encode import)
- `src/lib.rs` - Add `pub mod path_encoding;`

### Test Files
- `src/path_encoding.rs` (inline #[cfg(test)] module) - 6 unit tests
- `src/storage/s3.rs` (test section) - hermetic mock, 3 #[ignore] markers
- `src/backup/mod.rs` (test section) - check_parts_columns strict-fail test
- `src/config.rs` (test section) - env_key_to_dot_notation tests

### Related Documentation
- `docs/design.md` - Section 3.3 (parts column check), Section 12 (config params)
- `context/patterns.md` - URL encoding patterns and config override pattern

## Issue-to-Task Mapping

| Issue | Priority | Task(s) | Description |
|-------|----------|---------|-------------|
| 1 | P1 | 1, 7 | Path traversal sanitization via new path_encoding module |
| 2 | P1 | 2 | Fix disable_cert_verification (HTTP fallback) |
| 3 | P2 | 3 | Hermetic S3 unit tests (offline-safe sync tests) |
| 4 | P2 | 4 | Wire disable_ssl into S3Client construction |
| 5 | P2 | 5 | check_parts_columns strict-fail (error instead of warn) |
| 6 | P3 | 6 | --env supports env-style keys (S3_BUCKET=val) |
| 7 | P3 | 1, 7 | DRY: canonical url_encode module |

## Commit Log

| Task | Commit | Description |
|------|--------|-------------|
| 3 | cbf1a3d2 | Hermetic S3 unit tests |
| 1, 4, 5, 6 | 9ee9b616 | path_encoding module, disable_ssl wiring, strict check_parts_columns, env-style keys |
| 7 | 17b6f855 | Replace all 4 url_encode implementations with path_encoding module |
| 2 | a1c8be25 | disable_cert_verification forces HTTP endpoint (remove broken CA_BUNDLE approach) |
| 8 | cedd2681 | Update CLAUDE.md for path_encoding, disable_ssl/cert_verification, check_parts_columns, env-style --env |

## AWS SDK Limitation (Important)

The AWS SDK for Rust (aws-smithy-http-client v1.1.10) does NOT expose a public API to disable TLS certificate verification. The `TlsContext` API only provides `TrustStore` configuration (root CA certificates), not a "danger mode" to skip verification. Therefore, Issue 2 is resolved by forcing HTTP when `disable_cert_verification=true`, which is the pragmatic approach. Full HTTPS-without-verification would require a future SDK version or vendoring internal APIs.
