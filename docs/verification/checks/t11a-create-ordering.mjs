#!/usr/bin/env node
// A source-OFFSET comparison is unsound (a helper defined early but called late
// passes it). This instead requires a DATA-FLOW relationship: the completeness
// decision must be bound to a variable, and the value passed to the CREATE phase
// must derive from it - so an incomplete table cannot reach CREATE.
import { loadLive, fnBody, bindingOf, fail } from "./rust-scan.mjs";

const src = loadLive("src/restore/mod.rs");

// Locate the enclosing function that performs the CREATE phase.
let host = null;
for (const name of ["restore", "restore_inner", "run_restore", "restore_from_manifest"]) {
  const f = fnBody(src, name);
  if (f && /\bcreate_tables\s*\(/.test(f.body)) { host = { name, ...f }; break; }
}
if (!host) fail("could not find the function containing the create_tables( call in src/restore/mod.rs");

const completeness = ["resolve_attach_completeness", "attach_completeness", "plan_attach_completeness"]
  .map(n => ({ n, b: bindingOf(host.body, n) })).find(x => x.b);
if (!completeness) {
  fail("no completeness resolution is bound to a variable inside " + host.name + " - the decision must "
     + "be computed and bound BEFORE the CREATE phase so its result can gate CREATE");
}
const { n: fnName, b: binding } = completeness;

const cIdx = host.body.search(/\bcreate_tables\s*\(/);
const pIdx = host.body.search(new RegExp(`\\b${fnName}\\s*\\(`));
if (pIdx < 0 || cIdx < 0) fail("could not locate both the completeness call and create_tables(");
if (pIdx > cIdx) {
  fail(`completeness (${fnName}) is invoked AFTER create_tables in ${host.name}; the table would `
     + `already exist and be queryable, so refusing to attach still leaves a live empty table`);
}
// Data flow: the create_tables call must reference the completeness binding (or a
// value derived from it), otherwise ordering alone proves nothing.
const callArgs = host.body.slice(cIdx, cIdx + 600);
if (!new RegExp(`\\b${binding}\\b`).test(callArgs)) {
  fail(`create_tables does not receive anything derived from the completeness result \`${binding}\` - `
     + `compute the create set from it so incomplete tables are excluded from CREATE`);
}
console.log(`OK: ${fnName} is resolved before create_tables and its result (\`${binding}\`) feeds the CREATE phase`);
