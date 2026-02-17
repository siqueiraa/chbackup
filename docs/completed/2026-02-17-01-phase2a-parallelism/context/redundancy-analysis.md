# Redundancy Analysis

## New Public Components Proposed

Phase 2a will introduce the following new public functionality:

| Proposed | Existing Match | Decision | Justification |
|----------|----------------|----------|---------------|
| `S3Client::create_multipart_upload()` | None | N/A -- new API | S3 multipart upload does not exist in current S3Client |
| `S3Client::upload_part()` | None | N/A -- new API | Required for multipart |
| `S3Client::complete_multipart_upload()` | None | N/A -- new API | Required for multipart |
| `S3Client::abort_multipart_upload()` | None | N/A -- new API | Required for cleanup on failure |
| Rate limiter (token bucket) | None | N/A -- new module | No rate limiting exists anywhere in codebase |
| `url_encode` / `url_encode_component` duplication | `upload::url_encode_component`, `download::url_encode`, `restore::attach::url_encode`, `backup::collect::url_encode_path` | COEXIST (pre-existing) | Four separate copies already exist with slightly different behavior (some preserve `/`, some don't). Cleanup is out of scope for Phase 2a but should be addressed in a future refactoring plan. |

## Existing Functions Modified (Not New)

| Function | Change | Decision |
|----------|--------|----------|
| `backup::create()` | Refactor sequential FREEZE loop to parallel | EXTEND |
| `upload::upload()` | Refactor sequential upload loop to parallel with semaphore | EXTEND |
| `download::download()` | Refactor sequential download loop to parallel with semaphore | EXTEND |
| `restore::restore()` | Refactor sequential table loop to parallel with semaphore | EXTEND |
| `restore::attach::attach_parts()` | Add parallel ATTACH for non-Replacing engines | EXTEND |

## Pre-Existing Duplication Note

The `url_encode` function exists in 4 different files with slight behavioral variations:
- `backup/collect.rs:url_encode_path` -- preserves `/`, `-`, `_`, `.`
- `upload/mod.rs:url_encode_component` -- does NOT preserve `/`
- `download/mod.rs:url_encode` -- preserves `/`
- `restore/attach.rs:url_encode` -- preserves `/`

This is pre-existing technical debt, not introduced by Phase 2a. Cleanup deadline: future refactoring plan targeting module utilities.
