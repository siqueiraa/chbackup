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
// The strongest static signal: the old unsafe path must be GONE. While a
// `continue` remains in the manifest-error handler there is a working unsafe path
// that a dead decoy call cannot compensate for.
const inner = fnBody(src, "retention_remote_inner");
const scope = inner ? inner.body : f.body;
if (/(?:fetch|load|get)[_a-z]*manifest[\s\S]{0,400}?Err\([\s\S]{0,300}?\bcontinue\s*;/.test(scope)) {
  fail("a manifest fetch/parse error path still uses `continue` - retention must ABORT the whole "
     + "pass (fail closed), not skip the unreadable manifest and keep deleting later candidates");
}
console.log("OK: retention_remote delegates to retention_remote_inner on a live path; "
          + "no manifest-error `continue` remains");
