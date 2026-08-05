#!/usr/bin/env node
// Asserts textual facts only: the specific fail-open log messages that encode
// "manifest unreadable -> skip it and keep deleting" are gone.
//
// These three strings ARE the H6 defect. While any of them exists, a transient S3
// error silently shrinks the protected-key set and GC deletes keys that a surviving
// incremental still needs. Deleting them is a sound static assertion; whether the
// replacement aborts correctly is proved by the named test
// retention_abort_on_manifest_error (which must assert an EMPTY delete log).
//
// NOTE: an earlier version of this check searched for a `continue` near a manifest
// fetch. That was simply the wrong construct — the real fail-open is a `match` whose
// Err arms warn and fall through, with no `continue` at all — so the check passed
// against the unfixed code. Match on the actual messages instead.
import fs from "node:fs";

const fail = msg => { console.error("FAIL: " + msg); process.exit(1); };

let src;
try {
  src = fs.readFileSync("src/list.rs", "utf8");
} catch (e) {
  fail(`cannot read src/list.rs (${e.code || e.message}) - the gate cannot verify anything`);
}

// Exact fail-open messages present in the unfixed code (src/list.rs ~:988, :996, :1054).
const FAIL_OPEN = [
  "gc: failed to parse manifest, skipping",
  "gc: failed to download manifest, skipping",
  "retention_remote: failed to collect referenced keys, skipping backup",
];

const remaining = FAIL_OPEN.filter(s => src.includes(s));
if (remaining.length > 0) {
  fail(`the retention/GC path still fails OPEN. These messages encode "manifest unreadable -> skip it `
     + `and keep deleting", which is exactly finding H6:\n`
     + remaining.map(s => `    - "${s}"`).join("\n")
     + `\n  A manifest fetch/parse failure must ABORT the retention pass. Protection must fail CLOSED: `
     + `a transient S3 error must never remove a backup's keys from the protected set, because GC then `
     + `deletes data a surviving incremental still references.`);
}

// The planner must at least be reachable from the production entry point.
if (!/\bretention_remote_inner\s*\(/.test(src)) {
  fail("retention_remote_inner not found in src/list.rs - the injected, unit-testable core is absent");
}

console.log(`OK: all ${FAIL_OPEN.length} fail-open manifest messages are gone and `
          + `retention_remote_inner exists`);
