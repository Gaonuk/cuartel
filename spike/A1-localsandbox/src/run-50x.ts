/**
 * The load-bearing test: run a full ACP turn 50 times in a row, count hangs.
 *
 * This is the test that decides whether v2's architecture is sound. If
 * 50/50 succeed, the V8-vs-OS hypothesis is confirmed (the sendPrompt hang
 * was caused by V8 nesting, not by something inside claude-code-acp). If
 * any hang reproduces, the architecture itself needs reconsideration
 * before Phase B begins.
 *
 * Each iteration:
 *   - spawns a fresh `claude-code-acp` subprocess
 *   - completes one prompt → result cycle
 *   - tears down the subprocess
 *   - is bounded by HANG_TIMEOUT_MS — if it exceeds, mark as hang and continue
 *
 * Outputs results to results/run-50x-<ISO>.json for the FINDINGS doc.
 */
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promises as fs } from "node:fs";
import { runOnce } from "./run-once.ts";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const ITERATIONS = 50;
const HANG_TIMEOUT_MS = 60_000; // 60s per iteration; longer = hang
const RESULTS_DIR = path.resolve(__dirname, "../results");

interface IterationResult {
  index: number;
  startedAt: string;
  durationMs: number;
  outcome: "success" | "hang" | "error";
  stopReason?: string;
  errorMessage?: string;
  notificationCount: number;
  toolCallCount: number;
}

interface RunSummary {
  startedAt: string;
  finishedAt: string;
  iterations: number;
  successCount: number;
  hangCount: number;
  errorCount: number;
  durations: { p50: number; p95: number; max: number };
  results: IterationResult[];
}

async function withTimeout<T>(p: Promise<T>, ms: number): Promise<T | "TIMEOUT"> {
  return Promise.race<T | "TIMEOUT">([
    p,
    new Promise<"TIMEOUT">((resolve) => setTimeout(() => resolve("TIMEOUT"), ms)),
  ]);
}

function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0;
  const idx = Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length));
  return sorted[idx];
}

async function main() {
  await fs.mkdir(RESULTS_DIR, { recursive: true });
  const startedAt = new Date().toISOString();
  const results: IterationResult[] = [];

  console.log(`A1 spike: ${ITERATIONS} iterations, ${HANG_TIMEOUT_MS}ms timeout each.`);
  console.log("Each iteration spawns a fresh claude-code-acp and runs one full turn.");
  console.log("");

  for (let i = 1; i <= ITERATIONS; i++) {
    const iterStart = new Date().toISOString();
    process.stdout.write(`[${i.toString().padStart(2, " ")}/${ITERATIONS}] `);

    const t0 = Date.now();
    // verbose=false during the 50× run; one-shot run-once already proved the wire.
    const outcome = await withTimeout(runOnce(false), HANG_TIMEOUT_MS);
    const durationMs = Date.now() - t0;

    if (outcome === "TIMEOUT") {
      results.push({
        index: i,
        startedAt: iterStart,
        durationMs,
        outcome: "hang",
        notificationCount: 0,
        toolCallCount: 0,
      });
      console.log(`HANG (${durationMs}ms)`);
    } else if (outcome.ok) {
      results.push({
        index: i,
        startedAt: iterStart,
        durationMs,
        outcome: "success",
        stopReason: outcome.stopReason,
        notificationCount: outcome.notificationCount,
        toolCallCount: outcome.toolCallCount,
      });
      console.log(
        `OK (${durationMs}ms, ${outcome.notificationCount} updates, ${outcome.toolCallCount} tool calls)`,
      );
    } else {
      results.push({
        index: i,
        startedAt: iterStart,
        durationMs,
        outcome: "error",
        errorMessage: outcome.error,
        notificationCount: outcome.notificationCount,
        toolCallCount: outcome.toolCallCount,
      });
      console.log(`ERROR (${durationMs}ms): ${outcome.error}`);
    }
  }

  const finishedAt = new Date().toISOString();
  const successCount = results.filter((r) => r.outcome === "success").length;
  const hangCount = results.filter((r) => r.outcome === "hang").length;
  const errorCount = results.filter((r) => r.outcome === "error").length;
  const durations = results.map((r) => r.durationMs).sort((a, b) => a - b);

  const summary: RunSummary = {
    startedAt,
    finishedAt,
    iterations: ITERATIONS,
    successCount,
    hangCount,
    errorCount,
    durations: {
      p50: percentile(durations, 50),
      p95: percentile(durations, 95),
      max: durations[durations.length - 1] ?? 0,
    },
    results,
  };

  const outFile = path.join(RESULTS_DIR, `run-50x-${startedAt.replaceAll(":", "-")}.json`);
  await fs.writeFile(outFile, JSON.stringify(summary, null, 2));

  console.log("");
  console.log("=".repeat(60));
  console.log(`Successes: ${successCount}/${ITERATIONS}`);
  console.log(`Hangs:     ${hangCount}/${ITERATIONS}`);
  console.log(`Errors:    ${errorCount}/${ITERATIONS}`);
  console.log(`Duration:  p50=${summary.durations.p50}ms  p95=${summary.durations.p95}ms  max=${summary.durations.max}ms`);
  console.log("");
  console.log(`Results written to: ${outFile}`);
  console.log("");

  if (hangCount === 0 && errorCount === 0) {
    console.log("✓ HYPOTHESIS CONFIRMED: V8 nesting was the cause.");
    console.log("  Green light to start Phase B (cuartel-acp + AppleVzSandbox).");
    process.exit(0);
  } else {
    console.log("✗ HYPOTHESIS NOT CONFIRMED: investigate hangs/errors before Phase B.");
    process.exit(1);
  }
}

main().catch((e) => {
  console.error("Fatal:", e);
  process.exit(2);
});
