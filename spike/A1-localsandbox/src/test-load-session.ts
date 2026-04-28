/**
 * Secondary test: does claude-code-acp support ACP's `session/load`?
 *
 * Resolves open question 3 in ARCHITECTURE_REFACTOR_V2.md. The answer
 * affects how Phase D (HetznerSandbox + workspace move) handles session
 * resume after VM restart.
 *
 * Approach:
 *   Run 1: spawn → initialize → session/new → prompt "remember the word
 *          'chartreuse'" → record sessionId → exit
 *   Run 2: spawn → initialize → session/load(recorded sessionId) → prompt
 *          "what word did I ask you to remember?" → check if response
 *          mentions chartreuse
 *
 * If Run 2 mentions "chartreuse", session resume works. If it doesn't, or
 * if `session/load` returns an error, claude-code-acp doesn't (yet)
 * support resume — Phase D needs a fallback strategy (e.g. replay
 * messages on the host side).
 */
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promises as fs } from "node:fs";
import { AcpClient } from "./acp-client.ts";
import {
  METHOD,
  type InitializeParams,
  type NewSessionParams,
  type LoadSessionParams,
  type PromptParams,
  type JsonRpcServerRequest,
} from "./protocol.ts";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "../../..");

const MEMORY_WORD = "chartreuse";

async function readPermissive(req: JsonRpcServerRequest): Promise<unknown> {
  if (req.method === METHOD.fsReadTextFile) {
    const params = req.params as { path: string; limit?: number };
    const content = await fs.readFile(params.path, "utf8");
    return { content: params.limit ? content.slice(0, params.limit) : content };
  }
  if (req.method === METHOD.permissionRequest) {
    return { outcome: { outcome: "selected", optionId: "allow_once" } };
  }
  throw new Error(`unimplemented: ${req.method}`);
}

interface PromptResultWithText {
  stopReason: string;
  responseText: string;
}

async function runPromptAndCaptureText(
  client: AcpClient,
  sessionId: string,
  text: string,
): Promise<PromptResultWithText> {
  let captured = "";
  const noteHandler = (n: { params?: unknown }) => {
    const params = n.params as
      | { update?: { sessionUpdate?: string; content?: { text?: string } } }
      | undefined;
    const upd = params?.update;
    if (upd?.sessionUpdate === "agent_message_chunk" && upd.content?.text) {
      captured += upd.content.text;
    }
  };
  client.on("notification", noteHandler);

  const params: PromptParams = {
    sessionId,
    prompt: [{ type: "text", text }],
  };
  const result = (await client.request(METHOD.prompt, params)) as { stopReason: string };

  client.off("notification", noteHandler);
  return { stopReason: result.stopReason, responseText: captured };
}

async function newClient(): Promise<AcpClient> {
  const client = new AcpClient({
    command: "npx",
    args: ["claude-code-acp"],
    cwd: repoRoot,
    verbose: true,
    onServerRequest: readPermissive,
  });

  const initParams: InitializeParams = {
    protocolVersion: 1,
    clientCapabilities: {
      fs: { readTextFile: true, writeTextFile: false },
      terminal: false,
    },
  };
  await client.request(METHOD.initialize, initParams);
  return client;
}

async function main() {
  console.log("Run 1: create a session with a memory cue.");
  console.log("---");

  const client1 = await newClient();
  const newParams: NewSessionParams = { cwd: repoRoot, mcpServers: [] };
  const session = (await client1.request(METHOD.newSession, newParams)) as {
    sessionId: string;
  };
  console.log(`Got sessionId: ${session.sessionId}`);

  const r1 = await runPromptAndCaptureText(
    client1,
    session.sessionId,
    `Remember this word for me: "${MEMORY_WORD}". Just acknowledge with "ok".`,
  );
  console.log(`Run 1 response: ${r1.responseText.trim() || "(empty)"}`);
  await client1.dispose();

  console.log("");
  console.log("Run 2: try to resume that session and recall.");
  console.log("---");

  const client2 = await newClient();
  let resumeOk = false;
  try {
    const loadParams: LoadSessionParams = {
      sessionId: session.sessionId,
      cwd: repoRoot,
      mcpServers: [],
    };
    await client2.request(METHOD.loadSession, loadParams);
    resumeOk = true;
    console.log(`session/load succeeded for ${session.sessionId}`);
  } catch (e) {
    console.log(`session/load FAILED: ${e instanceof Error ? e.message : e}`);
    console.log("→ claude-code-acp does not support resume in this version.");
  }

  if (resumeOk) {
    const r2 = await runPromptAndCaptureText(
      client2,
      session.sessionId,
      "What word did I ask you to remember? Reply with just the word.",
    );
    console.log(`Run 2 response: ${r2.responseText.trim() || "(empty)"}`);
    const remembered = r2.responseText.toLowerCase().includes(MEMORY_WORD);
    console.log("");
    console.log(remembered ? "✓ RESUME WORKS: agent recalled the word." : "✗ RESUME PARTIAL: session/load returned ok but agent did not recall.");
  } else {
    console.log("");
    console.log("✗ RESUME UNSUPPORTED: Phase D needs a fallback (e.g. replay messages on host).");
  }

  await client2.dispose();
}

main().catch((e) => {
  console.error("Fatal:", e);
  process.exit(1);
});
