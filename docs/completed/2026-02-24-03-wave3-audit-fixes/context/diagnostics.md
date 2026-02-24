# Diagnostics

## Compiler State (cargo check)

**Date:** 2026-02-24
**Result:** PASS -- zero errors, zero warnings

```
Checking chbackup v0.1.0 (/Users/rafael.siqueira/dev/personal/chbackup)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.70s
```

## Pre-Existing Issues

None. The codebase compiles cleanly with zero warnings.

## Files To Be Modified

| File | Finding | Current State |
|------|---------|---------------|
| `src/restore/remap.rs` | W3-1: `&&` should be `||` at line 647 | Compiles, logic bug |
| `src/watch/mod.rs` | W3-2: `name.contains("full"/"incr")` at lines 145-146 | Compiles, fragile classification |
| `src/server/routes.rs` | W3-3: `watch_start` has no body param | Compiles, missing feature |
| `src/config.rs` | W3-4: `watch.enabled` gate on interval validation at line 1400 | Compiles, validation gap |
| `src/cli.rs` | W3-5: Server variant missing watch flags | Compiles, missing feature |
| `src/main.rs` | W3-5: Server dispatch doesn't wire watch flags | Compiles, missing wiring |
