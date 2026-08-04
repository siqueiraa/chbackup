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
// Data flow. Merely MENTIONING the binding in the argument list is not enough:
// `create_tables(&all_tables, complete.is_complete())` passes the binding while still
// handing over every table, so the incomplete ones get created anyway. What must be
// true is that the TABLE COLLECTION argument is derived from the completeness result.
//
// Extract the balanced argument list and check the collection-shaped arguments.
const open = host.body.indexOf("(", cIdx);
let depth = 0, end = -1;
for (let i = open; i < host.body.length; i++) {
  if (host.body[i] === "(") depth++;
  else if (host.body[i] === ")") { depth--; if (depth === 0) { end = i; break; } }
}
if (end < 0) fail("could not parse create_tables' argument list");
const rawArgs = host.body.slice(open + 1, end);

// Split top-level arguments (ignore commas nested inside (), [], <>).
const args = [];
let cur = "", d2 = 0;
for (const ch of rawArgs) {
  if ("([<".includes(ch)) d2++;
  else if (")]>".includes(ch)) d2--;
  if (ch === "," && d2 === 0) { args.push(cur.trim()); cur = ""; continue; }
  cur += ch;
}
if (cur.trim()) args.push(cur.trim());

// Follow derivation transitively: `let to_create = complete.creatable_tables();` means
// `to_create` counts as derived from `complete`, so passing `&to_create` is correct even
// though the argument never names `complete`. Without this the check rejects the most
// natural correct implementation.
const derived = new Set([binding]);
const preCall = host.body.slice(0, cIdx);
for (let pass = 0; pass < 4; pass++) {
  const before = derived.size;
  const letRe = /let\s+(?:mut\s+)?(\w+)\s*(?::[^=]+)?=\s*([^;]+);/g;
  let lm;
  while ((lm = letRe.exec(preCall))) {
    const [, name, rhs] = lm;
    if (derived.has(name)) continue;
    if ([...derived].some(d => new RegExp(`\\b${d}\\b`).test(rhs))) derived.add(name);
  }
  if (derived.size === before) break;
}

const bindingRe = new RegExp(`\\b(${[...derived].join("|")})\\b`);
if (!args.some(x => bindingRe.test(x))) {
  fail(`create_tables receives nothing derived from the completeness result \`${binding}\` `
     + `(tracked derivations: ${[...derived].join(", ")}) - compute the create set from it so `
     + `incomplete tables are excluded from CREATE`);
}

// The argument carrying the binding must not be a bare boolean/accessor while the
// table collection is passed separately and unfiltered. Reject the known-bad shape:
// a plain `phases.data_tables` / `&manifest.tables` style collection argument
// alongside a scalar completeness flag.
const RAW_COLLECTION = /^&?\s*(phases\.\w*tables\w*|manifest\.tables|all_tables|tables)\b/;
const rawCollectionArg = args.find(x => RAW_COLLECTION.test(x) && !bindingRe.test(x));
const bindingArg = args.find(x => bindingRe.test(x));
const bindingLooksScalar = /\.(is_complete|is_ok|len|count)\s*\(\s*\)\s*$|^&?\s*\w+\s*$/.test(bindingArg ?? "");

if (rawCollectionArg && bindingLooksScalar) {
  fail(`create_tables receives the unfiltered table collection \`${rawCollectionArg}\` alongside a `
     + `scalar completeness value \`${bindingArg}\`. Passing the flag next to the full list does not `
     + `exclude anything - the incomplete tables are still created. The table-set argument itself must `
     + `be derived from \`${binding}\` (e.g. a filtered Vec), or incomplete tables must be removed `
     + `before this call.`);
}

console.log(`OK: ${fnName} is resolved before create_tables and the create set is derived from `
          + `\`${binding}\` (arg: ${bindingArg})`);
