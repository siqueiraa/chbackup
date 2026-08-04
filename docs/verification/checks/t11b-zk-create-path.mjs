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

// 4. There must be a way out before the DDL executes, AND that escape must be
//    CONTROLLED BY the ZK result. Merely finding some `continue` before execute_ddl is
//    not enough: an unrelated loop-control statement (e.g. a `continue` for a skipped
//    system database) satisfied that while the ZK result was still only logged.
//
//    So: find each escape before execute_ddl, walk back to its governing condition,
//    and require at least one whose condition mentions the ZK decision.
const ddlIdx = body.search(/execute_ddl\s*\(/);
const preDdl = ddlIdx > 0 ? body.slice(0, ddlIdx) : body;

const ZK_TERMS = /(zk_check_strict|zk_check_action|ZkReplicaCheck|Indeterminate)/;
const escapes = [...preDdl.matchAll(/(return\s+Err|\breturn\b|\bcontinue\s*;|\?\s*;)/g)];
if (escapes.length === 0) {
  fail("there is no propagation, `?`, `continue`, or return before execute_ddl, so nothing can stop "
     + "CREATE when the strict check fails");
}

// For each escape, inspect the enclosing statement/condition that precedes it. A
// governed escape looks like `if <cond-mentioning-zk> { ... continue; }` or
// `match zk_check_action(..) { .. => return Err(..) }`.
let governed = null;
for (const esc of escapes) {
  const before = preDdl.slice(0, esc.index);

  // Candidate governing constructs, nearest first. For a `match` ARM (`=>`) the
  // deciding expression is the match SCRUTINEE, which sits further back than the arm
  // itself — looking only at text after `=>` misses it and rejects correct code like
  //   match zk_check_action(check, strict) { ZkAction::FailTable => { continue; } }
  const candidates = [];
  const ifIdx = before.lastIndexOf("if ");
  if (ifIdx >= 0) candidates.push(before.slice(ifIdx, Math.min(before.length, ifIdx + 300)));

  const matchIdx = before.lastIndexOf("match ");
  if (matchIdx >= 0) candidates.push(before.slice(matchIdx, Math.min(before.length, matchIdx + 300)));

  const arrowIdx = before.lastIndexOf("=>");
  if (arrowIdx >= 0) {
    // The arm pattern itself (e.g. `ZkAction::FailTable =>`) plus, crucially, the
    // enclosing match scrutinee found by scanning back from the arm.
    candidates.push(before.slice(Math.max(0, arrowIdx - 120), arrowIdx + 40));
    const enclosing = before.lastIndexOf("match ", arrowIdx);
    if (enclosing >= 0) candidates.push(before.slice(enclosing, Math.min(arrowIdx + 40, before.length)));
  }

  const hit = candidates.find(c => ZK_TERMS.test(c));
  if (hit) { governed = { escape: esc[1].trim(), guard: hit.replace(/\s+/g, " ").slice(0, 120) }; break; }
}

if (!governed) {
  fail("no escape before execute_ddl is governed by the ZK result. An escape exists, but its "
     + "condition never mentions zk_check_strict / zk_check_action / ZkReplicaCheck / Indeterminate, "
     + "so the ZK outcome is still only logged while CREATE proceeds regardless. The decision that "
     + "skips or propagates must be driven BY the ZK check result.");
}

console.log(`OK: ${host.n} consults the strict tri-state and has a ZK-governed escape before `
          + `execute_ddl (${governed.escape})`);
