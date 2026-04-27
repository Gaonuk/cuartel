# Cuartel Architecture Refactor: Harness Separate From Compute

## TL;DR

We currently nest the **harness** (agent loop + API calls) inside the **compute** (secure-exec sandbox). OpenAI's public guidance (and every other major agent stack) inverts this: the harness runs in a trusted environment and only delegates *execution* (shell / fs / code edits) into an untrusted sandbox. Moving the boundary gives us: (1) the sendPrompt hang disappears because the Claude CLI no longer lives inside a V8 isolate, (2) provider-agnostic design falls out for free — we can swap rivet for E2B / Modal / Daytona / Vercel Sandbox without rewriting the harness, and swap Claude for Pi / Codex / OpenCode without rewriting the sandbox.

---

## Today

```
┌──────────────────────────────────────────────────────────────────┐
│ Cuartel GPUI (Rust — trusted)                                    │
│   session_host.rs  ──HTTP──►  rivet sidecar :6420                │
│   credential_store                                               │
└──────────────────────────────────────────────────────────────────┘
                                     │
                                     ▼
┌──────────────────────────────────────────────────────────────────┐
│ Auth Gateway :6421 (Rust, trusted)                               │
│   - strips dummy key, injects real ANTHROPIC_API_KEY             │
└──────────────────────────────────────────────────────────────────┘
                ▲ loopbackExemptPorts: [6421]
                │ (the hole we punched back out)
┌───────────────┼──────────────────────────────────────────────────┐
│ Rivet sidecar (Node, untrusted execution env)                    │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │ AgentOs secure-exec V8 sandbox                             │  │
│  │                                                            │  │
│  │  ┌──────────────────────────────────────────────────────┐  │  │
│  │  │ Claude ACP adapter (inside sandbox!)                 │  │  │
│  │  │   - patches claude-agent-sdk at runtime              │  │  │
│  │  │   - spawns child V8 isolate running the CLI          │  │  │
│  │  │                                                      │  │  │
│  │  │   ┌────────────────────────────────────────────┐     │  │  │
│  │  │   │ Claude CLI (grandchild V8 isolate)         │─────┼──┼──┘
│  │  │   │   tries to HTTP-POST api.anthropic.com     │     │  │
│  │  │   └────────────────────────────────────────────┘     │  │
│  │  └──────────────────────────────────────────────────────┘  │
│  │                                                            │  │
│  │  Pi adapter, OpenCode adapter, ... (same fate awaits)      │  │
│  │  Bash / Read / Write / Edit / Grep — executed in-sandbox   │  │
│  └────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

**Who holds what:**

| Responsibility         | Location                | Problem                                              |
|------------------------|-------------------------|------------------------------------------------------|
| Agent loop             | inside V8 sandbox       | trusted logic in an untrusted box                    |
| Anthropic API calls    | inside V8 sandbox       | needs SSRF exemption + network polyfills to work     |
| API credentials        | pass through sandbox    | visible to any adapter that can read `process.env`   |
| Tool execution (bash)  | inside V8 sandbox       | correct — this is what the sandbox is for            |
| Provider choice        | baked into rivet        | adding a harness means editing rivet/agent-os-core   |
| Sandbox choice         | baked into rivet        | no way to swap in E2B or Modal without a rewrite     |

**Why sendPrompt hangs:** the Claude CLI is a child V8 isolate trying to do real HTTPS — it fights the sandbox's network adapter, HTTP polyfill, child-process bridge, and stdin bridge all at once. Even with every fix in place, we're one missing Node polyfill away from another silent hang.

---

## Target

```
┌──────────────────────────────────────────────────────────────────┐
│ Cuartel GPUI (Rust — trusted harness layer)                      │
│                                                                  │
│   ┌──────────────────────────────────────────────────────────┐   │
│   │ HARNESS (Claude / Pi / Codex / OpenCode / ...)           │   │
│   │   - agent loop                                           │   │
│   │   - model API calls (direct to api.anthropic.com)        │   │
│   │   - holds secrets                                        │   │
│   │   - receives tool calls from the model,                  │   │
│   │     forwards them to SANDBOX below                       │   │
│   └─────────────────┬────────────────────────────────────────┘   │
│                     │                                            │
│   trait Harness ────┘           trait ComputeSandbox ────┐       │
│                                                          │       │
└──────────────────────────────────────────────────────────┼───────┘
                                                           │ tool-RPC
                                     ┌─────────────────────┼─────┐
                                     ▼                     ▼     ▼
                           ┌──────────────┐    ┌──────────────┐   ┌────────┐
                           │ RivetSandbox │    │ E2BSandbox   │   │ Local  │
                           │ (secure-exec)│    │ (E2B cloud)  │   │ (dev)  │
                           └──────────────┘    └──────────────┘   └────────┘
                           ┌──────────────┐    ┌──────────────┐
                           │ ModalSandbox │    │ DaytonaSandb.│   ...
                           └──────────────┘    └──────────────┘

   Bash, Read, Write, Edit, Grep, Glob execute inside whichever
   ComputeSandbox is plugged in. NO credentials. NO outbound internet
   unless the harness explicitly exposes a URL to it.
