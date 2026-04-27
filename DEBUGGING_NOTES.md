# Cuartel - Claude Code Session Debugging Notes

**Date:** 2026-04-17
**Status:** Partially fixed. `createSession` works. `sendPrompt` still hangs.

---

## What Works

1. **createSession** - Claude Code ACP session creation succeeds after patching the SDK's `_j` function (see Fix #1 below)
2. **SSRF loopback exemption** - Verified that `loopbackExemptPorts: [6421]` correctly bypasses the secure-exec sandbox's SSRF check. A direct `http.request` from inside the VM to `127.0.0.1:6421` returns `ECONNREFUSED` (not `SSRF blocked`), confirming the exemption works.
3. **Auth gateway** - Binds on fixed port 6421, proxies requests to `api.anthropic.com` with real credential injection.
4. **SDK direct test** - Running `@anthropic-ai/claude-agent-sdk`'s `query()` OUTSIDE the sandbox with a real API key returns 11 messages in ~15 seconds. The SDK itself works fine.

---

## What Doesn't Work

**sendPrompt hangs for 120s then times out.** The ACP `session/prompt` request (id=3) never gets a response from the claude adapter running inside the secure-exec V8 sandbox.

ACP activity log: `initialize ✓ → session/new ✓ → session/prompt ✗ (timeout)`

---

## Fixes Applied So Far

### Fix #1: Patch `claude-agent-sdk` AbortSignal guard
**File:** `rivet/server.ts` — `patchClaudeSdkAbortSignalGuard()`
**Problem:** SDK v0.2.112+ minifies `setMaxListeners` as `_j`. Inside the secure-exec VM, the `events` module import can be undefined, causing `_j is not a function` during `session/new`.
**Fix:** Patch the SDK source on disk before the adapter loads: replace `return _j($,X.signal),X}` with `return typeof _j==="function"&&_j($,X.signal),X}`.
**Status:** ✅ Working

### Fix #2: Fixed gateway port + loopback exemption
**Files:** `crates/cuartel-app/src/main.rs`, `rivet/server.ts`
**Problem:** Gateway was binding on ephemeral port (`127.0.0.1:0`). The port needed to be known before spawning the sidecar so it could be passed as `loopbackExemptPorts`. Also, the sandbox blocks outbound loopback connections (SSRF protection).
**Fix:**
- Gateway now binds on fixed port 6421 (`GATEWAY_PORT = 6421`)
- Port is passed to sidecar env as `CUARTEL_LOOPBACK_EXEMPT_PORT=6421`
- `server.ts` reads this env var and passes `loopbackExemptPorts: [6421]` to `agentOs()`
- Gateway routing re-enabled: `ANTHROPIC_API_KEY=sk-cuartel-gateway`, `ANTHROPIC_BASE_URL=http://127.0.0.1:6421`
**Status:** ✅ Loopback exemption confirmed working (SSRF check passes). But prompt still hangs.

---

## Theories Why sendPrompt Still Hangs

### Theory A: The claude CLI child process doesn't use the sandbox's http polyfill
The claude adapter calls `spawnClaudeCodeProcess` which uses Node's `child_process.spawn("node", [cliPath, ...])`. Inside the sandbox, this goes through `sandbox-command-executor` which routes `node` commands to child V8 isolates. The child V8 isolate DOES get the http polyfill and SSRF exemption. **But** the `claude` CLI (the code at `@anthropic-ai/claude-agent-sdk/cli.js`) might be spawning its OWN child process (the actual `claude` binary) that runs OUTSIDE the V8 sandbox and has no network restrictions at all... or it might be using a different HTTP path.

**Test needed:** Add tracing inside the child isolate to see what the claude CLI is actually doing. Set `CLAUDE_CODE_TRACE_CHILD_IO=1` and `CLAUDE_CODE_TRACE_ADAPTER_MESSAGES=1` in the adapter's env.

### Theory B: The `ANTHROPIC_BASE_URL` env var isn't reaching the claude CLI
The adapter passes `{ ...process.env, CLAUDE_CODE_SIMPLE: "1", ... }` to the query options, and `spawnClaudeCodeProcess` passes `{ ...env, CLAUDE_CODE_SWAP_STDIO: "1" }` to the child process. But inside the sandbox, `process.env` might not contain the injected env vars from `createSession`.

