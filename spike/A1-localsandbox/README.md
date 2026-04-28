# Spike A1 — LocalSandbox

> **One question:** does `claude-code-acp` complete a turn reliably when run as a plain Node OS process (no V8 isolate around it)?
>
> **One outcome:** if 50 consecutive turns succeed with zero hangs, the v2 plan is greenlit. If any hangs, the architecture itself needs reconsideration before Phase B begins.

This is a throwaway spike. Production cuartel-acp is Rust against the `agent-client-protocol` crate (Phase B1). This directory exists to falsify (or confirm) the V8-vs-OS root-cause hypothesis as cheaply as possible.

See `ARCHITECTURE_REFACTOR_V2.md` Phase A1 for the DoD this spike must satisfy.

## What's in here

| File | What |
|---|---|
| `src/acp-client.ts` | Hand-rolled JSON-RPC-over-stdio ACP client (~200 LOC). Verbose by default. |
| `src/protocol.ts` | ACP method names + param shapes used by the spike. One place to fix if claude-code-acp's wire differs from our assumption. |
| `src/run-once.ts` | One full turn end-to-end. Run this first. |
| `src/run-50x.ts` | The load-bearing test: 50 iterations, count hangs. |
| `src/test-load-session.ts` | Secondary: does `session/load` work? Resolves open Q 3 in v2 doc. |
| `package.json` | Single dep: `@zed-industries/claude-code-acp`. ESM, Node ≥22 (uses native TS strip). |
| `results/` | Gitignored. `run-50x` writes `run-50x-<ISO>.json` here. |

## Prerequisites

1. **Node ≥22** (uses `--experimental-strip-types` to run `.ts` directly; you have 25.2.1).
2. **Anthropic credentials** — either:
   - Existing `claude` CLI auth at `~/.claude/` (you already have this — `claude-code-acp` inherits it), OR
   - `ANTHROPIC_API_KEY` env var.

Nothing else. No global installs needed.

## How to run (after review)

From `spike/A1-localsandbox/`:

```bash
# Step 1: install the one dep (claude-code-acp).
npm install

# Step 2: prove the wire works with a single verbose turn.
# Watch the JSON-RPC frames going both directions.
npm run once

# Step 3: the load-bearing test. ~10-15 min. ~$1-2 in API spend.
npm run fifty

# Step 4 (independent): does session resume work?
npm run load-session
```

Each script's exit code is the test result:
- `npm run once`: 0 = succeeded, 1 = failed
- `npm run fifty`: 0 = 50/50 success (hypothesis confirmed), 1 = any hangs/errors, 2 = fatal
- `npm run load-session`: prints conclusion to stdout

## How to interpret results

### `npm run once`
Prints every JSON-RPC line in/out. Use this to verify our protocol assumptions match claude-code-acp's actual behavior. If the method names or param shapes are wrong, you'll see the error here and we can fix `protocol.ts` before running the 50× test.

Expected: ends with `Result: { ok: true, durationMs: <some-ms>, stopReason: "end_turn", ... }`.

### `npm run fifty`
Outputs per-iteration progress. Final summary:

```
Successes: 50/50
Hangs:     0/50
Errors:    0/50
Duration:  p50=...ms  p95=...ms  max=...ms
✓ HYPOTHESIS CONFIRMED: V8 nesting was the cause.
  Green light to start Phase B (cuartel-acp + AppleVzSandbox).
```

If anything other than 50/50, the test exits non-zero and prints `✗ HYPOTHESIS NOT CONFIRMED`. Investigate before Phase B.

Per-iteration data lands in `results/run-50x-<timestamp>.json` for the FINDINGS doc to reference.

### `npm run load-session`
Prints one of:
- `✓ RESUME WORKS: agent recalled the word.` — Phase D can rely on `session/load`.
- `✗ RESUME PARTIAL: session/load returned ok but agent did not recall.` — works at protocol level but transcript context isn't restored. Phase D needs to replay messages.
- `✗ RESUME UNSUPPORTED` — `session/load` errors. Phase D needs a fallback strategy (most likely: keep transcript host-side, replay on resume).

## What this spike does NOT test

Out of scope (each is a later phase):
- Tool-call normalization across providers (B1)
- Multi-provider support beyond Claude (B1+)
- Sandbox VM cold-start (B2)
- portless / branch-named URLs (B2 + C3)
- Persistence (C2)
- UI integration (C2+)
- Anything user-facing

## Tearing this down

Once Phase B starts, this spike is no longer relevant. Either:
- Keep it as a regression check for "does claude-code-acp still hang-free outside V8?", OR
- Delete the directory and rely on cuartel-acp's own integration tests.

Recommend keeping until the end of Phase B, then archive.
