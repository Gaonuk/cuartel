# Cuartel Architecture Refactor v2: ACP in the Sandbox

> **What changed from v1.** v1 proposed a new `trait Harness` in Rust and a per-tool `trait ComputeSandbox` (exec / read_file / grep / edit_file / …). That's the wrong split. ACP (Agent Client Protocol) already exists and every major coding agent speaks it. The right move is to adopt ACP as the harness wire format and put the ACP server **inside the sandbox** where the workspace lives — not on the host. Tool traffic becomes loopback inside the VM. The host↔sandbox boundary carries only ACP messages (turns, tool-call previews, permission prompts), not raw file I/O. The cuartel-defined sandbox trait shrinks to *provisioning only*: spawn VM, mount workspace, exec ACP server, forward stdio.

## TL;DR

- **Adopt ACP** (`agent-client-protocol` / `claude-code-acp` / `gemini-cli`) as the harness protocol. Delete the idea of a cuartel-defined `trait Harness`.
- **ACP server runs inside the sandbox**, co-located with the workspace files. No FS virtualization. No patching upstream SDKs.
- **`trait Sandbox` is provisioning-only**: `provision / spawn_agent / forward_port / snapshot / dispose`. No per-tool RPC.
- **`Workspace` becomes a first-class type** (above sessions) with N worktrees, an access policy, and a `RuntimeLocation = Local | Remote(endpoint)`.
- **Runtime can move** between Local and Remote (Proliferate-style one-click) because the abstraction is the same in both cases — only the transport changes.
- The sendPrompt hang disappears because the Claude CLI runs as a plain OS process in a Linux VM, not inside a V8 isolate pretending to be one.

---

## The two facts behind this design

**Fact 1 — claude-code-acp runs all tools locally to its own Node process.**
`@zed-industries/claude-code-acp` imports `query()` from `@anthropic-ai/claude-agent-sdk` and passes a `cwd` — every Read/Write/Edit/Bash/Glob/Grep call is executed by the bundled Claude CLI against that cwd's filesystem (`acp-agent.ts:1673`). The `fs/read_text_file` / `fs/write_text_file` ACP RPCs exist (`acp-agent.ts:1234`) but the SDK never calls them. Virtualizing the FS would require forking the SDK — ongoing pain.

**Fact 2 — Zed already has the remote-spawn pattern.**
`zed/crates/agent_servers/src/acp.rs:505–528` transforms the ACP launch command via `project.remote_client().build_command_with_options(..., Interactive::No)` when the project is remote SSH. Stdio stays local; the agent process runs on the remote host. For remote Zed projects, Zed also becomes the FS middleman using its buffer-sync infrastructure (`acp_thread.rs:2601`) — we don't have that and shouldn't build it.

Conclusion: put the ACP server where the files are. Keep the wire boundary clean.

---

## Sandbox kind: V8 isolate ≠ real OS

The word "sandbox" hides a critical distinction. There are two kinds, and only one of them solves our problem:

| Sandbox kind | What runs the agent | sendPrompt hang? |
|---|---|---|
| **V8 isolate** (Rivet AgentOS secure-exec, Cloudflare Workers, Deno isolate, …) | Claude CLI as nested V8 child fighting polyfilled `net` / `child_process` / `fs` / TLS | **Yes — same hang as today.** |
| **Real OS** (Linux VM, microVM, container) | Claude CLI as a plain `node` OS process with real syscalls | **No.** |

The current sendPrompt hang is **not** caused by *where* the adapter file lives — it's caused by *what kind of runtime* the Claude CLI is executing inside. Lifting claude-code-acp from one V8 isolate and dropping it into another solves nothing.

**This refactor requires a real OS sandbox.** Concretely:

| Topology | Sandbox tech | Status |
|---|---|---|
| Local (laptop) | **Apple Virtualization.framework** — Linux VM | `com.apple.security.virtualization` entitlement is **already declared** in `entitlements.plist`. Someone planned for this. |
| Remote (Hetzner) | **Firecracker / Cloud Hypervisor / plain KVM VM** | Trivial on a Linux host. |
| Dev / CI | **Local container or process group** (`LocalSandbox`) | No isolation; for the trait shape only. |

### Where does Rivet fit then?

Rivet has two products that get conflated:

- **AgentOS secure-exec** — the V8 sandbox that's causing the hang. **Drop this dependency for the agent runtime.** It is the wrong primitive for executing third-party CLIs that expect Node.
- **Actors** — durable stateful processes / control-plane primitives. Potentially useful for VM lifecycle, networking, port forwarding. **Not load-bearing for this refactor.**

Plan as if **Rivet ships nothing new**. Their VM-backed sandboxes are roadmap; the whole point of this refactor is to stop being blocked by upstream timelines. If/when Rivet ships real VM sandboxes, they slot in as another `Sandbox` trait impl alongside Apple-VZ and Firecracker — no architecture change.

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
└──────────────────────────────────────────────────────────────────┘
                ▲ loopbackExemptPorts: [6421]
┌───────────────┼──────────────────────────────────────────────────┐
│ Rivet sidecar (Node, untrusted)                                  │
│   ┌──────────────────────────────────────────────────────────┐   │
│   │ AgentOs secure-exec V8 sandbox                           │   │
│   │   Claude ACP adapter (in sandbox)                        │   │
│   │   └─ Claude CLI (grandchild V8 isolate, wants real HTTP) │   │
│   │      ├─ patches claude-agent-sdk at runtime              │   │
│   │      └─ fights sandbox's HTTP polyfill → sendPrompt hang │   │
│   └──────────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────────┘
```

Problems (same as v1):

- Claude CLI is a V8 grandchild doing "real" HTTPS inside a V8 sandbox. One missing polyfill = silent hang.
- Adding a new model or swapping a sandbox vendor means editing `rivet/agent-os-core`.
- Credentials flow into the sandbox so the gateway is load-bearing.

---

## Target

```
┌──────────────────────────────────────────────────────────────────┐
│ Cuartel GPUI (Rust — trusted UI)                                 │
│                                                                  │
│   Workspace registry   ─── N worktrees, access policy,           │
│                            runtime location (Local|Remote)       │
│   Thread / session store (SQLite, Zed-style)                     │
│   ACP client (one connection per session)                        │
│   Permission UI, notification windows, sidebar                   │
│   Auth gateway (optional, defense-in-depth)                      │
└──────────────────────────────────────────────────────────────────┘
                       ▲ ACP wire (JSON-RPC, framed)
                       │ Transport: local stdio │ stdio-over-SSH │ framed-TCP over Tailscale
                       ▼
