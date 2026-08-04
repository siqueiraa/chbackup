#!/usr/bin/env node
// Proves clean_broken_remote() consults plan_clean_broken_deletion() AND that the
// deletion is fed by the planner's RESULT (data flow), not merely preceded by a
// call to it. A dead decoy call cannot satisfy the binding check.
import { loadLive, fnBody, callsFn, bindingOf, fail } from "./rust-scan.mjs";

const src = loadLive("src/list.rs");
const f = fnBody(src, "clean_broken_remote");
if (!f) fail("clean_broken_remote not found in src/list.rs (after stripping dead code)");
if (!callsFn(f.body, "plan_clean_broken_deletion")) {
  fail("clean_broken_remote does not call plan_clean_broken_deletion on any live path");
}
const binding = bindingOf(f.body, "plan_clean_broken_deletion");
if (!binding) {
  fail("the plan_clean_broken_deletion result is not bound to a variable, so it cannot be what "
     + "drives the deletion - bind it (`let plan = plan_clean_broken_deletion(..)`) and delete from it");
}
// Inspect the deletion call ARGUMENTS specifically. A neighbourhood window is too
// generous: the `let plan = ...` line sits within a few hundred characters of the
// call and would satisfy a proximity test while the call actually deletes something else.
// Require EXACTLY ONE deletion call in this function, and require it to consume the
// planner binding.
//
// "At least one deletion consumes the plan" was insufficient: an ADDITIONAL unguarded
// `delete_objects(&all_broken_keys)` alongside the planned one still deleted protected
// keys and still passed. Requiring one-and-only-one deletion makes
// deleted-set == planner-set structural rather than a property we hope holds.
const delRe = /\b(delete_objects|delete_remote|delete_backup|gc_delete_backup)\s*\(/g;
const calls = [...f.body.matchAll(delRe)];

if (calls.length === 0) {
  fail(`no deletion call found in clean_broken_remote - the plan is computed and never acted on`);
}
if (calls.length > 1) {
  fail(`clean_broken_remote contains ${calls.length} deletion calls (${calls.map(c => c[1]).join(", ")}); `
     + `exactly one is required so the deleted set is structurally the planner's set. An extra `
     + `unguarded deletion is how protected keys get destroyed. Delete once, from the plan.`);
}

// That single call must receive the planner binding in its balanced argument list.
const call = calls[0];
const open = f.body.indexOf("(", call.index + call[0].length - 1);
let depth = 0, end = -1;
for (let i = open; i < f.body.length; i++) {
  if (f.body[i] === "(") depth++;
  else if (f.body[i] === ")") { depth--; if (depth === 0) { end = i; break; } }
}
const args = end > open ? f.body.slice(open + 1, end) : "";
if (!new RegExp(`\\b${binding}\\b`).test(args)) {
  fail(`the single deletion call does not receive the planner result \`${binding}\` - it would delete a `
     + `different key set than the one that was planned and protected, bypassing reference protection`);
}

// And nothing may be deleted BEFORE the planner runs.
const beforePlan = f.body.slice(0, f.body.search(/\bplan_clean_broken_deletion\s*\(/));
if (/\b(delete_objects|delete_remote|delete_backup|gc_delete_backup)\s*\(/.test(beforePlan)) {
  fail("clean_broken_remote deletes before consulting plan_clean_broken_deletion");
}
// The earlier "additional unguarded deletion" hole is now closed structurally by the
// exactly-one rule above. The required named test clean_broken_deletes_only_planned
// remains the authoritative behavioral proof (deleted key set EQUALS planner set);
// this static check exists to prove the production path consumes the tested planner
// at all, which a unit test on a pure function cannot show.
console.log("OK: clean_broken_remote performs exactly one deletion, fed by the planner's result, "
          + "with nothing deleted beforehand");
