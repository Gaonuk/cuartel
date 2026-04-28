/**
 * One full ACP turn end-to-end: spawn → initialize → session/new → prompt → exit.
 *
 * Verbose: prints every wire frame. Use this script first to confirm the
 * basic protocol shape is right before running the 50× hang test.
 *
 * Usage (after `npm install` in this directory):
 *   npm run once
 *
 * Requires Anthropic credentials. claude-code-acp inherits from the
 * `claude` CLI's existing auth (~/.claude/), or from ANTHROPIC_API_KEY.
 */
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promises as fs } from "node:fs";
import { AcpClient } from "./acp-client.ts";
import {
  METHOD,
  type InitializeParams,
  type NewSessionParams,
  type PromptParams,
  type PromptResult,
  type JsonRpcServerRequest,
} from "./protocol.ts";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "../../..");

const TEST_PROMPT =
  "Read README.md and reply with just its first heading text. Be brief — one line.";

interface RunResult {
  ok: boolean;
  durationMs: number;
  stopReason?: string;
  error?: string;
  notificationCount: number;
  toolCallCount: number;
}

export async function runOnce(verbose = true): Promise<RunResult> {
  const start = Date.now();
  let notificationCount = 0;
  let toolCallCount = 0;

  // Server → Client requests we have to implement.
  // claude-code-acp doesn't normally call these for tool work (it runs tools
  // in-process), but Gemini/others do. Implementing read/write keeps the
  // spike compatible with any ACP server.
  const onServerRequest = async (req: JsonRpcServerRequest): Promise<unknown> => {
    if (req.method === METHOD.fsReadTextFile) {
      const params = req.params as { path: string; limit?: number };
      const content = await fs.readFile(params.path, "utf8");
      return { content: params.limit ? content.slice(0, params.limit) : content };
    }
    if (req.method === METHOD.fsWriteTextFile) {
      const params = req.params as { path: string; content: string };
      await fs.writeFile(params.path, params.content, "utf8");
      return null;
    }
    if (req.method === METHOD.permissionRequest) {
      // Auto-approve for the spike. Real cuartel surfaces this in the UI.
      const params = req.params as { options?: Array<{ optionId: string }> };
      const allow =
        params.options?.find((o) => o.optionId.includes("allow")) ?? params.options?.[0];
      return { outcome: { outcome: "selected", optionId: allow?.optionId ?? "allow_once" } };
    }
    throw new Error(`unimplemented server method: ${req.method}`);
  };

  const client = new AcpClient({
    command: "npx",
    args: ["claude-code-acp"],
    cwd: repoRoot,
    verbose,
    onServerRequest,
  });

  client.on("notification", (n) => {
    notificationCount++;
    if ((n.params as { update?: { sessionUpdate?: string } })?.update?.sessionUpdate?.includes("tool_call")) {
      toolCallCount++;
    }
  });

  try {
    // 1. Initialize.
    const initParams: InitializeParams = {
      protocolVersion: 1,
      clientCapabilities: {
        fs: { readTextFile: true, writeTextFile: true },
        terminal: false,
      },
    };
    await client.request(METHOD.initialize, initParams);

    // 2. New session.
    const newSessionParams: NewSessionParams = {
      cwd: repoRoot,
      mcpServers: [],
    };
    const session = (await client.request(
      METHOD.newSession,
      newSessionParams,
    )) as { sessionId: string };

    // 3. Send the prompt.
    const promptParams: PromptParams = {
      sessionId: session.sessionId,
      prompt: [{ type: "text", text: TEST_PROMPT }],
    };
    const result = (await client.request(METHOD.prompt, promptParams)) as PromptResult;

    const durationMs = Date.now() - start;
    return {
      ok: result.stopReason === "end_turn",
      durationMs,
      stopReason: result.stopReason,
      notificationCount,
      toolCallCount,
    };
  } catch (err) {
    return {
      ok: false,
      durationMs: Date.now() - start,
      error: err instanceof Error ? err.message : String(err),
      notificationCount,
      toolCallCount,
    };
  } finally {
    await client.dispose();
  }
}

// Run when invoked directly.
if (import.meta.url === `file://${process.argv[1]}`) {
  console.log(`Workspace: ${repoRoot}`);
  console.log(`Prompt:    ${TEST_PROMPT}`);
  console.log("---");
  const result = await runOnce(true);
  console.log("---");
  console.log("Result:", JSON.stringify(result, null, 2));
  process.exit(result.ok ? 0 : 1);
}
