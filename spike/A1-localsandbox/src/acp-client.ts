/**
 * Minimal hand-rolled ACP (Agent Client Protocol) client.
 *
 * Spawns an ACP server (e.g. `claude-code-acp`) as a subprocess and
 * speaks JSON-RPC line-delimited over its stdio. Verbose by default —
 * every line in/out is logged so the spike's "what does the wire actually
 * look like?" question is answered just by running it.
 *
 * Throwaway code. Production cuartel-acp will be Rust against the
 * `agent-client-protocol` crate (Phase B1). This file exists only to
 * answer one question: does claude-code-acp hang when run as a plain
 * Node OS process (no V8 isolate)?
 */
import { spawn, ChildProcess } from "node:child_process";
import { EventEmitter } from "node:events";
import readline from "node:readline";

export interface JsonRpcRequest {
  jsonrpc: "2.0";
  id: number;
  method: string;
  params?: unknown;
}

export interface JsonRpcResponse {
  jsonrpc: "2.0";
  id: number;
  result?: unknown;
  error?: { code: number; message: string; data?: unknown };
}

export interface JsonRpcNotification {
  jsonrpc: "2.0";
  method: string;
  params?: unknown;
}

export interface JsonRpcServerRequest {
  jsonrpc: "2.0";
  id: number | string;
  method: string;
  params?: unknown;
}

export interface AcpClientOptions {
  command: string; // e.g. "npx" or absolute path to claude-code-acp binary
  args: string[]; // e.g. ["claude-code-acp"]
  cwd: string; // working directory for the spawned process
  env?: NodeJS.ProcessEnv;
  verbose?: boolean; // log every wire line; default true
  onServerRequest?: (req: JsonRpcServerRequest) => Promise<unknown> | unknown;
}

export class AcpClient extends EventEmitter {
  private child: ChildProcess;
  private rl: readline.Interface;
  private nextId = 1;
  private pending = new Map<
    number,
    { resolve: (v: unknown) => void; reject: (e: Error) => void }
  >();
  private verbose: boolean;
  private onServerRequest: AcpClientOptions["onServerRequest"];

  constructor(opts: AcpClientOptions) {
    super();
    this.verbose = opts.verbose ?? true;
    this.onServerRequest = opts.onServerRequest;

    this.child = spawn(opts.command, opts.args, {
      cwd: opts.cwd,
      env: opts.env ?? process.env,
      stdio: ["pipe", "pipe", "pipe"],
    });

    this.rl = readline.createInterface({ input: this.child.stdout! });
    this.rl.on("line", (line) => this.onLine(line));

    this.child.stderr!.on("data", (chunk) => {
      if (this.verbose) {
        process.stderr.write(`[acp-stderr] ${chunk}`);
      }
      this.emit("stderr", chunk);
    });

    this.child.on("exit", (code, signal) => {
      this.emit("exit", { code, signal });
      // Reject any pending requests on early exit.
      for (const { reject } of this.pending.values()) {
        reject(
          new Error(
            `ACP server exited (code=${code}, signal=${signal}) with pending requests`,
          ),
        );
      }
      this.pending.clear();
    });

    this.child.on("error", (err) => this.emit("error", err));
  }

  private onLine(line: string): void {
    if (line.trim() === "") return;

    if (this.verbose) {
      console.log(`← ${line}`);
    }

    let msg: unknown;
    try {
      msg = JSON.parse(line);
    } catch {
      // Non-JSON output (shouldn't happen with a clean ACP server, but log).
      if (this.verbose) console.log(`  (non-JSON line ignored)`);
      return;
    }

    if (typeof msg !== "object" || msg === null) return;
    const m = msg as Record<string, unknown>;

    // Response to a client request: has `id` and `result`/`error`, no `method`.
    if ("id" in m && !("method" in m)) {
      const id = m.id as number;
      const pending = this.pending.get(id);
      if (!pending) {
        if (this.verbose) console.log(`  (response for unknown id ${id})`);
        return;
      }
      this.pending.delete(id);
      if ("error" in m && m.error) {
        const err = m.error as { message?: string };
        pending.reject(new Error(err.message ?? "ACP error"));
      } else {
        pending.resolve(m.result);
      }
      return;
    }

    // Server-initiated request: has `id` AND `method` (e.g. fs/read_text_file,
    // session/permission_request).
    if ("id" in m && "method" in m) {
      void this.handleServerRequest(m as unknown as JsonRpcServerRequest);
      return;
    }

    // Notification: has `method`, no `id` (streaming updates).
    if ("method" in m && !("id" in m)) {
      this.emit("notification", m as JsonRpcNotification);
      return;
    }
  }

  private async handleServerRequest(req: JsonRpcServerRequest): Promise<void> {
    let result: unknown;
    let error: { code: number; message: string } | undefined;

    if (this.onServerRequest) {
      try {
        result = await this.onServerRequest(req);
      } catch (e) {
        error = {
          code: -32000,
          message: e instanceof Error ? e.message : String(e),
        };
      }
    } else {
      // Default: refuse with method-not-found.
      error = { code: -32601, message: `Method ${req.method} not implemented` };
    }

    const response: JsonRpcResponse = error
      ? { jsonrpc: "2.0", id: req.id as number, error }
      : { jsonrpc: "2.0", id: req.id as number, result };
    this.send(response);
  }

  private send(obj: unknown): void {
    const line = JSON.stringify(obj) + "\n";
    if (this.verbose) console.log(`→ ${line.trim()}`);
    this.child.stdin!.write(line);
  }

  /**
   * Send a JSON-RPC request and await its response.
   * Throws if the ACP server returns an error or exits before responding.
   */
  async request(method: string, params?: unknown): Promise<unknown> {
    const id = this.nextId++;
    const req: JsonRpcRequest = { jsonrpc: "2.0", id, method, params };
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.send(req);
    });
  }

  /**
   * Send a JSON-RPC notification (no response expected).
   */
  notify(method: string, params?: unknown): void {
    const note: JsonRpcNotification = { jsonrpc: "2.0", method, params };
    this.send(note);
  }

  /**
   * Kill the child process and wait for exit.
   */
  async dispose(): Promise<void> {
    if (this.child.killed || this.child.exitCode !== null) return;
    this.child.kill("SIGTERM");
    await new Promise<void>((resolve) => {
      const timer = setTimeout(() => {
        this.child.kill("SIGKILL");
        resolve();
      }, 2000);
      this.child.once("exit", () => {
        clearTimeout(timer);
        resolve();
      });
    });
  }

  get pid(): number | undefined {
    return this.child.pid;
  }
}
