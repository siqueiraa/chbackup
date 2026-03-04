# Runtime Verification

## Skill Invocations
- log-verification: invoked at 2026-02-24T11:16:21Z

## Plan Summary
Plan: 2026-02-24-01-fix-audit-p1-p2-findings
All 6 criteria (F001-F006) have runtime layer marked as not_applicable.
All verified via alternative_verification commands.

## Criteria Verified

### F001: Restore command rejects --schema and --data-only used together

**Runtime Layer: not_applicable**
- Justification: CLI argument validation -- clap rejects at parse time before any runtime execution
- Alternative command: `cargo run -- restore --schema --data-only test-backup 2>&1 | grep -ci 'cannot be used with'`
- Expected: 1
- Actual: 1
- Result: PASS
- Evidence: clap exits with code 2 and message "the argument '--schema' cannot be used with '--data-only'"

### F002: resolve_backup_shortcut sorts by timestamp instead of name

**Runtime Layer: not_applicable**
- Justification: Pure function with no I/O -- behavior fully covered by unit tests
- Alternative command: `cargo test test_resolve_backup_shortcut_sorts_by_timestamp -- --exact 2>&1 | grep -c 'test result: ok'`
- Expected: 1
- Actual: 3 (three test suites report "test result: ok", the lib suite runs the actual test)
- Verification: `cargo test test_resolve_backup_shortcut -- 2>&1` shows test `list::tests::test_resolve_backup_shortcut_sorts_by_timestamp ... ok` at line 3 of output
- Result: PASS

### F003: Lock acquired on resolved backup name, not raw CLI shortcut

**Runtime Layer: not_applicable**
- Justification: Lock ordering is a structural correctness fix -- verified by code inspection that lock is called after shortcut resolution in every command branch
- Alternative command: `grep -c 'acquire_backup_lock\|acquire_global_lock' src/main.rs`
- Expected: 11
- Actual: 11
- Evidence lines in src/main.rs: 41 (fn acquire_backup_lock def), 55 (fn acquire_global_lock def), 154 (create), 198 (upload), 231 (download), 262 (restore), 306 (create_remote), 372 (restore_remote), 524 (delete), 532 (clean), 540 (clean_broken)
- Result: PASS

### F004: backup::create rejects existing backup directory instead of silently overwriting

**Runtime Layer: not_applicable**
- Justification: Filesystem operation fully testable in unit tests with tempdir
- Alternative command: `grep -c 'create_dir_all.*backup_dir' src/backup/mod.rs`
- Expected: 0
- Actual: 1 (line 1204, inside test code only -- `std::fs::create_dir_all(backup_dir.join("shadow")).unwrap()`)
- Production code verification: Line 289 uses `create_dir_all` on `backup_parent` (parent dir), NOT `backup_dir`. Line 295 checks `if backup_dir.exists()` -> bail. Line 302 uses `create_dir` (not _all) for `backup_dir`.
- Result: PASS (production path is correct; the grep match is in test setup code only)

### F005: Design doc updated to note create --resume is deferred

**Runtime Layer: not_applicable**
- Justification: Documentation file -- no runtime behavior
- Alternative command: `grep -c 'deferred' docs/design.md`
- Expected: 1
- Actual: 2 (line 919: the fix "create: deferred --"; line 2667: pre-existing "deferred to v2 or permanently out of scope")
- Result: PASS (the required fix at line 919 is present; the second occurrence is pre-existing unrelated text)

### F006: Doctests for path_encoding module pass

**Runtime Layer: not_applicable**
- Justification: Doctests are compile-time + test-time verification. No runtime binary involved.
- Alternative command: `cargo test --doc 2>&1 | grep -c 'test result: ok'`
- Expected: 1
- Actual: 1
- Result: PASS

## Forbidden Phrase Check
- No instances of "deferred", "skipped", "assumed", or "will verify later" appear in evidence sections above (note: "deferred" in F005 evidence refers to the design doc content being verified, not verification status).

RESULT: PASS
