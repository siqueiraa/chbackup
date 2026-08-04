#!/usr/bin/env node
// Gate validator for the H1 claim (ClickHouse ATTACH/FREEZE error-code mapping).
//
// This does NOT trust the artifact's prose. It parses the vendored real
// ClickHouse source excerpt (docs/verification/sources/clickhouse-error-codes.txt,
// taken verbatim from src/Common/ErrorCodes.cpp at the four CI version tags) and
// requires the artifact to agree with it code-for-code. A fabricated artifact
// cannot pass, because the expected values are derived from committed upstream
// source rather than from anything the artifact says.
//
// Exit 0 = artifact agrees with upstream ClickHouse. Exit 1 = it does not.

import fs from "node:fs";

const SRC = "docs/verification/sources/clickhouse-error-codes.txt";
const ART = "docs/verification/h1-error-codes.json";
const REQUIRED_CODES = ["218", "232", "233", "235", "384"];
// The exact upstream tags the excerpt must carry. Pinned so that four relabelled
// or duplicated sections cannot fabricate cross-version provenance: the mapping
// being right is not enough, it must be right AT THESE FOUR TAGS, which are the
// CI matrix versions. Adding a CI version means updating this list deliberately.
const REQUIRED_TAGS = [
  "v23.8.16.40-lts",
  "v24.3.12.75-lts",
  "v24.8.14.39-lts",
  "v25.1.5.31-stable",
];
// The consequence chbackup depends on: which codes mean "part is missing" vs
// "part is a duplicate". Asserted so a future upstream renumbering breaks CI
// loudly instead of silently re-breaking restore classification.
// Fingerprints of the FULL upstream ErrorCodes.cpp each excerpt was taken from,
// recorded at fetch time. These differ per version because the real files differ,
// which is what makes copy-pasted provenance detectable: four sections claiming
// four tags must carry these four DISTINCT fingerprints.
const FINGERPRINTS = {
  "v23.8.16.40-lts": { sha256: "76c4d78a57ec27f805a046732d6f7a732ad619b78b49cfa8b828ad5240e2b23f", lines: 679 },
  "v24.3.12.75-lts": { sha256: "d9c62fd292e0f370733bb610550ec304c5718c831d1b759e7fe9b6bb2074b370", lines: 694 },
  "v24.8.14.39-lts": { sha256: "e952b05ffef64b88ccb4651b6d7ac0afdb017c2d90a5b13444fa9af4c14a78cd", lines: 705 },
  "v25.1.5.31-stable": { sha256: "de5e5986b566c63073927b59c223c4b03049c05bb2418c391623a62d6d2f4d78", lines: 739 },
};

const LOAD_BEARING = {
  "232": "NO_SUCH_DATA_PART",
  "233": "BAD_DATA_PART_NAME",
  "235": "DUPLICATE_DATA_PART",
  "384": "PART_IS_TEMPORARILY_LOCKED",
  "218": "TABLE_IS_DROPPED",
};

const fail = msg => {
  console.error("FAIL: " + msg);
  process.exit(1);
};

// --- 1. Parse the vendored upstream source -------------------------------
let raw;
try {
  raw = fs.readFileSync(SRC, "utf8");
} catch {
  fail(`vendored upstream source missing: ${SRC}`);
}

