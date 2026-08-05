#!/usr/bin/env node
// Asserts textual facts only: the unconditional fall-through is gone, and the strict
// flag / tri-state is referenced where CREATE happens.
//
// It does NOT try to prove the escape is *governed by* the ZK result — a proximity
// search rejected a correct guard through an intermediate `action` variable. That
// property is proved by the named test zk_strict_indeterminate_blocks_create, which
// must assert the table is NOT created. See README.md.
import { loadLive, fnBody, fail } from "./rust-scan.mjs";

const src = loadLive("src/restore/schema.rs");

// The specific fall-through that makes a strict result meaningless must be deleted.
// While it exists, resolve_zk_conflict returning Err cannot stop CREATE.
if (/non-fatal, proceeding with CREATE/.test(src)) {
  fail("the 'non-fatal, proceeding with CREATE' fall-through is still present at the "
     + "resolve_zk_conflict call site - a strict Indeterminate result would still be swallowed and "
     + "the table created anyway, making the tri-state a no-op");
}

const host = ["create_tables", "create_ddl_objects"]
  .map(n => ({ n, f: fnBody(src, n) }))
  .find(x => x.f && /\bresolve_zk_conflict\s*\(/.test(x.f.body));
if (!host) fail("could not find the function containing the resolve_zk_conflict( call");

// The strict decision must at least be referenced where CREATE happens.
if (!/(zk_check_strict|zk_check_action|ZkReplicaCheck)/.test(host.f.body)) {
  fail(`${host.n} never references zk_check_strict / zk_check_action / ZkReplicaCheck, so the ZK `
     + `outcome cannot be influencing whether the table is created`);
}

console.log(`OK: the unconditional fall-through is gone and ${host.n} references the strict tri-state`);
