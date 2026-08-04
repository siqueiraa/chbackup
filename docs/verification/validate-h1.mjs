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
let cur = null;
for (const line of raw.split("\n")) {
  const header = line.match(/^##\s+(\S+)/);
  if (header) {
    cur = header[1];
    versions[cur] = {};
    continue;
  }
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
