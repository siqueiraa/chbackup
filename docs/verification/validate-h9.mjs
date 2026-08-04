#!/usr/bin/env node
// Gate validator for the H9 claim (does pre-24.3 ClickHouse ATTACH reject a part
// whose checksums.txt references stripped projections?).
//
// Like H3/H4 this is a behavioral claim this validator cannot settle. What it
// enforces is that the claim can only ever move the THRESHOLD, never remove the
// gate: T16's projection restore-gate defaults ON regardless of the verdict,
// because refusing to restore a possibly-unrestorable backup is conservative on
// its own merits. A CONFIRMED verdict supplies a precise min_version; anything
// else falls back to the conservative 24.3 threshold upstream clickhouse-backup
// uses.

import fs from "node:fs";

const ART = "docs/verification/h9-projection-gate.json";
const VERDICTS = ["CONFIRMED", "REFUTED", "UNKNOWN"];
// Upstream Altinity/clickhouse-backup gates at >= 24.3.0.0; used as the
// conservative fallback when this claim is not independently confirmed.
const CONSERVATIVE_FALLBACK = "24.3";

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

if (art.claim_set !== "h9-projection-gate") fail("claim_set must be 'h9-projection-gate'");
if (!VERDICTS.includes(art.verdict)) {
  fail(`verdict must be one of ${VERDICTS.join("/")}, got ${JSON.stringify(art.verdict)}`);
}

// min_version must be either a real version or the literal UNKNOWN - never a
// vague string, and never absent.
const mv = String(art.min_version ?? "");
const isNumeric = /^\d+\.\d+(\.\d+)*$/.test(mv);
if (!isNumeric && mv !== "UNKNOWN") {
  fail(`min_version must be a numeric version like "24.3" or the literal "UNKNOWN", got ${JSON.stringify(art.min_version)}`);
}

if (art.verdict === "CONFIRMED") {
  if (!isNumeric) {
    fail("verdict is CONFIRMED, so min_version must be a numeric version, not UNKNOWN");
  }
  const src = art.sources;
  if (!Array.isArray(src) || src.length < 1) {
    fail("verdict is CONFIRMED, so at least one source must be cited");
  }
  for (const s of src) {
    if (typeof s.url !== "string" || !/^https?:\/\/|^src\/|^tests?\//.test(s.url)) {
      fail(`source url must be an http(s) URL or a repo-relative path, got ${JSON.stringify(s.url)}`);
    }
    if (typeof s.version_or_commit !== "string" || s.version_or_commit.trim().length === 0) {
      fail("each source must record a version or commit");
    }
  }
  if (typeof art.how_observed !== "string" || art.how_observed.trim().length < 30) {
    fail("verdict is CONFIRMED, so how_observed must describe how the ATTACH rejection was actually "
       + "observed (which version, what was attached, what error) - not merely asserted");
  }
} else {
  // Not confirmed: the artifact must acknowledge the gate still applies at the
  // conservative threshold. This is the check that stops an UNKNOWN verdict from
  // being read as permission to drop the gate.
  if (art.gate_still_applies !== true) {
    fail(`verdict is ${art.verdict}, so gate_still_applies must be boolean true - the restore gate `
       + `stays ON regardless of this claim, because refusing a possibly-unrestorable backup is `
       + `conservative on its own merits`);
  }
  if (String(art.fallback_min_version ?? "") !== CONSERVATIVE_FALLBACK) {
    fail(`verdict is ${art.verdict}, so fallback_min_version must be "${CONSERVATIVE_FALLBACK}" `
       + `(the threshold upstream clickhouse-backup uses)`);
  }
}

const eff = art.verdict === "CONFIRMED" ? mv : CONSERVATIVE_FALLBACK;
console.log(`OK: verdict=${art.verdict}, effective restore-gate threshold=${eff} `
          + `(gate is ON either way)`);
