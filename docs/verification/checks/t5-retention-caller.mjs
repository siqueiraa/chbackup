#!/usr/bin/env node
// Proves retention_remote() calls retention_remote_inner() on a LIVE path, and
// that the old unsafe warn-and-continue on manifest error is gone.
// Dead branches (`if false { ... }`) and #[cfg(test)] modules are stripped first,
// so satisfying this by adding unreachable code does not work.
import { loadLive, fnBody, callsFn, fail } from "./rust-scan.mjs";

const src = loadLive("src/list.rs");
const f = fnBody(src, "retention_remote");
if (!f) fail("retention_remote not found in src/list.rs (after stripping dead code)");
if (!callsFn(f.body, "retention_remote_inner")) {
  fail("retention_remote does not call retention_remote_inner on any live path - "
     + "the injected core is dead code, or the call sits inside a stripped dead branch");
}
// The strongest static signal: the old unsafe path must be GONE. While a `continue`
// remains in the manifest-error handler there is a working fail-open path, and a
// correct call elsewhere cannot compensate for it.
//
// BUG THIS REPLACES: an earlier version scanned ONLY retention_remote_inner whenever
// that helper existed (`inner ? inner.body : f.body`). That let the old unsafe path
// survive untouched in the OUTER function while this check still passed. Every
// function on the retention path is now scanned.
const UNSAFE = /(?:fetch|load|get|read)[_a-z]*manifest[\s\S]{0,400}?Err\([\s\S]{0,300}?\bcontinue\s*;/;

const scopes = [["retention_remote", f.body]];
for (const name of ["retention_remote_inner", "gc_collect_referenced_keys", "collect_incremental_bases"]) {
  const h = fnBody(src, name);
  if (h) scopes.push([name, h.body]);
}

for (const [name, body] of scopes) {
  if (UNSAFE.test(body)) {
    fail(`a manifest fetch/parse error path in ${name} still uses \`continue\` - retention must ABORT `
       + `the whole pass (fail closed), not skip the unreadable manifest and keep deleting later `
       + `candidates. A transient S3 error must never silently shrink the protected-key set.`);
  }
}

console.log(`OK: retention_remote delegates to retention_remote_inner on a live path; `
          + `no manifest-error \`continue\` remains in ${scopes.map(s => s[0]).join(", ")}`);
