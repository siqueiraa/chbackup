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
const afterPlan = f.body.slice(f.body.search(/\bplan_clean_broken_deletion\s*\(/));
const delRe = /\b(delete_objects|delete_remote|delete_backup|gc_delete_backup)\s*\(/g;
let m, consumed = false, sawDelete = false;
while ((m = delRe.exec(afterPlan))) {
  sawDelete = true;
  // Extract the balanced argument list of this call.
  const open = afterPlan.indexOf("(", m.index + m[0].length - 1);
  let depth = 0, end = -1;
  for (let i = open; i < afterPlan.length; i++) {
    if (afterPlan[i] === "(") depth++;
    else if (afterPlan[i] === ")") { depth--; if (depth === 0) { end = i; break; } }
  }
  const args = end > open ? afterPlan.slice(open + 1, end) : "";
  if (new RegExp(`\\b${binding}\\b`).test(args)) { consumed = true; break; }
}
if (!sawDelete) {
  fail(`no deletion call appears after the planner result \`${binding}\` - the plan is computed and ignored`);
}
if (!consumed) {
  fail(`no deletion call passes the planner result \`${binding}\` as an argument - the code deletes a `
     + `different key set than the one that was planned and protected, so unreferenced-key protection is bypassed`);
}

// And nothing may be deleted BEFORE the planner runs.
const beforePlan = f.body.slice(0, f.body.search(/\bplan_clean_broken_deletion\s*\(/));
if (/\b(delete_objects|delete_remote|delete_backup|gc_delete_backup)\s*\(/.test(beforePlan)) {
  fail("clean_broken_remote deletes before consulting plan_clean_broken_deletion");
}
// KNOWN LIMIT (documented deliberately): this proves at least one deletion consumes
// the plan and that nothing is deleted beforehand, but it cannot rule out an ADDITIONAL
// unguarded deletion elsewhere in the function. The authoritative proof of that is the
// required named test clean_broken_deletes_only_planned, which must assert the deleted
// key set EQUALS the planner-returned set. Static analysis is the supplement here, not
// the proof.
console.log("OK: clean_broken_remote deletes from the planner's result, and nothing before it");
