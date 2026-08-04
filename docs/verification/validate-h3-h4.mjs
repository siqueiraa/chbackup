#!/usr/bin/env node
// Gate validator for the H3 and H4 claims (Replicated-restore semantics).
//
// Unlike H1, these are BEHAVIORAL claims about a running ClickHouse cluster
// (does attaching during in-flight replication duplicate rows? does {uuid}
// expand per-node into separate replication groups?). No text file can settle
// them, and this validator deliberately does NOT pretend to verify truth.
//
// Instead it enforces the property that makes an unverifiable claim safe:
//
//   A verdict may only ever make behavior MORE conservative, never less.
//
// So the validator requires typed per-claim verdicts, forbids a single blended
// verdict that would let one claim's confidence leak into the other, and
// requires each claim to state the conservative fallback that applies when it
// is not CONFIRMED. Whether the plan honors that fallback is asserted by the
// consuming tasks' own gates (T12, T13).

import fs from "node:fs";

const ART = "docs/verification/h3-h4-replicated-semantics.json";
const VERDICTS = ["CONFIRMED", "REFUTED", "UNKNOWN"];

const fail = msg => {
  console.error("FAIL: " + msg);
  process.exit(1);
};

let art;
try {
  art = JSON.parse(fs.readFileSync(ART, "utf8"));
} catch (e) {
  fail(`artifact missing or not valid JSON (${ART}): ${e.message}`);
}

if (art.claim_set !== "h3-h4-replicated-semantics") {
  fail("claim_set must be 'h3-h4-replicated-semantics'");
}

// A single top-level verdict is rejected outright: H3 and H4 are independent
// claims consumed by different tasks with OPPOSITE default polarities, so a
// blended verdict is meaningless and dangerous.
if ("verdict" in art) {
  fail("a combined top-level 'verdict' is not allowed - H3 and H4 are independent claims "
     + "with opposite default polarities (T12 defaults safe-on, T13 defaults off). "
     + "Report art.h3.verdict and art.h4.verdict separately.");
}

for (const key of ["h3", "h4"]) {
  const c = art[key];
  if (!c || typeof c !== "object") fail(`missing claim object '${key}'`);
  if (!VERDICTS.includes(c.verdict)) {
    fail(`${key}.verdict must be one of ${VERDICTS.join("/")}, got ${JSON.stringify(c.verdict)}`);
  }

  // Every claim must name the conservative behavior that applies when it is not
  // CONFIRMED. This is the field that keeps an unverified claim safe.
  if (typeof c.conservative_fallback !== "string" || c.conservative_fallback.trim().length < 20) {
    fail(`${key}.conservative_fallback must describe what happens when this claim is NOT confirmed`);
  }

  // A CONFIRMED verdict is the only one permitted to carry weight, so it is the
  // only one required to cite something checkable.
  if (c.verdict === "CONFIRMED") {
    const src = c.sources;
    if (!Array.isArray(src) || src.length < 1) {
      fail(`${key}.verdict is CONFIRMED, so it must cite at least one source in ${key}.sources`);
    }
    for (const s of src) {
      if (typeof s.url !== "string" || !/^https?:\/\/|^src\/|^tests?\//.test(s.url)) {
        fail(`${key} source url must be an http(s) URL or a repo-relative source/test path, got ${JSON.stringify(s.url)}`);
      }
      if (typeof s.version_or_commit !== "string" || s.version_or_commit.trim().length === 0) {
        fail(`${key} source must record a version or commit`);
      }
    }
    if (typeof c.how_observed !== "string" || c.how_observed.trim().length < 30) {
      fail(`${key}.verdict is CONFIRMED, so ${key}.how_observed must describe how it was observed `
         + `(which cluster/version, what was run, what was seen) - not merely asserted`);
    }
  }

  // An UNKNOWN verdict must say what would settle it, so the gap is actionable
  // rather than a shrug.
  if (c.verdict === "UNKNOWN" && (typeof c.what_would_settle_it !== "string" || c.what_would_settle_it.trim().length < 20)) {
    fail(`${key}.verdict is UNKNOWN, so ${key}.what_would_settle_it must say what evidence would resolve it`);
  }
}

const summary = ["h3", "h4"].map(k => `${k}=${art[k].verdict}`).join(", ");
console.log(`OK: typed independent verdicts present (${summary}); `
          + `conservative fallbacks recorded for both`);
