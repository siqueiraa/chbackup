#!/usr/bin/env node
// Asserts ONE textual fact: clean_broken_remote consults the deletion planner.
//
// It deliberately does NOT try to prove the deleted set equals the planned set. An
// earlier "exactly one deletion call" rule was wrong: delete_objects batches at 1000
// keys internally, and gc_delete_backup MUST issue two deletions (unreferenced keys,
// then the manifest last) because manifest-last ordering is what makes a crash
// recoverable. The set equality is proved by the named test
// clean_broken_deletes_only_planned. See README.md.
import { loadLive, fnBody, callsFn, fail } from "./rust-scan.mjs";

const src = loadLive("src/list.rs");
const f = fnBody(src, "clean_broken_remote");
if (!f) fail("clean_broken_remote not found in src/list.rs (after stripping dead code)");

if (!callsFn(f.body, "plan_clean_broken_deletion")) {
  fail("clean_broken_remote does not call plan_clean_broken_deletion on any live path - without it "
     + "there is no age threshold, no PID-lock liveness check and no reference check, so the command "
     + "can delete an in-flight upload's data or a base an incremental still needs");
}

console.log("OK: clean_broken_remote consults plan_clean_broken_deletion");