```

**Who holds what (proposed):**

| Responsibility         | Location                             | Provider-agnostic?       |
|------------------------|--------------------------------------|--------------------------|
| Agent loop             | Harness trait impl (on host)         | yes — one impl per model |
| Model API calls        | Harness impl (host, real internet)   | yes                      |
| API credentials        | `credential_store`, never leave host | yes                      |
| Tool execution (bash)  | ComputeSandbox trait impl            | yes — rivet/E2B/Modal/…  |
| File edits             | ComputeSandbox trait impl            | yes                      |
| Network egress policy  | host firewall + per-tool allowlist   | yes                      |

---

## What moves where

| Thing today                                          | Where it lives now                | Where it goes                                |
|------------------------------------------------------|-----------------------------------|----------------------------------------------|
| Claude ACP adapter (`@rivet-dev/agent-os-claude`)    | inside secure-exec V8 sandbox     | host Node process (or Rust via FFI)          |
| Claude CLI child process                             | grandchild V8 isolate             | host child process (real `node` subprocess)  |
| `claude-agent-sdk` minified-symbol patch             | `rivet/server.ts`                 | DELETED — not needed off-sandbox             |
| `loopbackExemptPorts: [6421]`                        | `rivet/server.ts`                 | DELETED — harness is on host                 |
| `GATEWAY_PORT = 6421` + `CUARTEL_LOOPBACK_EXEMPT_PORT` | `crates/cuartel-app/src/main.rs` | DELETED (or kept as optional DIDH layer)     |
| `ANTHROPIC_API_KEY` / `ANTHROPIC_BASE_URL` injection | `build_sidecar_env`               | used only by the host-side `ClaudeHarness`   |
| Tool calls (Bash, Read, Write, Edit, Grep)           | handled inside rivet VM           | dispatched from harness through `ComputeSandbox` trait |
| Pi adapter, OpenCode, Codex                          | rivet/agent-os-core has them      | each gets a host-side `Harness` impl         |

---

## New abstractions

### `trait Harness` (trusted, per-model)

```rust
// crates/cuartel-harness/src/lib.rs
#[async_trait]
pub trait Harness: Send + Sync {
    async fn start_session(
        &self,
        cwd: PathBuf,
        sandbox: Arc<dyn ComputeSandbox>,
    ) -> Result<SessionHandle>;

    async fn send_prompt(
        &self,
        session: &SessionHandle,
        text: &str,
    ) -> Result<PromptStream>;