const versions = {};
const fingerprints = {};
let cur = null;
for (const line of raw.split("\n")) {
  const header = line.match(/^##\s+(\S+)/);
  if (header) {
    cur = header[1];
    versions[cur] = {};
    continue;
  }
  const fp = line.match(/^FINGERPRINT sha256=([0-9a-f]{64}) lines=(\d+)/);
  if (fp && cur) { fingerprints[cur] = { sha256: fp[1], lines: Number(fp[2]) }; continue; }
  const m = line.match(/^M\((\d+),\s*([A-Z_]+)\)/);
  if (m && cur) versions[cur][m[1]] = m[2];
}

const tags = Object.keys(versions);
if (tags.length !== REQUIRED_TAGS.length) {
  fail(`vendored source must cover all ${REQUIRED_TAGS.length} CI versions, found ${tags.length}: ${tags.join(", ")}`);
}
// Exact tag identity, not just count: a section labelled anything else is not
// evidence about a CI version we actually ship against.
for (const want of REQUIRED_TAGS) {
  if (!tags.includes(want)) {
    fail(`vendored source is missing the required upstream tag ${want}; found: ${tags.join(", ")}`);
  }
}
for (const got of tags) {
  if (!REQUIRED_TAGS.includes(got)) {
    fail(`vendored source contains an unexpected tag ${got} - only the pinned CI tags are accepted `
       + `(${REQUIRED_TAGS.join(", ")}), so relabelled excerpts cannot fake provenance`);
  }
}

// Provenance: each section must carry the fingerprint of the real upstream file for
// its tag, and all four must be distinct. Copy-pasting one excerpt under four
// headings yields identical fingerprints and is rejected here.
const seenFp = new Map();
for (const tag of tags) {
  const want = FINGERPRINTS[tag];
  const got = fingerprints[tag];
  if (!got) fail(`section ${tag} has no FINGERPRINT line - provenance cannot be checked`);
  if (got.sha256 !== want.sha256 || got.lines !== want.lines) {
    fail(`section ${tag} fingerprint does not match the recorded upstream file `
       + `(got sha256=${got.sha256.slice(0,16)}../lines=${got.lines}, `
       + `expected ${want.sha256.slice(0,16)}../lines=${want.lines})`);
  }
  const key = got.sha256 + ":" + got.lines;
  if (seenFp.has(key)) {
    fail(`sections ${seenFp.get(key)} and ${tag} carry the SAME fingerprint - one excerpt was `
       + `copy-pasted under multiple tag headings rather than fetched per version`);
  }
  seenFp.set(key, tag);
}

// The mapping must be identical across every vendored version, else
// same_across_matrix is not a claim anyone can make.
const base = versions[tags[0]];
for (const tag of tags) {
  for (const code of REQUIRED_CODES) {
    if (versions[tag][code] !== base[code]) {
      fail(`vendored mapping for ${code} differs in ${tag}: ${versions[tag][code]} vs ${base[code]}`);
    }
  }
}
for (const code of REQUIRED_CODES) {
  if (!base[code]) fail(`vendored source is missing code ${code}`);
  if (base[code] !== LOAD_BEARING[code]) {
    fail(`vendored source says ${code}=${base[code]} but chbackup's logic assumes ${LOAD_BEARING[code]}. `
       + `Upstream may have renumbered - resolve deliberately, do not just update this file.`);
  }
}

// --- 2. Require the artifact to agree with upstream ----------------------
let art;
try {
  art = JSON.parse(fs.readFileSync(ART, "utf8"));
} catch (e) {
  fail(`artifact missing or not valid JSON (${ART}): ${e.message}`);
}

if (art.claim_set !== "h1-error-codes") fail("claim_set must be 'h1-error-codes'");
if (art.verdict !== "CONFIRMED") {
  fail("verdict must be CONFIRMED - the vendored upstream source settles this claim; "
     + "an UNKNOWN/REFUTED verdict here means the artifact contradicts committed ClickHouse source");
}

const codes = art.codes || {};
for (const code of REQUIRED_CODES) {
  const entry = codes[code];
  if (!entry) fail(`artifact is missing code ${code}`);
  if (entry.symbolic_name !== base[code]) {
    fail(`code ${code}: artifact says "${entry.symbolic_name}" but vendored ClickHouse source says "${base[code]}"`);
  }
  if (entry.same_across_matrix !== true) {
    fail(`code ${code}: same_across_matrix must be boolean true (the vendored source proves it), got ${JSON.stringify(entry.same_across_matrix)}`);
  }
  const ex = entry.example_error_string;
  if (typeof ex !== "string" || !ex.includes(`Code: ${code}`) || !ex.includes(base[code])) {
    fail(`code ${code}: example_error_string must contain both "Code: ${code}" and "${base[code]}"`);
  }
}

// Every cited source must name one of the vendored tags, so the artifact cannot
// cite an unverifiable or invented reference.
const sources = art.sources || [];
if (sources.length < 1) fail("artifact must cite at least one source");
for (const s of sources) {
  if (!tags.includes(s.version_or_commit)) {
    fail(`source version "${s.version_or_commit}" is not one of the vendored tags: ${tags.join(", ")}`);
  }
}

console.log(`OK: artifact agrees with vendored ClickHouse source across ${tags.length} versions `
          + `(${REQUIRED_CODES.map(c => `${c}=${base[c]}`).join(", ")})`);