**Test needed:** Inside the child isolate, log `process.env.ANTHROPIC_BASE_URL` and `process.env.ANTHROPIC_API_KEY` to verify they reach the claude CLI.

### Theory C: The claude CLI is hanging on something else (not network)
Maybe the CLI is waiting for stdin, or hitting an infinite loop, or crashing silently. The `process exitCode=null` in the timeout error suggests the process is still running when the timeout hits.

**Test needed:** Check if the claude CLI process is actually alive during the 120s window. Add stderr tracing.

### Theory D: The claude CLI uses `https.request` directly to `api.anthropic.com` (ignoring ANTHROPIC_BASE_URL)
Some versions of the Claude SDK/CLI may not respect `ANTHROPIC_BASE_URL` for all requests, or may have hardcoded URLs.

**Test needed:** Check what URL the claude CLI is actually trying to connect to.

---

## Things To Do Next

### Priority 1: Add adapter tracing
The adapter has built-in trace flags. Pass them in the `launchEnv`:
```
CLAUDE_CODE_TRACE_ADAPTER_MESSAGES=1
CLAUDE_CODE_TRACE_CHILD_IO=1
```
These need to be in the adapter process's `process.env`. Currently they're not being injected because they're not in `sidecar_env`. Add them to `build_sidecar_env()` in `main.rs` when gateway is active.

Alternatively, patch the adapter's env inside `createSession` by passing them in the `options.env`:
```rust
let session_options = Some(json!({
    "env": {
        "CLAUDE_CODE_TRACE_ADAPTER_MESSAGES": "1",
        "CLAUDE_CODE_TRACE_CHILD_IO": "1"
    }
}));
```

### Priority 2: Verify env var propagation
Write a test script that runs INSIDE the sandbox via `agentOs.exec()` and logs `process.env.ANTHROPIC_BASE_URL`. This will confirm whether the env vars from `createSession` options actually reach child processes.

### Priority 3: Test with real API key (no gateway)
Before the gateway bypass was removed, we tried passing the real `ANTHROPIC_API_KEY` directly. That also timed out. This suggests the issue is NOT about the gateway being unreachable — the claude CLI inside the sandbox can't make ANY outbound HTTP request, even to `api.anthropic.com`.

**Test needed:** Inside the sandbox, try `fetch("https://api.anthropic.com")` — if this also fails, the problem is fundamental network access, not SSRF.

### Priority 4: Test the claude adapter outside the sandbox entirely
Run the full ACP flow (initialize → session/new → session/prompt) with the adapter running as a regular Node process (not inside secure-exec). If this works, it confirms the sandbox is the issue.

### Priority 5: Check if the sandbox blocks ALL outbound HTTPS
The SSRF check blocks private IPs but allows public IPs. However, the sandbox might have additional network restrictions. Check if `fetch("https://api.anthropic.com/v1/messages")` works from inside the sandbox.

---

## Possible PRs For Other Repos

### 1. PR for `@rivet-dev/agent-os-claude` (rivet repo)
**Title:** Make `loadPatchedClaudeSdkRuntime` use regex-based patching instead of hardcoded minified names
**Current problem:** The patch matches exact minified symbol names (`ML`) that change across SDK versions. Current SDK has `_j` instead, making the existing patch a no-op.
**Proposed fix:** Replace exact string matching with a regex that targets the structural pattern:
```ts
// Instead of:
const needle = 'function y1($=AL){let X=new AbortController;return ML($,X.signal),X}';

// Use:
const patched = source.replace(
  /return\s+(\w+)\(\$,X\.signal\),X\}/g,
  'return typeof $1==="function"&&$1($,X.signal),X}'
);
```
This survives any SDK update that renames the minified symbols.

### 2. PR for `@rivet-dev/agent-os-claude` (rivet repo)
**Title:** Support running the claude CLI outside the secure-exec sandbox
**Problem:** The claude CLI child process runs inside a sandboxed V8 isolate with restricted network access. The CLI needs to make outbound HTTPS requests to the Anthropic API, which may be blocked.
**Proposed fix:** Add a `spawnClaudeCodeProcess` override that spawns the CLI on the host (outside the sandbox) while keeping the ACP stdio bridge. This could use the host command executor or a direct `child_process.spawn` on the host side.

