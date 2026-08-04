// Shared Rust source scanning helpers for the plan's gate checks.
//
// WHY THIS EXISTS: a naive `grep` for "does function F call G" is defeated by a
// dead branch — `if false { G(); }` satisfies the grep while the real path still
// runs the old unsafe code. These helpers strip provably-dead constructs and
// test modules before scanning, and extract brace-BALANCED function bodies rather
// than a fixed-size character window (a window either truncates a long function
// or bleeds into the next one).
//
// These checks are still static and therefore still a proxy. The primary proof of
// behavior is each task's named unit test asserting an observable outcome; these
// checks exist to prove the production path actually CONSUMES the tested logic,
// and — most importantly — that the OLD unsafe path is GONE. Deleting the old path
// is the strongest static signal available, because once it is gone there is no
// working unsafe branch left for dead code to hide.

import fs from "node:fs";

export const fail = msg => {
  console.error("FAIL: " + msg);
  process.exit(1);
};

/** Strip line and block comments, preserving offsets loosely (length-safe). */
export function stripComments(src) {
  let out = "";
  let i = 0;
  let inStr = null;
  while (i < src.length) {
    const c = src[i], d = src[i + 1];
    if (inStr) {
      out += c;
      if (c === "\\") { out += d ?? ""; i += 2; continue; }
      if (c === inStr) inStr = null;
      i++;
      continue;
    }
    if (c === '"') { inStr = '"'; out += c; i++; continue; }
    if (c === "/" && d === "/") {
      while (i < src.length && src[i] !== "\n") { out += " "; i++; }
      continue;
    }
    if (c === "/" && d === "*") {
      const end = src.indexOf("*/", i + 2);
      const stop = end === -1 ? src.length : end + 2;
      out += " ".repeat(stop - i);
      i = stop;
      continue;
    }
    out += c;
    i++;
  }
  return out;
}

/** Find the balanced `{...}` block starting at or after `from`. Returns [start, end]. */
function balancedBlock(src, from) {
  const start = src.indexOf("{", from);
  if (start === -1) return null;
  let depth = 0;
  for (let i = start; i < src.length; i++) {
    const c = src[i];
    if (c === "{") depth++;
    else if (c === "}") {
      depth--;
      if (depth === 0) return [start, i + 1];
    }
  }
  return null;
}

/**
 * Remove constructs that cannot execute: `if false { ... }`, `#[cfg(any())]`
 * items, and `#[cfg(test)] mod ... { ... }` blocks. Replaced with equal-length
 * whitespace so later offset comparisons stay meaningful.
 */
export function stripDead(src) {
  let s = src;
  const blank = (str, a, b) => str.slice(0, a) + " ".repeat(b - a) + str.slice(b);

  // `if false { ... }` / `if cfg!(any()) { ... }`
  for (const re of [/\bif\s+false\s*\{/g, /\bif\s+cfg!\(\s*any\(\s*\)\s*\)\s*\{/g]) {
    let m;
    while ((m = re.exec(s))) {
      const blk = balancedBlock(s, m.index);
      if (!blk) break;
      s = blank(s, m.index, blk[1]);
      re.lastIndex = 0;
    }
  }
  // `#[cfg(test)] mod tests { ... }` and `#[cfg(any())] <item> { ... }`
  for (const re of [/#\[cfg\(test\)\]\s*mod\s+\w+\s*\{/g, /#\[cfg\(any\(\s*\)\)\]\s*[^;{]*\{/g]) {
    let m;
    while ((m = re.exec(s))) {
      const blk = balancedBlock(s, m.index);
      if (!blk) break;
      s = blank(s, m.index, blk[1]);
      re.lastIndex = 0;
    }
  }
  return s;
}

/** Load a Rust file with comments and provably-dead code removed. */
export function loadLive(path) {
  let raw;
  try {
    raw = fs.readFileSync(path, "utf8");
  } catch (e) {
    // Must be an assertion, never an uncaught throw: a crash exit is easily misread
    // as a real finding, and a crash on some other path could exit 0.
    fail(`cannot read ${path} (${e.code || e.message}) - the gate cannot verify anything`);
  }
  return stripDead(stripComments(raw));
}

/**
 * Extract the brace-balanced body of `fn <name>`. Returns {body, start, end}.
 * Skips the signature (including generics/args/where-clause) by finding the
 * first `{` after the fn keyword at depth 0 of the parameter list.
 */
export function fnBody(src, name) {
  const re = new RegExp(`\\bfn\\s+${name}\\s*[(<]`);
  const m = re.exec(src);
  if (!m) return null;
  const blk = balancedBlock(src, m.index);
  if (!blk) return null;
  return { body: src.slice(blk[0], blk[1]), start: blk[0], end: blk[1] };
}

/** True if `body` contains a call to `name` that is not inside a nested dead block. */
export function callsFn(body, name) {
  return new RegExp(`\\b${name}\\s*\\(`).test(body);
}

/**
 * Find the identifier a call's result is bound to: `let X = name(...)` or
 * `let X = name(...).await?`. Returns the binding name or null.
 */
export function bindingOf(body, name) {
  const re = new RegExp(`let\\s+(?:mut\\s+)?(\\w+)\\s*(?::[^=]+)?=\\s*[^;]*?\\b${name}\\s*\\(`, "s");
  const m = re.exec(body);
  return m ? m[1] : null;
}
