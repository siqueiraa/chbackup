# Gate check scripts — what these can and cannot prove

These scripts supplement the plan's acceptance gates. Their scope is deliberately narrow,
and the narrowing was learned the hard way.

## The rule

**A check here may only assert a fact that is true or false from the source text alone,
requiring no reasoning about reachability, data flow, or control flow.**

In practice that means one shape:

> the old unsafe path is GONE

That is a sound static assertion. Deleting the fail-open `continue`, the
`non-fatal, proceeding with CREATE` fall-through, or the `is_benign_attach_error`
misclassification are all textual facts. Once the unsafe path is deleted there is nothing
for dead code to hide behind, which is why this single shape carries real weight.

## What was removed, and why

Earlier versions of these scripts tried to prove semantic properties statically —
"does the production path *consume* this planner", "is this escape *governed by* the ZK
result", "does the create set *derive from* the completeness plan". Each attempt was
defeated, hardened, and defeated again. Across two review rounds the data-flow checks
produced **five false negatives that rejected correct implementations**, including:

- `let plan = ...; delete_objects(&plan[..100]); delete_objects(&plan[100..]);`
  — legitimate batching, rejected by an "exactly one deletion call" rule. Worse,
  `gc_delete_backup` *must* issue two deletions (unreferenced keys, then the manifest
  last) because manifest-last ordering is what makes a crash recoverable. The rule
  would have rejected the design's required pattern.
- `CompleteAttachPlan::try_new(parts)` — the constructor the plan itself **mandates**,
  rejected because the check only recognised a fixed list of helper names.
- `let action = zk_check_action(check, strict); if strict && matches!(action, ..)`
  — a correct guard through an intermediate variable, rejected by a textual
  proximity search.

A gate that blocks the mandated implementation is worse than no gate: it burns
iterations and pressures the implementer toward whatever contorted shape happens to
satisfy the scanner.

## Where semantic proof actually lives

1. **The named unit tests.** Each task requires tests asserting observable behavior
   (deleted set EQUALS planner set; a manifest error yields an EMPTY delete log; strict
   Indeterminate does not create the table). A test either observes the right outcome or
   it does not — no reachability inference needed.
2. **The per-task evaluator.** It reads the actual diff and can reason about control flow,
   which a regex cannot. Semantic judgement belongs there.
3. **These scripts**, for the one narrow job above.

Do not add reachability or data-flow checks here. If you find yourself writing a
brace-matcher or a derivation tracker, the property belongs in a test.
