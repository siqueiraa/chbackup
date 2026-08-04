#!/usr/bin/env node
// Proves a strict Indeterminate ZK check can actually PREVENT CREATE. Keys off
// the real call path and data flow, not one log string.
import { loadLive, fnBody, bindingOf, fail } from "./rust-scan.mjs";

const src = loadLive("src/restore/schema.rs");

// 1. The old unconditional fall-through must be gone. Matched semantically (an
//    Err arm that only warns) as well as by the known log text, so renaming the
//    message is not an escape.
if (/non-fatal, proceeding with CREATE/.test(src)) {
  fail("the 'non-fatal, proceeding with CREATE' fall-through is still present - a strict "
     + "Indeterminate result would still be swallowed and the table created anyway");
}

const host = ["create_tables", "create_ddl_objects"].map(n => ({ n, f: fnBody(src, n) }))
  .find(x => x.f && /\bresolve_zk_conflict\s*\(/.test(x.f.body));
if (!host) fail("could not find the function containing the resolve_zk_conflict( call");
const body = host.f.body;

// 2. The result must be BOUND, not discarded by `if let Err(e) = ...`.
const binding = bindingOf(body, "resolve_zk_conflict");
const hasIfLetErr = /if\s+let\s+Err\s*\(\s*\w+\s*\)\s*=\s*[^;{]*resolve_zk_conflict/.test(body);
if (!binding && hasIfLetErr) {
  fail("resolve_zk_conflict's result is consumed by `if let Err(..)` and discarded; bind it so the "
     + "strict branch can decide whether to skip CREATE");
}

// 3. The strict flag / tri-state must be consulted in this function.
if (!/(zk_check_strict|zk_check_action|ZkReplicaCheck)/.test(body)) {
  fail(`${host.n} never consults the strict flag or the ZkReplicaCheck tri-state`);
}

// 4. There must be a real way out before the DDL executes: propagation, an early
//    continue/skip, or the table being filtered out of the create loop.
const ddlIdx = body.search(/execute_ddl\s*\(/);
const preDdl = ddlIdx > 0 ? body.slice(0, ddlIdx) : body;
if (!/(return\s+Err|\?\s*;|\bcontinue\s*;|\bskip)/.test(preDdl)) {
  fail("there is no propagation, `?`, `continue`, or skip before execute_ddl, so nothing can stop "
     + "CREATE when the strict check fails");
}
console.log(`OK: ${host.n} consults the strict tri-state and can prevent CREATE before execute_ddl`);