### 3. PR for `@rivet-dev/agent-os-core` (rivet repo)
**Title:** Pass `loopbackExemptPorts` through the agent-os config schema
**Current state:** `AgentOsOptionsSchema` is `z.custom((val) => typeof val === "object")` — it accepts anything. But `loopbackExemptPorts` is not documented or typed. Making it explicit in the schema with proper TypeScript types would help consumers.

---

## Key Files And Locations

| Component | Path |
|-----------|------|
| SDK patch + loopback config | `rivet/server.ts` |
| Gateway config + env routing | `crates/cuartel-app/src/main.rs` |
| Gateway proxy implementation | `crates/cuartel-core/src/auth_gateway/proxy.rs` |
| Gateway host (lifecycle) | `crates/cuartel-core/src/auth_gateway/host.rs` |
| Gateway rules + fixed port | `crates/cuartel-core/src/auth_gateway/rules.rs` |
| Session host (sendPrompt) | `crates/cuartel-app/src/session_host.rs` |
| Rivet client (HTTP calls) | `crates/cuartel-rivet/src/client.rs` |
| Sidecar spawn + env injection | `crates/cuartel-rivet/src/sidecar.rs` |
| Agent harness definitions | `crates/cuartel-core/src/agent.rs` |
| Credential store + env_for_harness | `crates/cuartel-core/src/credential_store.rs` |
| Event decode (permission/text) | `crates/cuartel-rivet/src/event_decode.rs` |
| SSRF check + loopback exempt | `rivet/node_modules/@secure-exec/nodejs/dist/default-network-adapter.js` |
| Sandbox command executor | `rivet/node_modules/@secure-exec/nodejs/dist/sandbox-command-executor.js` |
| Network bridge (http polyfill) | `rivet/node_modules/@secure-exec/nodejs/dist/bridge/network.js` |
| Claude adapter (ACP) | `rivet/node_modules/@rivet-dev/agent-os-claude/dist/adapter.js` |
| Agent-os-core (createSession) | `rivet/node_modules/@rivet-dev/agent-os-core/dist/agent-os.js` |

---

## Architecture Recap

```
cuartel (Rust/GPUI app)
├── auth gateway (tokio, port 6421)
│   └── proxies api.anthropic.com with real key injection
├── rivet sidecar (Node, port 6420)
│   ├── rivetkit server.ts
│   └── secure-exec V8 sandbox
│       ├── agent-os VM (AgentOs.create)
│       │   ├── Pi adapter (works - uses in-process fetch)
│       │   └── Claude adapter (BROKEN - spawns CLI child process)
│       │       ├── ACP client (initialize ✓, session/new ✓, session/prompt ✗)
│       │       └── claude CLI (child V8 isolate)
│       │           └── http.request → _networkHttpRequestRaw → SSRF check
│       └── loopbackExemptPorts: [6421] (passes SSRF ✓)
└── SessionHost (Rust, calls rivet client)
```

The Pi adapter works because it calls the Anthropic API directly using `fetch()` inside the sandbox, which routes through the network adapter and passes the SSRF check for `api.anthropic.com` (public IP, not blocked).

The Claude adapter fails because its `query()` spawns a child `node` process (the claude CLI) inside a child V8 isolate. That child isolate's HTTP requests to `127.0.0.1:6421` (gateway) pass SSRF (exempted), but the CLI may not be using the sandbox's HTTP polyfill correctly, or it may be trying to connect to `api.anthropic.com` directly (which would also work if the sandbox allows it), or there may be some other issue.

---

## Quick Commands For Testing

```bash
# Run cuartel with logging
RUST_LOG=info cargo run 2>&1 | tee /tmp/cuartel.log

# Test claude SDK directly (outside sandbox)
cd rivet && ANTHROPIC_API_KEY=sk-xxx timeout 60 npx tsx test-claude-direct.mjs

# Test SSRF inside sandbox
cd rivet && CUARTEL_LOOPBACK_EXEMPT_PORT=6421 npx tsx test-ssrf.mts

# Check gateway is running
curl http://127.0.0.1:6421/v1/messages -H "Host: api.anthropic.com" -H "x-api-key: sk-cuartel-gateway"

# Kill everything
pkill -f cuartel; pkill -f "tsx server"
```
