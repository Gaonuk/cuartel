/**
 * ACP method names and parameter shapes used by the spike.
 *
 * Sourced from the Agent Client Protocol spec (agentclientprotocol.com)
 * and Zed's reference implementation in `crates/agent_servers/src/acp.rs`.
 *
 * These constants are kept in one place so when the spike runs and shows
 * the actual wire traffic, it's obvious whether claude-code-acp's
 * implementation matches our assumptions. If method names or param
 * shapes differ, fix them here and re-run.
 */

export const METHOD = {
  // Client → Server: lifecycle.
  initialize: "initialize",
  newSession: "session/new",
  loadSession: "session/load",
  prompt: "session/prompt",
  cancel: "session/cancel",

  // Server → Client: requests the client must implement.
  fsReadTextFile: "fs/read_text_file",
  fsWriteTextFile: "fs/write_text_file",
  permissionRequest: "session/request_permission",

  // Server → Client: notifications (streaming).
  sessionUpdate: "session/update",
} as const;

/** Parameters for `initialize`. Sent first, before any session work. */
export interface InitializeParams {
  protocolVersion: number;
  clientCapabilities: {
    fs?: { readTextFile?: boolean; writeTextFile?: boolean };
    terminal?: boolean;
  };
}

/** Parameters for `session/new`. */
export interface NewSessionParams {
  cwd: string;
  mcpServers: McpServerConfig[];
}

export interface McpServerConfig {
  name: string;
  command: string;
  args?: string[];
  env?: Record<string, string>;
}

/** Parameters for `session/load`. */
export interface LoadSessionParams {
  sessionId: string;
  cwd: string;
  mcpServers: McpServerConfig[];
}

/** Parameters for `session/prompt`. */
export interface PromptParams {
  sessionId: string;
  prompt: ContentBlock[];
}

/** ACP content blocks — text is the simplest case. */
export type ContentBlock =
  | { type: "text"; text: string }
  | { type: "image"; mimeType: string; data: string }
  | { type: "resource"; resource: { uri: string; mimeType?: string } };

/** Result shape returned by `session/prompt` once the turn completes. */
export interface PromptResult {
  stopReason: "end_turn" | "max_tokens" | "max_turn_requests" | "refusal" | "cancelled";
}