    async fn cancel(&self, session: &SessionHandle) -> Result<()>;
    async fn destroy(&self, session: SessionHandle) -> Result<()>;
}
```

Implementations:
- `ClaudeHarness` — wraps `claude-agent-sdk` running on the host.
- `PiHarness` — wraps Pi's HTTP API directly.
- `CodexHarness` — wraps OpenAI's codex-cli (itself runs on host).
- `OpenCodeHarness` — etc.

Each harness receives a `Arc<dyn ComputeSandbox>` and wires the model's tool calls into it.

### `trait ComputeSandbox` (untrusted, per-provider)

```rust
// crates/cuartel-sandbox/src/lib.rs
#[async_trait]
pub trait ComputeSandbox: Send + Sync {
    async fn exec(&self, cmd: ShellCommand) -> Result<ExecResult>;
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>>;
    async fn write_file(&self, path: &Path, data: &[u8]) -> Result<()>;
    async fn edit_file(&self, path: &Path, old: &str, new: &str) -> Result<()>;
    async fn glob(&self, pattern: &str) -> Result<Vec<PathBuf>>;
    async fn grep(&self, pattern: &str, path: &Path) -> Result<Vec<GrepMatch>>;
    async fn dispose(&self) -> Result<()>;
}
```

Implementations:
- `RivetSandbox` — keeps most of what's in `crates/cuartel-rivet` today, but exposes the trait instead of the full ACP lifecycle.
- `LocalSandbox` — direct `tokio::process::Command` on host, for dev.
- `E2BSandbox` — calls E2B's HTTP API.
- `ModalSandbox`, `DaytonaSandbox`, `VercelSandbox`, `CloudflareSandbox` — same pattern.

---

## Concrete code changes

### New crates

```
crates/
├── cuartel-harness/       # NEW — trait + impls
│   ├── src/lib.rs         #   trait Harness, SessionHandle, PromptStream
│   ├── src/claude.rs      #   ClaudeHarness: wraps claude-agent-sdk (host-side)
│   ├── src/pi.rs          #   PiHarness
│   └── src/codex.rs       #   (future)
└── cuartel-sandbox/       # NEW — trait + impls
    ├── src/lib.rs         #   trait ComputeSandbox
    ├── src/rivet.rs       #   RivetSandbox (replaces cuartel-rivet's session API)
    ├── src/local.rs       #   LocalSandbox (dev)
    └── src/e2b.rs         #   (future)
```

### Modified

| File                                     | Change                                                               |
|------------------------------------------|----------------------------------------------------------------------|
| `crates/cuartel-app/src/session_host.rs` | Use `Harness + ComputeSandbox` instead of `cuartel_rivet::Client::create_session` |
| `crates/cuartel-app/src/main.rs`         | Drop `GATEWAY_PORT`, `CUARTEL_LOOPBACK_EXEMPT_PORT`, Claude-specific env rewriting |
| `crates/cuartel-rivet/src/client.rs`     | Shrink to bash/fs/exec endpoints; session lifecycle moves to harness |
| `rivet/server.ts`                        | Drop SDK patch, drop `loopbackExemptPorts`, keep only tool surface   |

### Deleted

- The entire `patchClaudeSdkAbortSignalGuard()` helper.
- `CUARTEL_LOOPBACK_EXEMPT_PORT` plumbing across Rust + TS.
- Claude-specific branches in `build_sidecar_env` (harness holds its own env).

### Kept (optional)

- **Auth gateway** stays as defense-in-depth. With the harness on the host, secrets never have to leave Rust anyway — but routing API traffic through the gateway still buys you audit logs and per-request egress policy. It just becomes opt-in, not load-bearing.

---

## Provider matrix (what the target makes possible)

|              | Rivet/secure-exec | E2B | Modal | Daytona | Vercel Sandbox | Local (dev) |
|--------------|-------------------|-----|-------|---------|----------------|-------------|
| Claude       | ✓                 | ✓   | ✓     | ✓       | ✓              | ✓           |
| Pi           | ✓                 | ✓   | ✓     | ✓       | ✓              | ✓           |
| Codex (OAI)  | ✓                 | ✓   | ✓     | ✓       | ✓              | ✓           |
| OpenCode     | ✓                 | ✓   | ✓     | ✓       | ✓              | ✓           |

Any cell is a combination of one `Harness` impl + one `ComputeSandbox` impl. Adding a new model is one file in `cuartel-harness`. Adding a new sandbox vendor is one file in `cuartel-sandbox`. Nothing in cuartel's UI or `session_host.rs` changes.

---

## Migration plan (suggested order)

1. **Define the traits** (`cuartel-harness`, `cuartel-sandbox`) with no impls — just compile.
2. **`ClaudeHarness` (host-side)** against `claude-agent-sdk` directly — skip rivet entirely. Wire it behind a feature flag so both paths coexist while debugging.
3. **`LocalSandbox`** — dumb `tokio::process` runner. Gives us a harness that works end-to-end with zero sandboxing, proving the trait shape.
4. **`RivetSandbox`** — wrap the subset of `cuartel-rivet` that does `exec` / fs / glob. Drop the `createSession` / `sendPrompt` bits.
5. **Cut over `session_host.rs`** to use the trait pair. Delete gateway/loopback plumbing.
6. **Port Pi**, then add E2B/Modal on the sandbox side as appetite allows.

Step 2 alone unblocks the sendPrompt hang, because Claude stops running inside a V8 isolate. Everything after is strictly product upside.
