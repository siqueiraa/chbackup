#!/usr/bin/env node
// Asserts ONE textual fact: a completeness resolution exists and appears before the
// CREATE phase in source order.
//
// It does NOT try to prove the create set derives from it. A derivation tracker
// rejected `CompleteAttachPlan::try_new(..)` — the constructor this plan MANDATES.
// The real property (no live incomplete table remains) is proved by the named test
// restore_incomplete_leaves_no_live_table. See README.md.
import { loadLive, fnBody, fail } from "./rust-scan.mjs";

const src = loadLive("src/restore/mod.rs");

let host = null;
for (const name of ["restore", "restore_inner", "run_restore", "restore_from_manifest"]) {
  const f = fnBody(src, name);
  if (f && /\bcreate_tables\s*\(/.test(f.body)) { host = { name, ...f }; break; }
}
if (!host) fail("could not find the function containing the create_tables( call in src/restore/mod.rs");

// Accept any of the shapes the plan permits, including the mandated newtype constructor.
const COMPLETENESS = /(resolve_attach_completeness|attach_completeness|plan_attach_completeness|CompleteAttachPlan\s*::\s*(try_)?new)/;
const pIdx = host.body.search(COMPLETENESS);
const cIdx = host.body.search(/\bcreate_tables\s*\(/);

if (pIdx < 0) {
  fail(`no completeness resolution found in ${host.name} - expected one of `
     + `resolve_attach_completeness / attach_completeness / plan_attach_completeness / `
     + `CompleteAttachPlan::try_new`);
}
if (cIdx >= 0 && pIdx > cIdx) {
  fail(`completeness is resolved AFTER create_tables in ${host.name}; by then the table exists and is `
     + `queryable, so refusing to attach still leaves a live EMPTY table - which reads as a `
     + `successful restore of an empty table`);
}

console.log(`OK: completeness is resolved before the CREATE phase in ${host.name}`);