┌──────────────────────────────────────────────────────────────────┐
│ Sandbox (untrusted, REAL OS — Linux VM, not a V8 isolate)        │
│   Runtime: Apple-VZ (local) │ Hetzner Firecracker (remote) │ …   │
│                                                                  │
│   ┌──────────────────────────────────────────────────────────┐   │
│   │ ACP server (claude-code-acp │ gemini-cli │ codex-acp)    │   │
│   │   spawns model CLI, tools execute against /workspace     │   │
│   │   MCP servers spawn inside sandbox                       │   │
│   └──────────────────────────────────────────────────────────┘   │
│                                                                  │
│   /workspace ← mounted/cloned project files (N repos possible)   │
│   Outbound network: firewalled allowlist (phase 5f)              │
└──────────────────────────────────────────────────────────────────┘
```

Key property: **tool calls never cross the host↔sandbox boundary.** They're loopback inside the VM. The only traffic crossing is ACP messages — bounded, structured, auditable.

---

## Three axes, not two

v1 had two axes (model × sandbox). v2 makes a third one explicit:

| Axis | Values | What picks it |
|---|---|---|
| **Model** | Claude / Gemini / Codex / Pi / OpenCode | `AgentServerCommand` — binary + args + env |
| **Sandbox** | Rivet / E2B / Modal / Daytona / Local | `trait Sandbox` impl |
| **Runtime location** | Local (laptop) / Remote (Hetzner over Tailscale) | `RuntimeLocation` on `Workspace` |

All three axes are independent. "Claude + Rivet + Local" is my laptop dev mode. "Claude + Hetzner-VM + Remote" is a co-located long-running session surviving laptop sleep. "Gemini + E2B + Remote" is the same UI, same code, different config.

---

## Decisions made

Promoted out of "open questions" once research consensus + recommendation lined up. Each is the load-bearing call for v2 — if you disagree, raise it before implementation, not after.

### Architectural decisions

**D1. Adopt ACP (Agent Client Protocol).** No cuartel-defined `trait Harness`. cuartel-app is an ACP client; ACP servers (`claude-code-acp`, `gemini-cli`, future ACP-native agents) are spawned subprocesses. Validated by Zed (`crates/agent_servers`), Paseo (`agents.providers extends: "acp"`), Anthropic Managed Agents direction. KB §3 D1.

**D2. ACP server runs INSIDE the sandbox**, co-located with the workspace. Tool calls are loopback inside the VM. Host↔sandbox traffic is ACP messages only. Pinned by the fact that `claude-code-acp` runs all tools locally to its Node process — virtualizing FS via ACP's `fs/*` RPCs would require forking upstream. KB §3 D2.

**D3. Sandbox is pluggable with two tiers.** *(Updated 2026-04-27 after A1 spike + scope discussion.)*
- **MVP default — `LocalSandbox`:** claude-code-acp as a plain host subprocess. **No isolation.** Same as Zed / Polyscope / Paseo / Cursor — every comparable product ships interactive coding agents this way. The user-in-the-loop permission UI is the safety net for interactive sessions.
- **Secure mode — `AppleVzSandbox` / `HetznerSandbox`:** real Linux VM. Required for **autonomous / scheduled / unattended / remote** work where the user isn't watching each tool call. Ships in Phase D as part of the remote-runtime + scheduled-Replicas surface.
- **What's removed regardless:** AgentOS secure-exec (the V8 isolate that caused the sendPrompt hang). It was never the right primitive for executing third-party Node CLIs.

The original v2 framing "real VM is the load-bearing MVP step" was over-rotated toward the security story. Spike A1 confirmed the V8-vs-OS hypothesis with a single host-side run; the Rust port (B1) + cutover (B3) are what's load-bearing. VM sandboxing earns its place when we ship autonomous/remote features in Phase D, not at MVP.

**D4. Workspace is a first-class type above Sessions** with N worktrees, access policy, and `RuntimeLocation`. Lifted from Zed's `Project`/`WorktreeStore` model (`crates/project/src/project.rs:213`). KB §3 D4.

**D5. RuntimeLocation as third independent axis** — `Local | Remote(endpoint)`. Workspace owns it; can move (Proliferate-style one-click). KB §3 D5.

**D6. Depend on shuru** (`shuru-vm` + `shuru-darwin`, Apache-2.0) for the Apple VZ implementation. Don't reimplement objc2 + Apple VZ + virtiofs + vsock. KB §3 D6.

**D7. Per-provider adapter for managed sandboxes** (E2B, Daytona, Modal, Vercel Sandbox). Each provider gets its own `Sandbox` impl + `AcpTransport` variant. Cloudflare Workers excluded (V8 isolate trap). KB §3 D7.

**D8. Daemon + multi-client architectural split.** `cuartel-daemon` owns Workspaces / Sessions / Sandboxes / ACP transports / portless route registry. `cuartel-app` is a GPUI client over WebSocket to the daemon. CLI is the same WebSocket client. **Promoted from open question 17 to decision.** Validated by Paseo's daemon+multi-client architecture (KB 4.19). Unlocks: headless server use, future iOS/Android client, web client, multi-machine, easier testing, lower coupling, multiplayer foundation. Refactor cost grows non-linearly with feature surface — must land before step C2 / persistence work.

**D9. Session is an event-log interface, not a row in SQLite.** `append / read_range(epoch) / replay`. SQLite is one possible storage under it. Validated by Anthropic Managed Agents `getEvents/wake(sessionId)` and Paseo `agent-timeline-store.ts:130-191` epoch-based reset. Promoted from open question 12. **Without this, harness restarts can't replay context cleanly and we'll retrofit painfully.**

### Dependency decisions

**Dep1. `shuru-vm` + `shuru-darwin`** (Apache-2.0) for Apple VZ. Pin to a commit; contribute upstream when we hit limits; fork only if truly blocked. KB §3 D6 + KB 4.4.

**Dep2. `agent-client-protocol`** Rust crate for ACP wire (used by Zed). KB 4.1.

**Dep3. `@zed-industries/claude-code-acp`** baked into the sandbox VM image as the Claude ACP server. KB 4.3.

**Dep4. `portless`** (npm, Apache-2.0, Vercel Labs) for branch-named-URL reverse proxy. Run as a Node sidecar inside `cuartel-daemon`; also baked into the sandbox VM image. ~1 day of integration; saves months of cert mgmt + framework quirks + mDNS. KB 4.20.

**Dep5. `microsoft/playwright-mcp`** (MIT) baked into sandbox VM image as the default browser-control MCP for Replicas with browser tools. KB §7.5 "computer use Tier 1."

**Dep6. tweetnacl crate (or rust equivalent: `crypto_box`, `dalek`)** for the future E2E-encrypted relay (Tailscale alternative). Not a dependency now — referenced for Phase F.

### Replica + Blueprint data models (canonical here)

**`Replica`** — named, persistent, configured agent profile. Workspace-scoped first; user-scoped templates later. Sessions are spawned BY a Replica.

```rust
pub struct Replica {
    pub id: ReplicaId,
    pub workspace_id: WorkspaceId,    // None for user-scoped templates
    pub name: String,                  // "frontend-claude", "test-fixer", ...
    pub icon: ReplicaIcon,             // visual identity
    pub agent_server: AgentServerCommand,
    pub default_skills: Vec<SkillId>,
    pub default_mcps: Vec<McpServerId>,
    pub permission_policy: AccessPolicy,
    pub blueprint: Option<BlueprintId>,
    pub triggers: Vec<ReplicaTrigger>,
    // ReplicaTrigger = Manual | Cron(spec) | GitHubWebhook(filter) | LinearAssign(filter) | …
    pub created_at: u64,
    pub updated_at: u64,
}
```

**`Blueprint`** — workflow defined in code, state machine of `{deterministic_node, agent_node}`. Stripe Minions pattern (KB 4.15) operationalized as data. Executable via the Loop Service (Paseo `loop-service.ts`, KB 4.19.1).

```rust
pub struct Blueprint {
    pub id: BlueprintId,
    pub name: String,
    pub nodes: Vec<BlueprintNode>,
    pub edges: Vec<BlueprintEdge>,    // DAG
}

pub enum BlueprintNode {
    Deterministic { name: String, command: String, expect_exit: i32 },
    Agent { name: String, replica_id: ReplicaId, prompt_template: String,
            output_schema: Option<JsonSchema>, max_iterations: u32 },
    LoopUntil { worker: AgentNodeId, verifier: AgentNodeId | DeterministicNodeId,
                max_iterations: u32 },
}
```

**`Session`** — one conversation, owned by a Replica, with an event-log timeline.

```rust
pub struct Session {
    pub id: SessionId,
    pub replica_id: ReplicaId,
    pub workspace_id: WorkspaceId,
    pub acp_session_id: acp::SessionId,
    pub work_dirs: PathList,           // scoping within the workspace
    pub epoch: SessionEpoch,           // UUID, bumped on context reset
    pub running: bool,
    pub archived: bool,
    pub created_at: u64,
    pub updated_at: u64,
}

pub trait SessionEventLog {
    fn append(&mut self, event: SessionEvent) -> EventSeq;
    fn read_range(&self, epoch: SessionEpoch, after: EventSeq) -> Vec<SessionEvent>;
    fn replay(&self, epoch: SessionEpoch) -> Vec<SessionEvent>;
    fn current_epoch(&self) -> SessionEpoch;
    fn reset_epoch(&mut self, handoff_artifact: Option<PathBuf>) -> SessionEpoch;
}
```

Tier 0 of Replicas: ship config-only first (Paseo pattern, KB 4.19) — `cuartel.json`'s `replicas: []` lets users get the named-agent UX without the full DB-backed model. Sessions still get spawned from these config replicas; full data model lands in Phase E.

---

## Abstractions (revised)

### `Workspace` (new, above sessions)

Modeled after Zed's `Project` (`zed/crates/project/src/project.rs:213`) but stripped of editor concerns (no buffer_store, no git_store, no LSP):

```rust
pub struct Workspace {
    pub id: WorkspaceId,
    pub worktrees: Vec<Worktree>,         // N repos
    pub agent_servers: AgentServerStore,  // which ACP servers are available here
    pub access_policy: AccessPolicy,      // which paths this workspace's agents can touch
    pub runtime: RuntimeLocation,         // Local | Remote(endpoint)
}

pub enum RuntimeLocation {
    Local,
    Remote { endpoint: TailscaleEndpoint, sandbox: SandboxKind },
}
```

Sessions (threads) are children of a workspace. Workspaces can **relocate** — snapshot worktrees, drain sessions, transfer, rehydrate on the other side. Proliferate-style one-click move.

### `Session` (formerly "Thread")

One agent conversation. Per-session state (Zed-style, `crates/agent/src/thread.rs:936`):

```rust
pub struct Session {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub acp_session_id: acp::SessionId,     // ACP's own id
    pub agent_server_id: AgentServerId,     // Claude / Gemini / …
    pub work_dirs: PathList,                 // scoping within the workspace
    pub messages: Vec<Message>,
    pub model_selection: Option<ModelId>,
    pub running: bool,
    pub archived: bool,
}
```

Persistence: one `sessions` table in `cuartel-db`. Zed's schema is a fine starting point (`zed/crates/agent_ui/src/thread_metadata_store.rs:1238`): `(id, workspace_id, agent_id, title, created_at, updated_at, folder_paths, archived)`. Writes go through a `smol::channel` queue draining in a background task — never block the UI.

### `trait Sandbox` (provisioning only)

```rust
#[async_trait]
pub trait Sandbox: Send + Sync {
    async fn provision(&self, ws: &Workspace) -> Result<SandboxHandle>;
    async fn spawn_agent(
        &self,
        h: &SandboxHandle,
        cmd: AgentServerCommand,
    ) -> Result<AcpTransport>;
    async fn forward_port(&self, h: &SandboxHandle, port: u16) -> Result<HostPort>;
    async fn snapshot(&self, h: &SandboxHandle) -> Result<SnapshotId>;   // for moves
    async fn dispose(&self, h: SandboxHandle) -> Result<()>;
}
```

Impls (all backed by **real OS** environments — no V8 isolates):
- `AppleVzSandbox` — local Linux VM via `Virtualization.framework`. Entitlement already in `entitlements.plist`. Default for local mode.
- `LocalSandbox` — temp dir + process group on the host (no isolation). Dev / CI only — proves the trait shape, never ships to users.
- `HetznerSandbox` — Firecracker microVM (or plain KVM) on a Hetzner host, Tailscale-attached, workspace mounted. Default for remote mode.
- `E2BSandbox`, `ModalSandbox`, `DaytonaSandbox` — future, when product calls for them.
- `RivetVmSandbox` — *if* Rivet ships VM-backed sandboxes. Optional. Their secure-exec product is intentionally not in this list.

### `AgentServerCommand` (config, not a trait)

Adding a new model is a config entry, not a new Rust impl. Modeled on Zed's `AgentServerCommand` (`zed/crates/project/src/agent_server_store.rs`):

```rust
pub struct AgentServerCommand {
    pub id: AgentServerId,
    pub binary: PathBuf,         // path INSIDE the sandbox image
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub default_model: Option<ModelId>,
}
```

Shipping Claude = shipping a sandbox image that has `claude-code-acp` installed. Shipping Gemini = same, with `gemini-cli`. Zero cuartel code changes per model.

### `AcpTransport`

```rust
pub enum AcpTransport {
    LocalStdio(ChildStdio),
    RemoteStdio { ssh: SshSession, child_id: u32 },
    FramedTcp { addr: SocketAddr, auth: Token },
}
```

The ACP wire format (JSON-RPC lines) is transport-agnostic. `LocalStdio` for local sandboxes, `RemoteStdio` when ssh/Tailscale-wrapping the agent spawn (Zed's pattern — `acp.rs:505–528`), `FramedTcp` if we want a persistent remote ACP daemon.

---

## What moves where

| Thing today | Where it lives now | Where it goes |
|---|---|---|
| AgentOS secure-exec (V8 isolate) as the agent runtime | Rivet sidecar | **Removed** from the agent path. Replaced by a real Linux VM (`AppleVzSandbox` / `HetznerSandbox`). |
| Claude ACP adapter (inside V8) | Rivet sidecar | **ACP server (`claude-code-acp`) installed in the VM image; spawned as a plain OS process** |
| Claude CLI child | Grandchild V8 isolate | Child of the ACP server, plain `node` OS process inside the VM |
| `claude-agent-sdk` runtime patch | `rivet/server.ts` | **Deleted** — no V8, no patch needed |
| `loopbackExemptPorts: [6421]` | `rivet/server.ts` | **Deleted** — no secrets flow through VM unless we opt in |
| `GATEWAY_PORT` plumbing | `cuartel-app/src/main.rs` | Kept but optional — the gateway is a DIDH layer for audit/egress policy |
| `ANTHROPIC_API_KEY` injection | `build_sidecar_env` | Injected into the spawned ACP server's env (never persisted in VM) |
| Tool calls (Bash, Read, …) | In sandbox (via adapter) | **Still in sandbox, loopback — zero-RTT inside VM** |
| Session lifecycle | `cuartel-rivet` client | `cuartel-acp` client + `sessions` SQLite table |
| Model swapping | Code change in rivet | `AgentServerCommand` config entry |

---

## Security model

Threat surfaces, explicit:

| Threat | Mitigation |
|---|---|
| **Prompt injection via tool output** (file/web content steers the model) | Tool results flagged as untrusted in transcripts; permission prompts for destructive ops; outbound firewall denies private-IP + metadata endpoints (phase 5f, already landed) |
| **Malicious sandbox** (compromised ACP server lies in results) | Tool call previews surfaced in GPUI before approval; audit log kept host-side; destructive ops (rm -rf /, etc.) hardcoded-denied at cuartel layer regardless of ACP request (mirror Zed `tool_permissions.rs:20`) |
| **Credential exfiltration** | Secrets never persist in VM — injected as env at ACP-server spawn time; optional auth gateway still available for audit/rotation; keychain remains on host |
| **MCP servers** | Spawned by the ACP server inside the sandbox — inherit the VM's network policy automatically; no special handling needed |
| **Sandbox-escape → host** | Standard VM isolation (Rivet/Hetzner); host firewall blocks private-IP egress; no shared mounts beyond the workspace |
| **Cross-session contamination** | One sandbox per workspace-session. No shared VMs. |

Per-session scoping: `work_dirs: PathList` (Zed `acp_thread.rs:1040`) whitelists the paths an agent may touch inside the workspace. The ACP server honors it; we enforce it again at the GPUI layer when surfacing tool calls.

---

## Persistence

One SQLite migration in `cuartel-db`. Schema lifted from Zed (`zed/crates/agent_ui/src/thread_metadata_store.rs:1238`):

```sql
CREATE TABLE sessions (
    id              BLOB PRIMARY KEY,
    workspace_id    BLOB NOT NULL,
    acp_session_id  TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    title           TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    work_dirs       TEXT NOT NULL,   -- JSON array
    archived        INTEGER NOT NULL DEFAULT 0,
    model_id        TEXT
);
CREATE INDEX sessions_by_workspace ON sessions(workspace_id);
CREATE INDEX sessions_by_updated ON sessions(updated_at);
```

Writes are enqueued on a `smol::channel` and drained by one background task. UI never blocks on DB I/O.

Transcripts (message bodies) live in a separate table or JSONL file per session — not pinned yet, pick when we implement.

---

## Parallel sessions (UX)

Copy Zed's pattern (`zed/crates/agent_ui/src/agent_panel.rs:696`):

- `retained_sessions: HashMap<SessionId, Entity<SessionView>>`. Switching sessions moves the old view to the pool, doesn't drop it. Running sessions keep generating in the background.
- `MaxIdleRetainedSessions = 5` (config). Eviction only for idle+resumable sessions.
- Notification windows (`ui/agent_notification.rs:10`) for permission / error events — floating, cross-session, auto-dismiss.
- Per-session model selection lives on the session view (`thread_view.rs:280`).

---

## Runtime placement and one-click move

Each `Workspace` pins a `RuntimeLocation`. Moves are a first-class operation:

1. **Drain** — stop accepting new prompts in the running session. Flush in-flight turn.
2. **Snapshot** — `Sandbox::snapshot(handle) -> SnapshotId`. For Rivet/Hetzner: disk image + ACP server state (session transcript is already in SQLite). For local: tar the worktree.
3. **Transfer** — push snapshot to target runtime. Tailscale-encrypted.
4. **Rehydrate** — target-side `Sandbox::provision` with `SnapshotId`. Respawn ACP server, resume session via ACP's `loadSession`.
5. **Re-point** — cuartel-app updates `Workspace.runtime`, reopens ACP transport, unblocks UI.

This is Proliferate's "local → cloud in one click." Because workspace/session state is identical across locations, moves are a transport swap — no data model changes.

---

## Phases & steps (with Definition of Done)

Each step has: **Goal** (one sentence), **Scope** (in/out), **DoD** (acceptance tests that mechanically prove "done"), **Effort** (rough person-weeks for one engineer focused), **Risks**, **Rollback** (what we ship if this step partially fails).

> **The phases.** **A** proves the hypothesis. **B** replaces the broken runtime. **C** is the MVP — cuartel becomes self-hosting at the end of C. **D** adds remote runtime + workspace mobility. **E** delivers the visual command center + full Replicas. **F** is ongoing polish.

### Phase A — Prove the hypothesis (Week 1)

#### Step A1: `LocalSandbox` spike
- **Goal:** Prove the V8-vs-OS hypothesis. If the sendPrompt hang doesn't reproduce when claude-code-acp runs as a plain Node process, the v2 architecture is correct.
- **Scope:** in: spawn claude-code-acp as a child OS process on the host (no VM); hand-rolled minimal ACP client in TypeScript or Rust; one full Claude turn end-to-end. out: cuartel-app integration, persistence, UI.
- **DoD:**
  1. ✅ `claude-code-acp` spawns as `node` subprocess.
  2. ✅ ACP `initialize` handshake succeeds.
  3. ✅ One `prompt` request → streaming `update` events → final `result` arrives in ≤ 60s.
  4. ✅ Tool call (e.g. `Read`) round-trips.
  5. ✅ Run the spike 50 times in a row — **zero hangs**.
  6. ✅ Document behavior of ACP `loadSession` on this version of claude-code-acp (resolves open Q 3).
- **Effort:** 1 person-week.
- **Risks:** The hang reproduces on host (would mean V8 is not the cause; would force a different architectural pivot — likely fork claude-code-acp).
- **Rollback:** Throwaway code; if hypothesis fails, the v2 plan needs reconsideration before Phase B.

### Phase B — Replace the runtime (Weeks 2–4)

> **Pivoted 2026-04-27 (after A1 spike).** Original Phase B included `AppleVzSandbox` (B2) as the load-bearing step at ~3 person-weeks. After confirming the V8-vs-OS hypothesis and discussing scope, we ship MVP with `LocalSandbox` only — claude-code-acp as a plain host subprocess, like Zed/Polyscope/Paseo all do today. `AppleVzSandbox` moves to Phase D as the "secure mode" for autonomous/scheduled/remote work where the user isn't watching. **Saves ~3 person-weeks; gets to dogfood faster.**

#### Step B1: `cuartel-acp` Rust crate
- **Goal:** Production-quality ACP client in Rust; the foundation of every later step.
- **Scope:** in: wrap `agent-client-protocol` crate; implement `fs/read_text_file`, `fs/write_text_file`, `terminal/create` server-side handlers; multi-pass Zod-equivalent (likely `serde` + custom tagged unions) tool-call normalization to canonical kinds (`shell`, `read`, `write`, `edit`, `search`, `fetch`); provider capability flags (Paseo `acp-agent.ts:92-99` pattern); session lifecycle (`createSession`, `prompt`, `cancel`, `loadSession` if supported). out: VM integration, UI integration.
- **DoD:**
  1. ✅ `cuartel-acp` connects to `claude-code-acp` (subprocess, stdio transport) and completes one full turn.
  2. ✅ Tool-call normalization test: every variant of `bash` / `Bash` / `shell` / `exec_command` collapses to canonical `shell` kind.
  3. ✅ Capability-flag negotiation: cuartel-acp queries server, gates feature use.
  4. ✅ `fs/read_text_file` round-trips a 10MB file without OOM.
  5. ✅ `terminal/create` spawns a PTY and streams output.
  6. ✅ Session resume via `loadSession` works (or, if claude-code-acp doesn't support it, we have a documented fallback strategy).
  7. ✅ Unit-test coverage ≥ 80% on the wire-protocol layer; 100% on tool-call normalization.
  8. ✅ Crate published to internal registry (or `crates.io` if open-sourcing).
- **Effort:** 1.5 person-weeks.
- **Risks:** ACP's Rust client may have rough edges; tool-name variants discovered post-shipping.
- **Rollback:** `cuartel-acp` is a library; partial impl still useful for spike iterations.

#### Step B2: `LocalSandbox` impl + cutover
- **Goal:** Replace AgentOS secure-exec with `LocalSandbox` (claude-code-acp as a plain host subprocess). Cuartel runs the new stack; sendPrompt hang gone.
- **Scope:** in: `LocalSandbox` impl of `trait Sandbox` (process group on host, no isolation, the simplest possible impl); rewrite `session_host.rs` to use `Workspace` + `Sandbox` + `cuartel-acp`; delete `GATEWAY_PORT`/`CUARTEL_LOOPBACK_EXEMPT_PORT` plumbing; keep auth-gateway as opt-in; SQLite migration for `sessions` table (MVP shape — Replicas come in C3); migrate any in-flight existing sessions to the new schema (or document one-time wipe). out: VM-based sandboxing (Phase D), portless integration (C3), persistence/UI rework (C2).
- **DoD progress (updated 2026-04-27 after step 1 commit `feb6d10`):**
  1. ⚠️ Partial — `CUARTEL_USE_ACP=1` opts into the host-direct ACP driver; AgentOS secure-exec still spawned by default. Flipping default + ripping out Rivet path lands in a follow-up commit once the user smoke-tests the ACP path in GPUI.
  2. ✅ `tests/live_claude_code_acp.rs` proves cuartel-acp + LocalSandbox completes a full turn end-to-end (`stop_reason=end_turn`, ~15s, clean dispose).
  3. ✅ `tests/no_hang_regression.rs` runs 5 consecutive turns, **5/5 pass, 0 hangs, p_min=7.48s, p_max=9.14s.** sendPrompt-hang is gone for the new path.
  4. ⏳ DB schema unchanged in step 1 (existing `sessions` table is sufficient). Event-log migration lands in C2 alongside Session-as-event-log per D9.
  5. ✅ Auth gateway plumbing untouched; the new ACP path uses the user's existing `claude` CLI subscription auth (no `ANTHROPIC_BASE_URL` redirect needed for that path).
  6. ⏳ User smoke-test pending: launch cuartel-app with `CUARTEL_USE_ACP=1 CUARTEL_ACP_CWD=/path/to/repo cargo run -p cuartel-app`.
  7. ⏳ `cuartel-rivet` deprecation deferred until step-1 has GPUI smoke-test signoff.
- **Effort:** 2 person-weeks (~half spent on step 1).
- **Risks:** Existing-session migration edge cases; first-time users without `claude` CLI auth need a clean error path.
- **Rollback:** Feature-flag the new path; old AgentOS secure-exec path stays bootable for one release. If new path errors, fall back automatically and log telemetry.

> **End of Phase B: cuartel runs claude-code-acp natively, no V8 nesting, no hang. Architecture is sound; UX still rudimentary. Same isolation level as Zed/Polyscope/Paseo today (i.e. host-direct with permission UI as the safety net).**

### Phase C — MVP (Weeks 8–15) — DOGFOOD LINE

> **Goal of Phase C:** Cuartel becomes good enough that the cuartel team uses cuartel as their primary tool to build the next cuartel feature. See "MVP definition" below for what self-hosting requires.

#### Step C1: Daemon + multi-client split (D8)
- **Goal:** Decouple state ownership from UI rendering. `cuartel-daemon` owns Workspaces / Sessions / Sandboxes / ACP transports / portless route registry. `cuartel-app` (GPUI) and `cuartel-cli` are clients over a versioned WebSocket protocol.
- **Scope:** in: define WebSocket message schema with Zod-equivalent (`serde` discriminated unions); daemon-side state machine; reduce cuartel-app to a thin client; `cuartel daemon start/stop/status/restart` CLI; daemon auto-start when cuartel-app launches; daemon survives cuartel-app close; pairing for future remote/multi-client. out: relay (Phase F), mobile (Phase F).
- **DoD:**
  1. ✅ `cuartel daemon start` runs the daemon headless; `cuartel ls` lists active sessions; `cuartel run "prompt"` spawns a session — all without cuartel-app open.
  2. ✅ cuartel-app close-and-reopen reconnects to the same daemon and shows running sessions.
  3. ✅ WebSocket protocol versioned (`PROTOCOL_VERSION = 1`); backward-compat test passes (a "v1 client" snapshot can parse a "v1 daemon" responses, and vice versa — see Test Strategy below).
  4. ✅ All shared types live in a single `cuartel-protocol` crate.
  5. ✅ Daemon process can run on a separate machine (Mac Mini); cuartel-app on laptop connects to it via direct WebSocket. (Foundation for Phase D + F.)
- **Effort:** 2 person-weeks.
- **Risks:** Refactor scope creep; backward-compat semantics decided too late.
- **Rollback:** If daemon split runs into issues mid-phase, can ship in-process (cuartel-app embeds the daemon) for one release while iterating; the protocol/abstractions still work.

#### Step C2: Persistence + per-session timeline + retained-sessions pool + notifications (Zed patterns)
- **Goal:** Sessions persist across restarts. Multiple parallel sessions don't lose state. UI surfaces what needs attention.
- **Scope:** in: `Session` event-log interface (D9) backed by SQLite; per-session SQLite file under `$CUARTEL_HOME/sessions/<id>.sqlite` (Ramp pattern, KB 4.7); epoch-based timeline reset (Paseo pattern, KB 4.19.1); retained-sessions pool with `MaxIdleRetainedSessions = 5` and idle eviction (Zed `agent_panel.rs:696`); notification windows (Zed `ui/agent_notification.rs:10`); per-session model selector. out: command-center view (E1).
- **DoD:**
  1. ✅ Restart cuartel-app → previous sessions visible in sidebar with full transcript.
  2. ✅ Run 6 parallel sessions; oldest idle one evicts from retained pool when 7th opens.
  3. ✅ Notification window fires when session A needs permission while user looks at session B.
  4. ✅ Per-session model selector switches between Claude/Codex/etc. mid-session.
  5. ✅ Session event log unit tests: `append → read_range → epoch reset → replay` works correctly.
  6. ✅ Backward-compat: a session created in C2 is still readable after C3, C4, etc.
- **Effort:** 2 person-weeks.
- **Risks:** SQLite write-queue contention under load (mitigation: per-session SQLite, not one global).
- **Rollback:** Disable retained-pool feature flag if causes memory issues; persistence is required (no rollback there).

#### Step C3: `cuartel.json` + Replicas v0 (config) + Tasks + portless integration
- **Goal:** Workspaces have a config file that defines dev environment + reusable prompts + named agent profiles. Branch-named URLs work.
- **Scope:** in: `cuartel.json` parser at workspace root (`worktree.setup`, `worktree.teardown`, `worktree.terminals[]`, `scripts: {name: {command, type: "service"?, port?}}`, `tasks: []`, `replicas: []` Tier 0 config-only); setup script runs in sandbox VM at workspace boot; portless integration in `cuartel-daemon` Node sidecar (`getRoutes` callback reads from session/workspace store); branch-aware hostname routing; service-to-service env injection (`CUARTEL_SERVICE_<NAME>_URL`). out: full Replica data model (E2), visual workflow editor (E4), triggers beyond manual (E2).
- **DoD:**
  1. ✅ Sample workspace (e.g. a fresh Next.js app) added: `cuartel.json` with `setup: "npm ci"` + `scripts.web: {command: "next dev", type: "service"}`.
  2. ✅ Setup script runs inside the AppleVz sandbox at workspace boot; dev server reachable at `https://web.<branch>.<workspace>.localhost` from the host browser, HTTPS, HTTP/2.
  3. ✅ Branch rename → URL changes accordingly without restart.
  4. ✅ `cuartel.json` `tasks: [{label: "Security review", prompt: "..."}]` shows in sidebar dropdown; click → fresh session with prompt.
  5. ✅ `cuartel.json` `replicas: [{name: "claude-frontend", agent_server: "claude", default_skills: [...], default_mcps: [...]}]` parsed; sidebar shows two named Replicas with different defaults; new sessions can be spawned from each.
  6. ✅ Service-to-service env: a frontend service inside the sandbox can hit `$CUARTEL_SERVICE_API_URL` and reach the API service.
  7. ✅ Run a real Vite app + Express API in two parallel worktrees of the same workspace — each gets distinct hostnames, no port conflicts.
- **Effort:** 2 person-weeks.
- **Risks:** Framework-quirk surprises in portless (mitigation: portless's hand-tuned framework support — Vite/Next/Astro/Expo — is exactly why we depend on it).
- **Rollback:** If portless integration runs late, ship `cuartel.json` parsing + Tasks + config Replicas first; portless can land in a follow-up.

#### Step C4: CLI + output-schemas + minimal MCP-server-for-sub-agent-control
- **Goal:** Headless usability + scripting + agent-self-invocation foundations.
- **Scope:** in: full CLI surface (`cuartel run`, `ls`, `attach`, `send`, `logs`, `wait`, `permit ls/allow/deny`, `replica ls/spawn`); `--output-schema` re-prompting up to 2 retries (Paseo pattern, KB 4.19.1); minimal MCP server inside `cuartel-daemon` exposing `replica.spawn`, `replica.send_prompt`, `replica.wait`, `replica.logs` (Paseo `mcp-server.ts:318+` pattern); caller-context inheritance (parent's mode/model/MCPs propagate to child); workspace-level subagent quota config. out: full visual workflow editor (E4), full Blueprint executor (E2 partial, E4 visual).
- **DoD:**
  1. ✅ `cuartel run "fix the failing tests"` from a workspace's terminal spawns a session, runs to completion, prints the PR diff URL.
  2. ✅ Implement+verify shell-script loop works (Paseo pattern): `codex` implements, `claude --output-schema {criteria_met:bool}` verifies, `jq` parses verdict, loop until `true` or max-iterations.
  3. ✅ A running agent invokes the MCP `replica.spawn` tool to spawn a subagent; subagent inherits parent's mode/model/MCPs by default; parent receives child's final result via `replica.wait`.
  4. ✅ Per-workspace quota (e.g. `max_subagents: 3`) enforced — subagent spawn beyond quota errors gracefully.
  5. ✅ CLI integration test: scripted multi-agent workflow (orchestrator → 3 parallel workers → reduce) completes end-to-end.
- **Effort:** 1.5 person-weeks.
- **Risks:** MCP server boundary semantics (what counts as caller-context?); quota enforcement edge cases.
- **Rollback:** Ship CLI without MCP server first; subagent spawn becomes a Phase E feature.

#### Step C5: Dogfood checkpoint
- **Goal:** Validate MVP. Cuartel team uses cuartel as the primary tool to build the next cuartel feature for one full week.
- **Scope:** in: open the cuartel repo as a workspace; configure a `cuartel-builder` Replica (Claude w/ Rust skills + cuartel-codebase context) and a `cuartel-reviewer` Replica (Claude w/ generator-evaluator pattern, reads `AGENTS.md`); use them to design and ship one real feature; capture learnings in `DOGFOOD_NOTES.md`. out: any new features (only what naturally falls out of dogfood).
- **DoD:**
  1. ✅ Cuartel team uses cuartel daily for ≥ 1 week (each engineer ≥ 5 sessions per day).
  2. ✅ ≥ 1 PR merged to the cuartel repo where the diff was substantially written by cuartel-driven sessions (subjective: ≥ 60% of lines, accept human review/edits).
  3. ✅ Zero falls-back-to-Polyscope/Paseo/Cursor/Claude-app for things cuartel should be able to do (only intentional choice of alternatives is OK).
  4. ✅ sendPrompt hang regression test passes consistently (50 runs, 0 hangs).
  5. ✅ Top three friction points captured in `DOGFOOD_NOTES.md` and triaged into Phase D/E backlog.
- **Effort:** 1 person-week (the team is using the product, not building it).
- **Risks:** We discover the MVP doesn't feel good enough for serious use (mitigation: this is the test — adjust scope of C2-C4 if dogfood reveals critical gaps).
- **Rollback:** N/A — this is the validation gate. If it fails, we extend Phase C, not roll back.

> **🎯 END OF PHASE C: cuartel is self-hosting. The MVP is shipped.** From this point on, every feature is built with cuartel.

### Phase D — Remote runtime + secure-mode VM sandbox + workspace mobility (Weeks 13–21)

> **Why VM sandboxing lives here, not in MVP.** Interactive sessions where a user is at the keyboard approving each tool call don't need VM isolation — the user is the sandbox. **VM sandboxing earns its place when the user isn't watching:** scheduled Replicas, GitHub-webhook-triggered runs, "ship it" autonomous mode, and remote sandboxes on Hetzner where untrusted code runs without local supervision. So `AppleVzSandbox` ships here, alongside `HetznerSandbox`, gated behind an opt-in "secure mode" toggle that becomes the default for autonomous/scheduled work.

#### Step D0: `AppleVzSandbox` as opt-in secure mode (was B2 in original v2)
- **Goal:** Local Linux VM via Apple Virtualization.framework that boots claude-code-acp and exposes ACP transport over vsock. Available as `Workspace.sandbox = "vm"` opt-in, automatically chosen when a session is autonomous/scheduled/unattended.
- **Scope:** in: depend on `shuru-vm` + `shuru-darwin`; build minimal Linux VM image with `claude-code-acp` + `portless` + `microsoft/playwright-mcp` + Chromium baked in; vsock-based stdio transport for ACP; `Sandbox::provision/spawn_agent/forward_port/snapshot/dispose` impls; lazy provision (defer VM allocation until first `spawn_agent` call); per-Workspace + per-Replica config flag to opt into VM mode. out: HetznerSandbox (D1, builds on the same image).
- **DoD:**
  1. ✅ VM cold-start time ≤ 5s from `provision()` to `spawn_agent` ready.
  2. ✅ Lazy provisioning verified: `provision()` without `spawn_agent` allocates no VM.
  3. ✅ Identical-results test: same prompt → `LocalSandbox` and `AppleVzSandbox` produce structurally-equivalent tool-call sequences.
  4. ✅ portless inside VM serves a Vite dev server at `https://web.<branch>.<workspace>.localhost`. Reachable from host browser.
  5. ✅ Snapshot + restore round-trips an active session within 10s.
  6. ✅ Sandbox dispose leaves no orphan processes or disk artifacts.
  7. ✅ Image build pipeline produces a `.dmg`-shippable image. Update cadence documented.
  8. ✅ Run 100 sandbox lifecycles in CI (provision → spawn_agent → one turn → dispose) — no hangs, no leaked resources.
  9. ✅ Workspace can opt in via `cuartel.json` `workspace.sandbox: "vm"`; per-Replica override works; autonomous Replicas (cron / webhook / "ship it" mode) auto-select VM.
- **Effort:** 3 person-weeks.
- **Risks:** Apple VZ + Linux GPU-passthrough quirks; shuru API gaps (may need upstream PRs); image-size bloat from Chromium (~200MB).
- **Rollback:** `LocalSandbox` stays default. VM mode is opt-in; users who don't want it never see it. If shuru blocks us, fork (Apache-2.0).

#### Step D1: `HetznerSandbox`
- **Goal:** Sessions can run on a Hetzner box co-located with the workspace files. ACP wire travels host↔Hetzner over Tailscale; tool calls are loopback inside the remote VM.
- **Scope:** in: Firecracker (or plain KVM) microVM provisioning on a Hetzner host; Tailscale-attached for the ACP transport; same VM image as `AppleVzSandbox` (Linux works on both); `RemoteStdio` transport (stdio-over-SSH inside Tailscale — Zed pattern from `acp.rs:505-528`); workspace mount over Tailscale-encrypted channel. out: workspace move (D2), background-session lifecycle UX (D3).
- **DoD:**
  1. ✅ Run an identical session on `AppleVzSandbox` and `HetznerSandbox` — produces structurally-equivalent results.
  2. ✅ ACP message round-trip latency ≤ 150ms (Hetzner Helsinki, user in Europe). Document the measured budget.
  3. ✅ Tailscale disconnect/reconnect doesn't lose session state (event log replays on reconnect).
  4. ✅ Hetzner VM lifecycle (provision/dispose) doesn't leak resources after 24h soak test.
  5. ✅ User can declare `workspace.runtime: Remote(hetzner-1)` in `cuartel.json` and sessions run remotely.
- **Effort:** 3 person-weeks (Firecracker tooling + Tailscale wiring + workspace transfer).
- **Risks:** Tailscale latency surprises (mitigation: test from European/US/Asian endpoints); Firecracker setup pain (mitigation: shuru-equivalent for Linux is plausible).
- **Rollback:** Ship as opt-in feature flag; local-only stays default.

#### Step D2: Workspace move (one-click local↔remote, idle sessions only)
- **Goal:** Proliferate-style "send this workspace to the cloud" — and bring it back.
- **Scope:** in: snapshot worktrees + transfer over Tailscale + rehydrate on target; UI button "Move workspace to..."; idle sessions only (running sessions error with "stop session first"); event-log replay on the new side. out: live migration of running sessions (Phase F).
- **DoD:**
  1. ✅ Move workspace local→remote completes in ≤ 60s for a 100MB workspace.
  2. ✅ After move, sessions on the new side replay full event-log history.
  3. ✅ Move back (remote→local) works symmetrically.
  4. ✅ Move with running session errors clearly: "Stop session X first; live migration coming soon."
  5. ✅ Round-trip move integrity: hash of workspace files on local matches after move-to-remote-and-back.
- **Effort:** 2 person-weeks.
- **Risks:** Workspace-mount semantics with encrypted vault (open Q 7 — host-decrypt vs in-VM-decrypt) must be resolved.
- **Rollback:** Ship one direction (local→remote) first; reverse direction can land separately.

#### Step D3: Background-session lifecycle on macOS
- **Goal:** Close laptop with a remote session running → on wake, cuartel-app reconnects, completed-while-asleep work is shown, notifications fired.
- **Scope:** in: cuartel-app sleep/wake handlers; `cuartel-daemon` (running on Hetzner) detects orphaned sessions and continues them; macOS notification pipeline (when cuartel-app is open: native NSUserNotification; when closed: future APNs or polling on next launch). out: full mobile companion (Phase F).
- **DoD:**
  1. ✅ Sleep test: open laptop, start session "summarize this 10K-line file," sleep laptop for 5 min, wake → session completed, summary visible.
  2. ✅ Long-sleep test (12h, simulated): same outcome; session state intact.
  3. ✅ Notification pipeline works while cuartel-app is open (basic case).
  4. ✅ When cuartel-app is closed and reopened later, completed sessions show with a "completed while away" badge.
- **Effort:** 1 person-week.
- **Risks:** macOS power-management edge cases; notification permissions UX.
- **Rollback:** Ship without notifications; users can poll the app on wake.

> **End of Phase D: workspaces are mobile, long-running cloud sessions survive sleep, daemon-on-Hetzner is real.**

### Phase E — Visual command center + Replicas v1 + computer use (Weeks 22–31)

#### Step E1: Birds-eye command center view
- **Goal:** A peer-level UI that shows every running session across every workspace at a glance. The "Factorio for agents" view (a16z thesis, KB 7.6).
- **Scope:** in: new top-level view alongside the per-session sidebar; cards for each session showing status / model / sandbox / cost-so-far / current tool call; click-to-zoom; hotkey groups (`Cmd-1..9` selects pre-assigned session groups); drag-to-assign skills/MCPs to Replicas via DnD. out: full visual workflow editor (E4), multiplayer (Phase F).
- **DoD:**
  1. ✅ 12 parallel sessions across 3 workspaces all visible in command-center view.
  2. ✅ Click a card → zoom into that session's view.
  3. ✅ Hotkey group: assign sessions A+B+C to `Cmd-1`; press `Cmd-1` later → those three sessions selected.
  4. ✅ Drag a skill from a palette onto a Replica card → Replica's `default_skills` updated (config Replica) or DB-backed Replica (after E2).
- **Effort:** 3 person-weeks.
- **Risks:** GPUI layout complexity for the multi-card grid; DnD UX iteration time.
- **Rollback:** Ship a simpler "all sessions" list view first; full Factorio-style cards iterate after.

#### Step E2: Replicas v1 (full DB-backed model) + Blueprint v1 + Loop Service
- **Goal:** Replicas as first-class DB-backed entities with triggers; Blueprints as executable workflows.
- **Scope:** in: Replica + Blueprint tables in cuartel-db; triggers (manual + cron + GitHub webhook); Loop Service (Paseo pattern — worker+verifier with bounded iterations, command-or-LLM verify); generator-evaluator Replica pair as canonical example; `cuartel.json` `replicas:[]` becomes one of several sources (config + DB merge). out: visual graph editor (E4), Linear/Slack triggers (Phase F).
- **DoD:**
  1. ✅ Cron-triggered Replica fires nightly at scheduled time; runs to completion; result visible next morning.
  2. ✅ GitHub webhook trigger: open a PR with `@cuartel review` → Replica spawns a session, posts review comment.
  3. ✅ Generator-evaluator pair: configure `feature-builder` and `pr-reviewer` Replicas; running `feature-builder` triggers `pr-reviewer` on PR open; only when reviewer passes does the PR move to "ready for human review."
  4. ✅ Loop Service test: run `worker` Replica with command-based verifier (`npm test`); loops until tests pass or hits `max_iterations: 5`.
  5. ✅ Blueprint executor: define a 4-node Blueprint (det → agent → det → agent); execution traverses the DAG correctly.
- **Effort:** 3 person-weeks.
- **Risks:** Trigger-source security (GitHub webhook auth); cron-during-laptop-sleep semantics; Blueprint cycle detection.
- **Rollback:** Ship triggers separately (manual first, cron second, webhook third); each is independently shippable.

#### Step E3: Computer use Tier 0 + 1 + 2
- **Goal:** Agents can drive UIs and produce video artifacts, like Cursor cloud agents (KB 4.16).
- **Scope:** in: Tier 0 (Visual Editor — embedded webview + DOM-element-picker JS overlay → agent gets selector + position + screenshot); Tier 1 (Playwright + microsoft/playwright-mcp already in image, surface as opt-in MCP per Replica); Tier 2 (Xvfb + openbox + xdotool + ffmpeg + x11vnc as optional desktop layer in image; custom desktop MCP); Artifacts panel (video / screenshot / HTML preview from sandbox `/artifacts` virtiofs mount); optional Live Desktop panel via noVNC in GPUI webview. out: per-pixel diff comparison (Phase F).
- **DoD:**
  1. ✅ Tier 0: open preview, click element, type "make this heading larger" → agent receives selector + position → makes correct CSS change in the file.
  2. ✅ Tier 1: agent uses Playwright MCP to navigate to a page and verify a button exists.
  3. ✅ Tier 2 (opt-in): agent records a 30s video of itself completing a task; video viewable in Artifacts panel.
  4. ✅ Live Desktop: open a session, click "Take over desktop," noVNC opens in webview, mouse/keyboard control the sandbox VM.
- **Effort:** 3 person-weeks (Tier 0 ~1 week; Tier 1 ~3 days; Tier 2 + noVNC ~1.5 weeks).
- **Risks:** Webview ergonomics in GPUI; Tier 2 image bloat.
- **Rollback:** Tier 0 alone is shippable and covers ~80% of frontend cases (Polyscope's bet, KB 4.18).

#### Step E4: Voice + scheduler UI + visual Blueprint editor
- **Goal:** Polish features that close the gap with Paseo + Polyscope.
- **Scope:** in: local voice stack (ONNX Parakeet STT + Kokoro TTS, downloaded on first use); voice-mode hidden agent session (Paseo `voiceOnly: true` pattern); cron scheduler UI (set/edit/pause/delete); visual Blueprint editor (drag-and-drop nodes, connect with edges, save). out: cloud voice (Phase F if asked).
- **DoD:**
  1. ✅ Voice dictation: hold-to-talk → text appears in prompt input.
  2. ✅ Voice mode: full conversation — user speaks, agent speaks back, hidden agent session orchestrates.
  3. ✅ Schedule UI: create a cron, see next-run time, edit, delete; runs fire at expected times.
  4. ✅ Visual Blueprint editor: drag a deterministic node + an agent node, connect, save → Blueprint runs and executes the saved DAG.
- **Effort:** 3 person-weeks (voice ~1.5w via ONNX + GPUI audio; scheduler UI ~3 days; Blueprint editor ~1w).
- **Risks:** Voice latency on weaker macOS hardware; Blueprint editor UX iteration.
- **Rollback:** Each is independently shippable. Voice can default to OpenAI cloud if local quality is poor.

> **End of Phase E: cuartel is the visual command center for multi-agent coding work, with full Replicas + Blueprints + computer use + voice. Comparable feature surface to Polyscope/Paseo while differentiated on infra (real VMs, remote runtime, mobility).**

### Phase F — Ongoing polish + scale (post-Phase-E)

Features become independent shippable units once Phase E foundations are in place. Backlog:

- Multi-version prompt fan-out UI (KB 7.5)
- Multiplayer / shared workspaces (KB 7.6, requires E2 + D-foundation)
- LAN mode via portless mDNS (KB 7.5; depends on portless mDNS integration)
- E2E-encrypted relay (Tailscale alternative, Paseo pattern, KB 4.19.1) — needed for users without Tailscale
- Worktree-based `LocalSandbox` Tier 0 (KB 4.19, open Q 18)
- Full Generator-evaluator Review tab per session (KB 4.18 Polyscope pattern)
- AGENTS.md / docs/ scaffold templates (KB 4.11 OpenAI pattern)
- Workspace blueprints (saveable templates, Factorio-style, KB 7.5)
- GitHub integration full surface (PR auto-fix loop, draft PRs, custom merge/PR prompts per repo)
- Linked workspaces (KB 4.18 Polyscope pattern)
- Skills marketplace (community-shareable, KB 4.5 BB pattern)
- Doc-gardening recurring agents (KB 4.11 OpenAI pattern)
- GPU-attached sandboxes (Modal pattern, KB 4.9; for ML/research workloads)
- Cloudflare Code Mode at MCP portal (KB 4.8) when MCP context-bloat warrants
- Cuartel mobile companion (iOS/Android via Expo or React Native)
- Personal AI Gateway features on the auth-gateway (per-user attribution, cost tracking, model catalog)

---

## MVP definition

**Cuartel is MVP when the cuartel team uses cuartel as their primary tool to build the next cuartel feature**, daily, for at least one week, without falling back to alternatives for things cuartel should handle.

### What MVP requires (everything in Phases A + B + C)

| Capability | Phase | Why required |
|---|---|---|
| `LocalSandbox` (claude-code-acp as host subprocess, no isolation) | B2 | Same isolation level as Zed/Polyscope/Paseo today; user-in-the-loop perms are the safety net |
| `claude-code-acp` ACP transport works end-to-end | A1 + B1 + B2 | Single agent provider must work cleanly |
| Session creation, persistence, restore across cuartel-app restarts | C2 | Without this, work is lost on every restart |
| Parallel sessions UI (sidebar + retained pool + notifications) | C2 | Multi-agent work is the product |
| `cuartel.json` workspace config + Tasks + portless integration | C3 | Real dev environments need real dev infrastructure |
| Two named Replicas (config-only Tier 0) per workspace | C3 | The Linear-vs-Asana feel — "your agents, configured" |
| CLI surface (`cuartel run/ls/attach/send/logs/wait`) | C4 | Headless / scripting / agent self-invocation |
| `--output-schema` for structured agent responses | C4 | Foundation for implement+verify shell loops |
| Minimal MCP server for sub-agent control (`replica.spawn`/`send`/`wait`) | C4 | Substrate for any orchestration |
| daemon + multi-client split | C1 | Without this, every Phase D/E/F feature retrofits painfully |
| sendPrompt hang regression test passing 5+ consecutive runs | A1 → ongoing | The bug that triggered the refactor |
| User permission UI for tool calls (PreToolUse-style approval) | C2 | The safety net for host-direct MVP — without VM isolation, the user is the boundary |

**Why VM isolation is NOT in the MVP (deferred to Phase D step D0):** every comparable product (Zed, Polyscope, Paseo, Cursor) ships interactive coding agents host-direct with permission UI as the safety net. VM isolation is genuinely valuable for **autonomous / scheduled / unattended / remote** work where the user isn't present to approve each tool call — and that's exactly when it ships (Phase D, where remote runtime + scheduled Replicas + "ship it" autonomous mode also land). For MVP interactive use, host-direct + user-approved tool calls is the right tier; sandbox-as-default would be over-engineering.

### What MVP does NOT require (deferred to Phase D/E/F)

- **VM-based sandbox isolation (`AppleVzSandbox`)** — Phase D step D0; opt-in for users who want it, automatic for autonomous Replicas
- Remote runtime (Hetzner) — Phase D
- Workspace move local↔remote — Phase D
- Background-session lifecycle on sleep — Phase D
- Birds-eye command-center view — Phase E
- Full Replica data model with triggers (cron, GitHub webhook) — Phase E
- Blueprint visual editor — Phase E
- Computer use (Tier 0/1/2) — Phase E (Tier 0 DOM-picker works on host; Tier 1 Playwright works on host; Tier 2 full desktop VNC requires VM)
- Voice — Phase E
- Multiplayer — Phase F
- LAN mode mDNS — Phase F
- E2E-encrypted relay — Phase F

### MVP success metrics (mechanical)

| Metric | Target |
|---|---|
| Days the cuartel team uses cuartel as primary tool | ≥ 7 consecutive |
| Sessions per engineer per day | ≥ 5 |
| PRs to cuartel repo where ≥60% of diff was cuartel-driven | ≥ 1 in the dogfood week |
| Falls-back-to-Polyscope/Paseo/Cursor/Claude-app for capability gaps | 0 (intentional choice OK; capability gap NOT) |
| sendPrompt hang regression test pass rate | 5+ consecutive runs, zero hangs |
| Session-spawn time p95 (`LocalSandbox`) | ≤ 1s |
| One-prompt-to-first-token p50 | ≤ 3s (claude-code-acp ready) |
| App-restart-to-session-list-visible | ≤ 2s |
| Cuartel-daemon memory under 6 parallel sessions | ≤ 1 GB |

After the MVP week, friction points triage into Phase D/E backlog. Anything that would have been "I can't dogfood this without it" moves into D/E from the F backlog.

---

## Test strategy

### Test levels

| Level | What | When it runs | Determinism |
|---|---|---|---|
| **Unit** | Per-crate, no external processes, no network | Every commit (CI <60s) | Pure functions; mocks for stdio |
| **Integration** | Cross-crate within cuartel; spawns real `claude-code-acp` child but stubs LLM responses with recorded fixtures | Every PR (CI <5min) | LLM calls replayed from fixtures |
| **E2E** | Full sandbox spawned (LocalSandbox in CI, AppleVz on macOS runners), real Claude API call, real workspace | PR + nightly (CI <30min) | Live LLM, small budget per CI run; idempotent prompts only |
| **Smoke** | One happy-path flow per shipped feature | Before every release | Live |
| **Acceptance** | Per-step DoD checks (the bullets above) | Gate for marking step complete | Mix |
| **Regression** | sendPrompt hang test, schema-backward-compat test, sandbox-leak test | Every PR | Deterministic |
| **Dogfood** | Cuartel team uses cuartel daily | Continuous, post-MVP | Live |

### Specific test pillars

1. **sendPrompt hang regression.** 50 consecutive `prompt → result` cycles via cuartel-acp. Zero hangs. Runs on PR. **The single most important test in the whole suite.**

2. **Schema-backward-compat.** Snapshot the WebSocket protocol Zod-equivalent schema at every release as a `protocol-snapshot.json`. CI runs: (a) deserialize a new daemon's responses with an old snapshot's parser; (b) deserialize an old daemon's responses with a new snapshot's parser. Both must succeed. **Mechanical version of Paseo's procedural rule** — cuartel-better-than-Paseo opportunity (KB 7.5).

3. **Sandbox lifecycle leak test.** Run 100 sandbox provision/spawn/dispose cycles in CI; assert zero orphan processes, zero leaked disk/network resources, zero memory growth in `cuartel-daemon`.

4. **Tool-call normalization invariants.** Property-test that every variant of provider tool names (`bash`/`Bash`/`shell`/`exec_command`) collapses to the canonical kind. Test all canonical kinds × all known variants.

5. **Identical-results test.** Same prompt + same workspace state → `LocalSandbox` and `AppleVzSandbox` produce structurally-equivalent tool-call sequences (modulo PIDs/timestamps). Catches sandbox-impl drift.

6. **portless integration smoke.** A real Vite app inside the sandbox image; assert reachable at branch-named URL from host browser; assert HTTP/2; assert WebSocket upgrade works.

7. **Workspace round-trip move.** Workspace files hash before and after a local→remote→local move; must match.

8. **Latency budgets.** ACP message round-trip (host↔Hetzner over Tailscale) ≤ 150ms p95. Sandbox cold-start ≤ 5s p95. App-startup-to-session-list ≤ 2s p95.

### Coverage targets

- `cuartel-acp` wire-protocol layer: ≥ 80% line coverage; tool-call normalization 100%.
- `cuartel-daemon` core (workspace/session/sandbox state machines): ≥ 70%.
- UI code (cuartel-app/GPUI): not measured by line coverage; smoke tests + dogfood instead.

### What we don't test

- LLM output quality (out of scope — the LLM is a black-box dependency).
- Polyscope/Paseo behavior (we don't depend on them).
- macOS UI animations / pixel-perfect rendering.

---

## CI / release / telemetry

### CI

- **Single trunk.** PR → CI → review → merge. No long-lived feature branches.
- **CI tiers:** unit + lint + format on every push (≤60s). Integration on every PR push (≤5min). E2E on PR + nightly (≤30min). Smoke before every release.
- **Required-status checks** on `main`: unit, lint, format, integration, schema-backward-compat, sendPrompt-hang regression.
- **Release gating:** smoke must pass; manual cut.

### Release

- **Cadence:** post-MVP, weekly cuts. Pre-MVP, ship when each Phase B/C step's DoD is green.
- **Channels:** stable (default for non-team users), beta (opt-in early adopters, gets 1-week jump), nightly (post-merge, internal team only).
- **Distribution:** macOS `.dmg` via cuartel-installer + Sparkle for auto-update. Future: Homebrew tap (`brew install cuartel`).
- **Rollback:** Sparkle supports downgrade; previous release stays available. If a release introduces a critical bug (sendPrompt hang reproduces, sandbox leak), revert via reverting the release on the update server within hours.

### Telemetry

- **Local logs always on** at `$CUARTEL_HOME/cuartel.log` (rotating, 10MB × 2). Never sent off-device by default.
- **Crash reports: opt-in** during onboarding. Anonymized stack traces only; no session content, no workspace paths, no env vars.
- **Performance metrics: opt-in.** Sandbox cold-start time, ACP RTT, session count distributions. No content.
- **Telemetry budget:** zero by default. Users explicitly opt in. If the user is opted in, we send aggregate per-user metrics once daily; no real-time stream.
- **Code or session content NEVER leaves the machine without explicit consent**, even with telemetry on. This is the local-first promise; breaking it breaks the trust story that's a major differentiator vs cloud-based competitors (Replicas, BB).

### Observability for cuartel team (separate from user telemetry)

- Dogfood: every cuartel-team member's `cuartel.log` is grepable via a shared script (their consent, our team only). Used to triage Phase D/E friction points.
- Aggregate sandbox cold-start metrics from team usage feed into the latency-budget regression tests.

---

## Migration plan (compressed view of the above phases)

The phases above are the actual migration plan. This compressed view is for quick reference (updated 2026-04-27 after the host-direct-MVP pivot):

```
A1   →   B1 → B2   →   C1 → C2 → C3 → C4 → C5   →   D0 → D1 → D2 → D3   →   E1 → E2 → E3 → E4   →   F (ongoing)
[1w]     [1.5+2 = 3.5w]   [2+2+2+1.5+1 = 8.5w]       [3+3+2+1 = 9w]            [3+3+3+3 = 12w]
                                              ↑
                                      DOGFOOD LINE (MVP)
```

Where:
- **A1** — confirm V8-vs-OS hypothesis (done; one run, zero hang).
- **B1+B2** — cuartel-acp Rust crate + LocalSandbox cutover. **No VM in MVP.**
- **C1–C5** — daemon split, persistence, cuartel.json+Replicas+portless, CLI+output-schemas+MCP-server, dogfood checkpoint. **End of C5 = MVP.**
- **D0** — `AppleVzSandbox` as opt-in secure mode. (Was originally B2; deferred until sandbox isolation earns its place via autonomous-work features in D and E.)
- **D1–D3** — HetznerSandbox + workspace move + background sessions.
- **E1–E4** — visual command center + Replicas v1 + computer use + voice + scheduler.

**Total to MVP (Phase A + B + C): ~13 person-weeks** of focused engineering for one engineer (down from ~16 in the original v2 — saved ~3 weeks by deferring `AppleVzSandbox`). With two engineers in parallel where steps allow, ~8-10 weeks calendar.

**Total to feature-complete v2 (through Phase E): ~34 person-weeks** for one engineer; ~22 weeks calendar with two engineers. Same as before — `AppleVzSandbox` work isn't deleted, just resequenced.

---

## Non-goals (this refactor)

- Custom MCP servers loaded from cuartel. MCP config pass-through to the ACP server is enough.
- ~~Multiplayer / shared sessions.~~ **Promoted from non-goal to "design for it now"** following the a16z "GUI for Agents" thesis. Tailscale + workspace-as-shareable-object makes the v3 implementation cheap *if* the data model is friendly to it from day one.
- Supporting agents that aren't ACP-compliant. (Pi's HTTP API: wrap it in a thin ACP shim, don't special-case in cuartel.)
- Collaborative buffers / real-time co-editing (Zed's `buffer_store` is explicitly out of scope).
- Cross-platform (Win/Linux host). Still Mac-only; GPUI and Virtualization.framework drive that.

---

## Patterns confirmed by industry convergence

Captured here so they're load-bearing in the design, not just notes elsewhere. Each is independently arrived at by ≥2 of {Ramp, Browserbase, Cloudflare, Modal, Vercel, Zed, OpenAI, Anthropic}.

1. **Snapshot-based fast cold start with diff storage.** Build a sandbox image periodically (cron, e.g. every 30 min), store snapshots as **diffs from a base image** (Modal pattern), boot from snapshot for sub-second-to-low-second startup. Required for the "kick off a session, see results in seconds" UX. Pairs with **block-writes-allow-reads-during-sync** for further perceived-latency wins (Ramp).
2. **Per-session compute isolation.** One sandbox per session, no cross-session contamination, ephemeral by default. Snapshot/restore for resume. Confirmed by every source.
3. **Credential brokering at the network layer with placeholder env vars.** Sandbox sees `OPENAI_API_KEY=credential-brokered-token`; egress proxy substitutes the real value transparently. Independently invented at Shuru, BB, Cloudflare, Anthropic Managed Agents. Cuartel's auth gateway already does this.
4. **Per-session SQLite (not one global DB) once parallel session count gets serious.** Ramp via Cloudflare Durable Objects; sustains hundreds of concurrent sessions. Cuartel can start with one DB; revisit when usage warrants.
5. **Subagent fan-out with quota.** When letting an agent spawn agents (real value-add for parallel research), **always** with: fresh context per child, status updates without context-exit, hard quota on count + spend. Modal/OAI SDK warning: "given async capabilities, the system can spiral into expensive overprovisioning without explicit guardrails." Carlini's 16-agent C-compiler validates the pattern at scale.
6. **GitHub App tokens scoped per-repo, not user OAuth tokens.** Separates approval rights from action rights — the agent can open PRs without ever being able to approve them under a human's identity (Ramp).
7. **Brain + Hands + Session as three pillars.** Anthropic Managed Agents commits to this OS-shaped abstraction explicitly: Brain (stateless harness), Hands (ephemeral sandbox, cattle-not-pets), Session (durable append-only event log queryable via `getEvents`). Cuartel's v2 originally collapsed Workspace+Session; **the next iteration must split Session out as a first-class durable object** so that (a) harness restarts replay from the log, (b) compaction/transformation can be applied between log and Claude's context, (c) we stay compatible with whatever Anthropic ships publicly.
8. **Lazy sandbox provisioning.** Anthropic dropped p50 TTFT 60% / p95 90% by deferring container creation until the harness actually calls a sandbox tool. `Sandbox::provision` should not allocate until first tool invocation, when feasible. Major UX win for the "kick off a quick prompt" path that doesn't need full compute.
9. **Agent legibility as the design north star** (OpenAI). "From the agent's point of view, anything it can't access in-context while running effectively doesn't exist." Push everything into the workspace-as-versioned-artifacts: `AGENTS.md` (~100 lines, table of contents), `docs/` system-of-record, plans as repo files, lint messages that teach. Cuartel can scaffold this template per workspace.
10. **Harness components encode assumptions about model gaps** (Anthropic). Continuously stress-test and remove components when models grow into them. Anthropic literally removed their sprint construct when Opus 4.6 handled decomposition natively. **Harness simplification is itself an engineering discipline.** Avoid baking workarounds for current-model limits as permanent architecture.
11. **Blueprints — orchestration as state machine of {deterministic, agent} nodes** (Stripe Minions). Each node either runs deterministic code (linters, push, lint loop) or an agent loop. Per-team customizable. Visualizable as a graph. Quote: "putting LLMs into contained boxes compounds into system-wide reliability upside." For cuartel: Blueprint becomes a first-class data type in step 5; a Replica references a Blueprint; the visual workflow editor (a16z thesis) renders Blueprints. **This is the substrate that makes "ship it" mode reliable.**
12. **Toolshed — centralized MCP server with per-agent curated subsets** (Stripe Minions, ~500 tools). All agents share one MCP server; each gets a small curated subset. Add a tool once, every agent gets it. For cuartel: a `cuartel-mcp-portal` shipped in step 5 prevents the multi-Replica MCP-fragmentation retrofit. Also the natural place to implement Cloudflare's Code Mode (collapse N tools into meta-tools) when context bloat warrants.
13. **No confirmation prompts when blast radius is contained** (Stripe Minions). Real-VM sandbox with no production access, no real user data, no arbitrary egress = full agent permissions are safe. Validates v2 D3. For autonomous mode: drop per-tool confirmation UX in favor of hard sandbox isolation.
14. **What's good for humans is good for agents** (Stripe + OpenAI). Pre-existing developer-productivity infra (devboxes, linters, rule files, observability stacks) generalizes to agents. Sandbox image bakes in fast linters, pre-push hooks, type-checking caches, observability — agents inherit it all. Avoid building agent-only infrastructure.
15. **Don't write the agent loop yourself.** Stripe forked block/goose. Cuartel adopts ACP. Both are the right call. The agent loop is a commodity; the value-add is everything around it.
16. **Per-workspace config file** (`cuartel.json` shape per Polyscope `polyscope.json`): `{ scripts: {setup, archive}, preview: {url}, tasks: [], replicas: [] }`. Setup/archive scripts run in the sandbox VM at workspace boot/teardown. `preview.url` with `{{folder}}` placeholder enables per-session previews. `tasks: []` for one-click reusable prompts. Distinct from `AGENTS.md` (config vs documentation). This is the standard pattern Polyscope's community has validated.
17. **Visual Editor as Tier 0 of computer use** — embedded webview + JS overlay that captures clicked element's selector + text + position + screenshot, sent to agent as structured context. **Vastly cheaper than VNC**, covers the most common frontend-edit case, and Polyscope already proved the UX works. Ships alongside Tier 1 (Playwright) and Tier 2 (full desktop) from KB 7.5.
18. **Generator-evaluator as a separate `Session`** (Polyscope's "Review" tab pattern). Same Workspace, second Session marked `role: Evaluator`, hard-coded review prompt prefix. The Activity tab and Review tab in the UI are two Sessions sharing the workspace. Pairs with Replicas (the evaluator can be a separate Replica) and with sprint contracts (the evaluator validates against the contract).
19. **Deterministic-hostname reverse proxy per branch + service** (Paseo's killer infra feature, made cheaper by portless). Each declared service in `cuartel.json` gets `https://<service>.<branch>.<workspace>.localhost`. WebSocket upgrades supported. Pairs with the existing port-forwarding infrastructure (phase 5e). **Eliminates port conflicts during parallel dev work.** **Implementation: depend on `portless` (npm, Apache-2.0, Vercel-maintained, KB 4.20)** rather than reimplement. Same logic as cuartel's "depend on shuru" decision (D6). Get HTTPS-by-default with system-trust CA + HTTP/2 multiplexing + framework-specific port/host injection (Vite/Astro/Next/Expo/RN) + `/etc/hosts` sync for Safari + `508 Loop Detected` UX + mDNS LAN mode for free. Cuartel-daemon spawns a Node sidecar that imports portless; `getRoutes` callback reads from cuartel's session/workspace store; ~1 day of integration. Bake portless into the sandbox VM image too (in-VM hostnames for in-VM services).
20. **Service-to-service env injection** (Paseo). Inject `$CUARTEL_SERVICE_<NAME>_PORT` and `$CUARTEL_SERVICE_<NAME>_URL` for every peer service in the workspace. Frontend points at `$CUARTEL_SERVICE_API_URL` instead of hardcoded port. Cuartel adopts as part of `cuartel.json` `scripts` section.
21. **Custom providers via config extension as Tier 0 of Replicas** (Paseo pattern). `agents.providers` config with `extends`, `label`, `env`, `models`, `disallowedTools`, `command` lets users get the named-agent UX through pure config — `claude-work` vs `claude-personal`, `claude-via-zai`, ACP-native `gemini`. **Ship config-only Replicas before building the full data model in step 5.** Faster path to the visible feature; full-fledged Replicas grow on top.
22. **Multi-pass Zod tool-call normalization** (Paseo `tool-call-mapper.ts:113-168`, KB 4.19.1). Provider tool names are messy (`bash` / `Bash` / `shell` / `exec_command` are all the same shell-exec tool). Collapse via discriminated-union pipeline to canonical kinds (`shell`, `read`, `write`, `edit`, `search`, `fetch`). **Build into `cuartel-acp` from day one** — UI needs canonical kinds for icons, treatments, permission checks. Hard to retrofit when N tool variants exist.
23. **MCP-server-as-daemon-API for sub-agent control** (Paseo `mcp-server.ts:318+`, KB 4.19.1). The daemon exposes an MCP server inside it that running agents call to spawn child agents (`agent.spawn`, `agent.send_prompt`, `agent.wait`). Caller-context inheritance: child inherits parent's mode, model, MCPs, system prompt, lockedCwd by default. **The substrate for Replica-spawning-Replica** (KB 7.8) and the subagent-pool-with-quota pattern (KB 4.9). Without this, agents can only orchestrate via shell-out to the CLI; with it, native programmatic spawn enabling visualizable workflows. Ship in step 5 alongside the MCP portal.
24. **Loop service as a first-class daemon primitive** (Paseo `loop-service.ts`, KB 4.19.1). Worker + verifier agents; each iteration spawns child agents until success criteria met. Verify can be **command-based** (run shell, check exit code) OR **LLM-based** (verifier returns structured output via `--output-schema`). Bounded by `maxIterations`. **This is the runtime substrate for `Blueprint`** (Stripe Minions pattern) — the "agent-loop-with-verification" subset that covers most real workflows. Cuartel's `Blueprint` data model from convergence pattern 11 is the visualizable graph; the Loop Service is its execution engine.
25. **Provider capability flags + graceful degradation** (Paseo `acp-agent.ts:92-99`, KB 4.19.1). Per-provider flags (`supportsStreaming`, `supportsSessionPersistence`, `supportsDynamicModes`, `supportsMcpServers`, `supportsReasoningStream`, `supportsToolInvocations`). Client checks before invoking; UI hides buttons for missing features rather than crash. **Required when supporting heterogeneous ACP servers** — different agents have different capabilities. Ship in `cuartel-acp`.
26. **Epoch-based timeline reset (no compaction)** (Paseo `agent-timeline-store.ts:130-191`, KB 4.19.1). Each agent run gets a UUID epoch. Client cursor includes epoch; if it differs from current, server returns `{reset: true, ...full timeline}`. **Beautifully simple** — no incremental compaction logic. Cuartel's session model adopts this for the timeline view; storage layer can still page if needed.

---

## Open questions

(Trimmed from 26 → 7. Resolved questions are now in **Decisions**, in **Phases & steps DoD**, or implicit in the architecture.)

1. **VM image build & distribution.** Who builds the Linux image with `claude-code-acp` + `portless` + Playwright + observability stack baked in? Where is it cached? **Recommendation:** GitHub Actions builds nightly and on tagged releases; published as OCI artifact to GitHub Container Registry; cuartel-daemon pulls on first use, caches locally. **Decision needed before B2.**

2. **VM cold-start time on Apple VZ.** DoD target is ≤5s p95. Likely feasible with snapshot-from-warm-image; if not, do we keep a warm VM pool? **Decision needed during B2.** Pre-warmed snapshots refreshed via cron (BB pattern, KB 4.5; Modal pattern, KB 4.9) is the fallback if pure-cold-start is too slow.

3. **Multi-repo workspaces.** ACP's `work_dirs` carries the list; VM image must mount N directories as `/workspace/{repo1,repo2,…}`. **Mount-spec format needs to be designed.** Probably: `cuartel.json` `workspace.repos: [{path, name, branch?}]`; daemon mounts each over virtiofs. **Decision needed during B2 / C3.**

4. **Encrypted workspace mount** (`workspace_mount.rs`): decryption on host (push plaintext over vsock) or in VM (push the key once)? **Security-review before picking.** Recommend: in-VM decryption — key crosses host↔sandbox boundary once at mount time, plaintext never touches the wire afterward. **Decision needed during B2.**

5. **Transport for remote stdio.** Bare SSH inside Tailscale (Zed pattern, simple, one process per session) vs framed-TCP daemon on the Hetzner VM (one long-lived process, lower per-session overhead). **Decision needed during D1.** Recommend: SSH-inside-Tailscale first; revisit if per-session process overhead becomes a real cost.

6. **Live migration of running sessions.** Currently scoped as Phase F. Is there a customer pull to bring it into Phase D? **Resolves during D2 dogfood.**

7. **MCP context efficiency: ship Code Mode early or defer?** Cloudflare collapsed 34 GitLab MCP tools (~15K tokens of definitions) into 2 portal-level meta-tools (KB 4.8). **Resolves based on dogfood-week MCP usage patterns.** If users wire many MCP servers, ship Code Mode in Phase E; if MCP usage stays modest, defer to Phase F.

### Architectural hedges (re-evaluate periodically, not actively decided)

- **v3 architectural hedge: agent on host vs in sandbox.** Vercel Open Agents puts the agent **on the host** (KB 4.10). We chose the inverse because claude-code-acp runs all tools in-process. **If a future ACP server ships an "FS-via-ACP" mode**, we could flip D2 — agent on host, sandbox swappable, providers freely interchangeable. **Re-evaluate every ~6 months** as the ACP ecosystem matures. No action needed unless landscape shifts.

- **Anthropic Managed Agents convergence.** When MA exposes a public API, cuartel should consume it as another transport for the same `AgentServer` interface. Design for transport-pluggability so {ACP, MA, hypothetical OpenAI Agent SDK} can coexist behind one cuartel UI. **No action until MA's public API surfaces.** (See KB section 7.7 for the strategic note.)

- **Rivet's role going forward.** If Rivet ships VM-backed sandboxes, do we adopt them as another `Sandbox` impl alongside Apple-VZ + Firecracker? **No commitment either way** — both fit the trait.

---

## References

Pinned for anyone implementing this.

- **ACP spec & Rust client**: `agent-client-protocol` crate (used by Zed at `zed/crates/agent_servers/src/acp.rs`).
- **Claude ACP server**: `github.com/zed-industries/claude-code-acp`. Key file: `src/acp-agent.ts`.
- **Zed project/worktree model**: `zed/crates/project/src/project.rs:213`, `zed/crates/worktree/src/worktree.rs:128`.
- **Zed thread model**: `zed/crates/agent/src/thread.rs:936`. Messages: `:123`. Tool permissions: `crates/agent/src/tool_permissions.rs:208`.
- **Zed remote-spawn pattern**: `zed/crates/agent_servers/src/acp.rs:505–528`. Interactive=No, command wrapped via `remote_client.build_command_with_options`.
- **Zed thread SQLite schema**: `zed/crates/agent_ui/src/thread_metadata_store.rs:1238–1308`.
- **Zed retained threads**: `zed/crates/agent_ui/src/agent_panel.rs:696`, cleanup at `:2068`.
- **Zed notification windows**: `zed/crates/agent_ui/src/ui/agent_notification.rs:10`.
- **Proliferate** (UX reference): `proliferate.com` — local↔cloud one-click workspace move.
- **Zed parallel agents** (UX reference): `zed.dev/blog/parallel-agents`.
- **Ramp Inspect** (background coding agent): `builders.ramp.com/post/why-we-built-our-background-agent` and `modal.com/blog/how-ramp-built-a-full-context-background-coding-agent-on-modal`.
- **Cloudflare internal AI engineering stack** (AI Gateway, Code Mode, AGENTS.md at scale): `blog.cloudflare.com/internal-ai-engineering-stack`.
- **Modal × OpenAI Agent SDK** (subagent pool with quota, snapshots-as-on-disk-memory, GPU sandboxes): `modal.com/blog/building-with-modal-and-the-openai-agent-sdk`.
- **Vercel Open Agents** (agent-outside-sandbox alternative architecture): `github.com/vercel-labs/open-agents`.
- **OpenAI Codex Harness Engineering** (100% agent-generated codebase, agent legibility, AGENTS.md as TOC): `openai.com/index/harness-engineering/`.
- **Anthropic harness design for long-running apps** (context anxiety, generator-evaluator, sprint contracts): `anthropic.com/engineering/harness-design-long-running-apps`.
- **Anthropic Managed Agents** (Brain+Hands+Session three-pillar abstraction; OS-virtualization framing): `anthropic.com/engineering/managed-agents`.
- **Anthropic Building a C Compiler with Claude — Carlini** (16-agent parallel coordination, file-locking task claiming, known-good oracle): `anthropic.com/engineering/building-c-compiler`.
- **Replicas** (autonomous coding-agent SaaS; brand inspires cuartel's "Replica" abstraction): `replicas.dev` + `docs.replicas.dev`.
- **Stripe Minions Parts 1+2** (Blueprints as orchestration state-machine, Toolshed centralized MCP, pre-warmed devbox pool, 1300+ unattended PRs/week): `stripe.dev/blog/minions-stripes-one-shot-end-to-end-coding-agents`.
- **Cursor cloud agents with computer use** ("agents control their own computers" — VM-resident desktop/browser, video artifacts, live remote control): `cursor.com/blog`, Feb 2026. Validates Tier 1/Tier 2 implementation plan in step 3 + 5 of this roadmap.
- **Polyscope** (the closest direct competitor — native macOS multi-agent coding, paid product, shipped feature parity for half of cuartel's vision): `getpolyscope.com` + `getpolyscope.com/docs`. Differentiation playbook in KB section 4.18: cuartel wins on real VM isolation + remote runtime + workspace mobility + strategy-game UX + ACP standard, not by replicating their feature list. Many small features worth lifting (`cuartel.json` config shape, Visual Editor, Autopilot story flow, Review tab, Tasks, plan-mode Clear-Context-and-Approve, CI auto-fix, per-repo merge/PR prompts, dev-env auto-detect).
- **Paseo** (most architecturally similar — open source, free, daemon + multi-client, mobile + desktop + web + CLI): `paseo.sh` + `paseo.sh/docs` + `github.com/getpaseo/paseo` (AGPL-3.0). KB section 4.19. Validates ACP adoption + remote-runtime + multi-provider config + voice-as-local-first. Surfaces the killer **deterministic-hostname reverse proxy per branch+service** infra trick (`web.fix-auth.my-app.localhost`) and the **daemon+multi-client** architectural split worth seriously considering (open question 17).
- **Paseo source-grounded deep dive** — KB section 4.19.1. Concrete file:line implementation references for: reverse proxy (`script-proxy.ts:38-145` + `script-hostname.ts` + `script-route-branch-handler.ts:17-68`), multi-pass tool-call normalization (`tool-call-mapper.ts:113-168`), MCP-server-as-daemon-API for sub-agent control (`mcp-server.ts:318+`), Loop Service as Blueprint substrate (`loop-service.ts`), epoch-based timeline reset (`agent-timeline-store.ts:130-191`), tweetnacl ECDH+AEAD relay (`encrypted-channel.ts:89-225`), provider capability flags (`acp-agent.ts:92-99`), custom 5-field cron parser (`schedule/cron.ts`). **AGPL implication: patterns are fair game, direct code copying isn't.**
- **portless** (Apache-2.0, Vercel Labs library + CLI for branch-named-URL reverse proxy): `github.com/vercel-labs/portless`, npm `portless`. KB 4.20. **Decision: depend on portless rather than reimplement** the reverse proxy. Get HTTPS-by-default with system-trust CA, HTTP/2 multiplexing, framework port/host injection, `/etc/hosts` sync, mDNS LAN mode for free. License-friendly (Apache-2.0); same dependency pattern as shuru.

For full digests of each source and how they map to cuartel decisions, see `KNOWLEDGE_BASE.md` sections 4 and 5.
