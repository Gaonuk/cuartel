# Cuartel Knowledge Base

> **Living document.** Captures project context, refactor decisions, learnings from external sources, and the conversation thread that produced them. Read this first in any new session — it resumes context without re-deriving it.

> Companion files: `ARCHITECTURE_REFACTOR.md` (v1, superseded), `ARCHITECTURE_REFACTOR_V2.md` (current target architecture), `DEBUGGING_NOTES.md`, `SPEC.md`.

---

## 1. The project

**Cuartel** is a 100% Rust native macOS application that orchestrates AI coding-agent sessions in isolated sandboxes. UI uses GPUI (Zed's GPU-accelerated framework) with Metal rendering. Local sandbox today is Rivet AgentOS sidecar at `localhost:6420`. Remote execution is over Tailscale to Hetzner. Includes an auth gateway that injects credentials on-the-fly into outbound API requests, SQLite vault encrypted with AES-256-GCM, and a workspace-mount system.

### Crate layout (current)

| Crate | Role |
|---|---|
| `cuartel-app` | GPUI binary; UI, menu bar, workspace/session views, terminal rendering |
| `cuartel-core` | Session lifecycle, API gateway, credential management, workspace mounting |
| `cuartel-rivet` | HTTP/WebSocket client for Rivet AgentOS |
| `cuartel-remote` | Tailscale connectivity |
| `cuartel-db` | SQLite + AES-256-GCM vault |
| `cuartel-terminal` | GPU-accelerated terminal rendering |

### Platform reality

**macOS-only by design.** Hard blockers for cross-platform: GPUI/Metal, `Info.plist`/`entitlements.plist`/`scripts/package.sh` produce `.app`/`.dmg`, `com.apple.security.virtualization` entitlement, `keyring` crate pinned to `apple-native`. Porting would mean swapping the UI toolkit, virtualization layer, keystore, and packager — effectively a rewrite of the host shell. **The VM-side workload is Linux already**, so that part is portable.

### Recent commits worth knowing

- `b6b14aa`, `935241e` — Phase 5f: firewall blocks private-address upstream authorities (VMs cannot reach credential storage).
- `97b5d49` — Phase 5e: port forwarding sandbox↔host, opt-in per port.
- `f7f09cb` — Phase 9a/b/d: register Claude Code harness, honor default agent choice.

### The pain that triggered the refactor

`sendPrompt` hangs silently. Root cause hypothesis: the Claude CLI is a grandchild V8 isolate inside Rivet AgentOS secure-exec, fighting the sandbox's polyfilled `net` / `child_process` / `fs` / TLS. One missing Node polyfill = silent hang. The architectural fix is to escape V8 nesting entirely.

---

## 2. The refactor (v2 summary)

Full doc: `ARCHITECTURE_REFACTOR_V2.md`. Compressed:

**Three independent axes, not two.**

| Axis | Values | Picked by |
|---|---|---|
| **Model** | Claude / Gemini / Codex / Pi / OpenCode | `AgentServerCommand` config (binary + args + env) |
| **Sandbox** | Apple VZ / Firecracker / E2B / Daytona / Modal / Local | `trait Sandbox` impl |
| **Runtime location** | Local (laptop) / Remote (Hetzner over Tailscale) | `RuntimeLocation` on `Workspace` |

**Boundary**: ACP wire protocol (JSON-RPC framed) between cuartel-app (host, trusted UI) and the ACP server (inside the sandbox, untrusted). Tool calls are loopback inside the VM — they never cross the host↔sandbox boundary.

```
cuartel-app (host, GPUI, trusted UI)
   Workspace registry · Sessions (SQLite) · ACP client · Permission UI · Auth gateway
              ▲ ACP wire (LocalStdio | RemoteStdio | FramedTcp)
              ▼
Sandbox (REAL OS — Linux VM, not V8 isolate)
   ACP server (claude-code-acp / gemini-cli) · model CLI · MCP servers · /workspace mount
   Outbound: firewalled allowlist, optional credential proxy
```

---

## 3. Key architectural decisions (with rationale)

### D1. Adopt ACP, don't invent a Harness trait

**Decision:** cuartel-app speaks ACP (Agent Client Protocol — `agent-client-protocol` crate, the same one Zed uses). No `trait Harness` in cuartel.

**Why:** ACP exists, is JSON-RPC over stdio, and every major coding agent (Claude, Gemini, Codex) ships an ACP server. Inventing a parallel Rust trait means writing an adapter per vendor. Adopting ACP = zero per-vendor cuartel code. Adding a model becomes shipping its ACP-server binary in the sandbox image.

**Source:** Zed `crates/agent_servers/src/acp.rs:229` (`AcpConnection`), `crates/acp_thread/src/connection.rs:47` (`trait AgentConnection`), `crates/acp_tools/src/acp_tools.rs:25` (wire format).

### D2. ACP server runs INSIDE the sandbox, not on the host

**Decision:** The ACP server (e.g. `claude-code-acp`) is installed in the sandbox image and spawned as an OS process inside the VM. cuartel-app speaks ACP to it over a transport.

**Why:** `claude-code-acp` runs all tools (Read/Write/Edit/Bash/Glob/Grep) locally to its Node process via the bundled Claude CLI — the `fs/read_text_file` / `fs/write_text_file` ACP RPCs exist (`acp-agent.ts:1234`) but the SDK never calls them. Co-locating agent and workspace makes tool calls loopback (zero-RTT). Avoids forking upstream SDKs to virtualize the FS.

**Source:** `claude-code-acp/src/acp-agent.ts:1673` calls `query()` from `@anthropic-ai/claude-agent-sdk` with `cwd`. All FS ops happen there.

### D3. Sandbox = real Linux VM, NOT V8 isolate

**Decision:** Drop AgentOS secure-exec from the agent runtime. Use real OS sandboxes only.

**Why:** The current sendPrompt hang is caused by V8 nesting (Claude CLI as V8 grandchild fighting polyfilled syscalls), not by *where* the adapter file lives. Lifting `claude-code-acp` into another V8 isolate solves nothing. A plain `node` OS process inside a Linux VM has no nesting problem.

**Concrete tech:**
- Local: **Apple Virtualization.framework** — entitlement is already declared in `entitlements.plist`.
- Remote: **Firecracker / Cloud Hypervisor / KVM** on Hetzner — trivial on Linux.
- Dev/CI: **`LocalSandbox`** (temp dir + process group, no isolation) for trait-shape verification only.

### D4. Workspace as first-class type above Sessions

**Decision:** Add `Workspace { worktrees: Vec<Worktree>, agent_servers, access_policy, runtime: RuntimeLocation }` above `Session`. Sessions are children of a workspace.

**Why:** Multi-repo work is real (frontend + API in one agent thread). Per-thread permission scoping needs a parent that owns the file tree + policy. Zed's `Project` / `WorktreeStore` model is the proven pattern.

**Source:** Zed `crates/project/src/project.rs:213` (`Project`), `crates/project/src/worktree_store.rs:185` (`WorktreeStore`).

### D5. RuntimeLocation as third independent axis

**Decision:** `RuntimeLocation = Local | Remote(endpoint)` is a property of the Workspace. Same code path; different transport.

**Why:** A user with their own Hetzner box wants the harness running there (next to the workspace), not on the laptop. Then ACP wire travels host↔Hetzner once per turn, but tool calls remain loopback inside the VM. Solves both latency (no per-tool RTT) and laptop-sleep continuity. This is the Proliferate "one-click move" pattern, made architectural.

### D6. Use Shuru as the Apple VZ implementation (depend, don't fork yet)

**Decision:** Depend on `shuru-vm` + `shuru-darwin` (Apache-2.0, github.com/superhq-ai/shuru) as the local microVM library. Pin to commit. Contribute upstream when missing pieces. Earn the right to fork.

**Why:** Shuru already wrote the painful parts (objc2 + Apple VZ bindings, KVO bridge, vsock + virtiofs setup, ephemeral cloning via `clonefile()`, TLS-MITM credential proxy). Reproducing this is weeks of zero-user-value work. Pre-1.0 risk is real but bounded.

### D7. Per-provider adapter for managed sandboxes

**Decision:** No common protocol with managed providers. Each (E2B, Daytona, Modal, Vercel Sandbox, Cloudflare Containers) gets its own `Sandbox` impl that translates to its SDK.

**Why:** Going managed loses vsock + virtiofs (local-VM superpowers). Each provider has its own transport (HTTP / WS / SSH / gRPC), file-mount API, and lifecycle. The `Workspace` / `Session` / ACP layers above stay unchanged; only `Sandbox` impl + `AcpTransport` variant differ. Order: local Apple VZ → self-hosted Hetzner → one managed provider as proof. **Avoid Cloudflare Workers** (V8 isolate; same trap as Rivet secure-exec). Cloudflare *Containers* (beta) is fine.

---

## 4. External sources (digested)

### 4.1 Zed (`github.com/zed-industries/zed`)

Read on `2026-04-24`. Sparse-checkout of `crates/{agent,agent_servers,agent_settings,agent_ui,acp_thread,acp_tools,project,worktree}`.

**Why it matters:** Same UI framework (GPUI), same language (Rust), open source, ships exactly the parallel-agent UX cuartel wants.

**Key patterns adopted (see file:line citations):**

- **ACP** as the harness wire protocol — `acp_tools.rs:25` (wire format), `acp_thread/connection.rs:47` (client trait).
- **Project / Worktree** model — `project/src/project.rs:213`, `worktree_store.rs:185`. Lifted as cuartel's `Workspace`.
- **Thread / message model** — `agent/src/thread.rs:936`, messages `:123`.
- **Tool permissions** — `agent/src/tool_permissions.rs:208` (`ToolPermissionDecision::{Allow,Deny,Confirm}`), `:20` (hardcoded security regex denylist for `rm -rf /` etc.).
- **Per-thread worktree scoping** — `acp_thread.rs:1040` (`work_dirs: Option<PathList>`), `agent_ui/src/thread_metadata_store.rs:301` (`WorktreePaths` main + linked).
- **SQLite thread schema** — `agent_ui/src/thread_metadata_store.rs:1238–1308`. `(thread_id, session_id, agent_id, title, created_at, updated_at, folder_paths, archived)`. Async write queue at `:463`.
- **Retained-sessions pool** — `agent_ui/src/agent_panel.rs:696`, eviction at `:2068`. `MaxIdleRetainedThreads=5`. Running sessions never evicted.
- **Notification windows** — `agent_ui/src/ui/agent_notification.rs:10`. Floating, cross-thread, auto-dismiss. Solves "thread A needs permission while user is on thread B."
- **Per-thread model selection** — `conversation_view/thread_view.rs:280` (`ModelSelectorPopover` per session).
- **Remote-spawn pattern** — `agent_servers/src/acp.rs:505–528`. `project.remote_client().build_command_with_options(..., Interactive::No)` transforms the ACP launch into an SSH invocation. Stdio stays local; agent process runs on remote host. Cwd is set only for local projects (`acp.rs:533`).
- **fs/read_text_file routing** — when the agent calls it, Zed validates path against worktrees (`acp_thread.rs:2601`) and opens via the buffer system — but cuartel won't replicate this since we don't have buffer infrastructure.
- **terminal/create routing** — Zed creates real PTY terminals locally (`acp.rs:3296–3354`); also "display-only" terminals stream agent-emitted output (`:3203`).
- **Subagent spawning** — `Thread::new_subagent` allows depth ≤ 1; child shares parent's project + action_log.

**Things to leave:** buffer_store / collaborative editing (Zed is a multiplayer editor, cuartel isn't); LSP integration; extension system (premature).

### 4.2 Proliferate (`proliferate.com`)

Marketing-only source. Single key concept: **runtime relocation** — "Send an entire workspace from your Mac to the cloud and back in a click. Files and session sync." Workspace identity persists, runtime moves. Promotes "harness placement" from internal abstraction to a user-visible verb. Adopted as `RuntimeLocation` + `Workspace::relocate(target)` operation.

### 4.3 claude-code-acp (`github.com/zed-industries/claude-code-acp`)

Anthropic's official ACP server for Claude Code (`@zed-industries/claude-code-acp`). Read on `2026-04-24`. Critical findings:

- **Runs all tools locally to its own Node process** (`src/acp-agent.ts:1673` — `query()` from `@anthropic-ai/claude-agent-sdk` with `cwd: params.cwd`).
- **`fs/read_text_file` / `fs/write_text_file` exist** (`acp-agent.ts:1234`) but **the SDK never calls them**. They're wired the right way but unused.
- **MCP servers spawn locally** (in the same process tree as the ACP server) — they inherit the sandbox automatically.
- **Permission flow is async + serialized** — `canUseTool()` blocks the SDK loop on the client's response.
- **Native CLI binary** is platform-specific NPM dep (`@anthropic-ai/claude-agent-sdk-${platform}-${arch}`).

**Implication:** Cannot virtualize FS by patching upstream. Must co-locate agent with files. (D2.)

### 4.4 Shuru (`github.com/superhq-ai/shuru`)

SuperHQ's open-source Rust microVM library. Apache-2.0. macOS via Apple VZ + experimental Linux KVM. Built explicitly for AI-agent sandboxing.

**Key observations** (see `shuru/crates/`):
- **Apple VZ wrapper:** `objc2 + objc2-virtualization` (no Swift, no FFI shim). `crates/shuru-darwin/src/vm.rs:85` wraps `VZVirtualMachine` with KVO. Same `com.apple.security.virtualization` entitlement cuartel already declares.
- **Kernel + rootfs:** minimal mainline ARM64 Linux, custom defconfig at `kernel/shuru_defconfig`. Initramfs.cpio.gz for fast boot. Rootfs cloned via `clonefile()` on macOS = instant CoW per run. Stored in `/instances/{uuid}/`.
- **Host↔guest wire:** AF_VSOCK on port 1024, framed binary protocol (`crates/shuru-proto/src/frame.rs`). Frame types: `ExecRequest`, `MountRequest`, `ForwardRequest`, `ReadFileRequest`, `WriteFileRequest`, plus streams `STDOUT/STDERR/STDIN/EXIT/RESIZE/WATCH_EVENT`.
- **Workspace mount:** virtiofs (not 9p, not rsync). Read-only by default with overlayfs (lower=virtiofs, upper=tmpfs) — writes discarded at shutdown. `--allow-host-writes` to bypass overlay.
- **Network policy:** off by default. `--allow-net` routes through `shuru-proxy` — userspace stateful firewall + per-domain allowlist + DNS↔IP cache + **TLS-MITM secret injection** (`--secret API_KEY=OPENAI_API_KEY@api.openai.com` → guest gets `shuru_tok_…` placeholder, proxy substitutes real value on outbound HTTPS).
- **Lifecycle:** one VM per `shuru run`. Cold-start optimized via initramfs + clonefile. Checkpoint feature for warm-state caching.
- **KVM backend** (`shuru-linux`): same trait as Darwin. ARM64-only, experimental. **Not Firecracker-compatible.**

**Adoption decision (D6):** depend on `shuru-vm` + `shuru-darwin` for `AppleVzSandbox`. Adopt `shuru-proto` as host↔guest wire. Steal `shuru-proxy` pattern for credential brokering (potentially retire cuartel's auth gateway). For Hetzner Firecracker, write our own — shuru-linux isn't the right fit.

### 4.5 Browserbase BB (`browserbase.com`, internal-agent architecture article)

Generalist agent in Slack ("@bb"). Routes feature requests, investigates sessions, writes PRs, etc. Lots of adoptable patterns even though their domain is internal ops, not coding-agent UI.

**Patterns worth adopting:**

1. **Pre-warmed sandbox snapshots refreshed periodically.** "Every sandbox starts from a pre-warmed snapshot that gets rebuilt every 30 minutes via a cron job: key repos cloned into `/knowledge/`, agent monorepo built with deps installed, OpenCode pre-installed and pre-started on a local port." → For cuartel: bake a fresh ACP-server image periodically so cold-start is "pull the delta, boot." Validates the open question on cold-start time.

2. **Skill system as markdown files, progressive disclosure.** ".opencode/skills/*.md inject domain-specific workflows on demand. Routing table in system prompt maps request patterns to skills." → Cuartel could ship a skill registry per workspace (or per agent server). Aligns with Anthropic's Skills primitive. Worth a future feature, not v1.

3. **Credential brokering at network layer with placeholder env vars.** "The sandbox firewall intercepts outbound HTTP requests to specific hosts and injects real API keys on egress. The sandbox env var is set to a placeholder string like 'credential-brokered'." → **Identical to Shuru's TLS-MITM proxy.** This is the same pattern, independently invented at two places. Strong signal it's the right design. Cuartel's auth gateway is already this; v2 keeps it as defense-in-depth.

4. **Two-layer access control: RBAC + ABAC (Agent-Based Access Control).** Service-level RBAC (Snowflake role read-only) PLUS per-session agent permissions (which `service.method` glob patterns allowed, which tools enabled). Defense in depth. → For cuartel: per-session permission config that bounds the agent regardless of model behavior. Aligns with Zed's `ToolPermissionDecision` pattern.

5. **Permissions correlate with invocation source.** Webhooks carry intent — a CRM-triggered run gets sales tools, not code access. Interactive Slack sessions get full access; the agent self-selects. → For cuartel: different session-spawn paths can carry different policy bundles.

6. **Sandbox keyed by thread ID in KV store, idle out at 30min, conversation persists in KV (resumable).** → Cuartel's sessions table is similar; add idle-eviction.

7. **Same agent loop, multiple invocation modes** (Slack interactive, webhook background, web UI). Same OpenCode core, different entry points. → For cuartel: future interface diversity (CLI, web, terminal) becomes orthogonal to the orchestrator.

8. **Service packages = typed wrappers around external APIs called through the proxy.** Keeps tool surface small, agent can do parallel calls and pre-transform results before context bloat. → Less applicable to cuartel (we're not building per-domain integrations), but the principle (small tool surface, pre-transform) generalizes.

**Industry signal from the article:** "Anthropic's Managed Agents and OpenAI's new agent SDK both separate the harness from the compute (filesystem/sandbox), something we've also found to be more performant overall." — direct vindication of v2's harness/compute split. We're moving with the industry direction, not against it.

### 4.6 Anthropic Managed Agents / OpenAI Agent SDK

Both endorse separating harness from compute. Cited by BB, consistent with Zed's ACP architecture, consistent with v2. Worth tracking their public SDKs as they evolve — they may converge with ACP or define a competing protocol that we should adopt or shim.

### 4.7 Ramp Inspect (builders.ramp.com + modal.com case study)

Ramp's internal background coding agent. ~30–50% of all PRs on their frontend/backend repos are written by Inspect. ~80% of Inspect itself is now written by Inspect. Multi-surface (Slack, web, Chrome extension, voice). Built on Modal sandboxes, OpenCode harness, Cloudflare Durable Objects for state.

**Patterns worth adopting:**

1. **Pre-warmed snapshots refreshed every 30 min via Modal Cron** — clones repos, installs deps, runs initial builds, snapshots filesystem. Snapshots stored as **diffs from base image** — only modified files persisted, fast and lightweight. *Confirms BB's pattern at scale, with concrete implementation: use diff-from-base, not full snapshots.*
2. **Block file edits until sync complete, but allow file reads in parallel.** Cold-start UX trick — agent starts research while remaining files finish syncing. Worth copying directly.
3. **Per-session SQLite databases on Cloudflare Durable Objects.** "Ensures high performance even when you have hundreds of sessions running in parallel." For cuartel: prefer per-session DB over one global DB if we expect parallel session counts in the dozens.
4. **Cloudflare WebSockets Hibernation** for low-cost streaming when sessions are idle.
5. **Chrome extension uses DOM/React internals to extract element trees** instead of screenshots. Token-cheap, semantic, dramatically reduces context cost. *General principle: prefer structured representations to pixels.*
6. **Subagent spawning** — "We added a tool that allows it to spawn sessions itself. Frontier models are smart enough to contain themselves." Parallel research across repos, incremental PR creation.
7. **Queue follow-up prompts rather than interrupting.** When the user sends a second message while agent is mid-turn, queue it. Default UX policy.
8. **VS Code server + web terminal + VNC + Chromium inside the sandbox** for human inspection / visual verification of frontend changes.
9. **GitHub App installation tokens scoped to repos**, not user tokens. "Preventing unreviewed code approval vectors" — separates approval rights from action rights.
10. **Voice input** as a real input modality, not a gimmick.
11. **"Kick off multiple versions of the same prompt and see which one lands"** — different prompts, different models, different temperatures. Compare and pick.

**Counter-intuitive choice:** OpenCode picked specifically because **it's open-source so the agent can read its own source** to ground its understanding instead of hallucinating capabilities.

**Industry signal:** Ramp explicitly cite the Modal compute / OpenCode harness split as the architectural unlock — same harness/compute split we adopted from Zed.

### 4.8 Cloudflare internal AI engineering stack (`blog.cloudflare.com/internal-ai-engineering-stack`)

Internal AI tooling at scale: 3,683 internal users (60% company, 93% R&D), 47.95M AI requests, 241B tokens routed in 30 days. Several patterns are *very* relevant to cuartel even though Cloudflare's stack is web-scale.

**Patterns worth adopting:**

1. **AI Gateway as universal proxy.** "Everything starts with Cloudflare Access, which handles all authentication and zero-trust policy enforcement. Once authenticated, every LLM request routes through AI Gateway." User emails mapped to anonymous UUIDs via D1+KV — "AI Gateway only ever sees the anonymous UUID, never the email." For cuartel: our auth gateway can grow into this — per-user attribution, cost tracking, model catalog, per-route policy. Same pattern, smaller scale.
2. **No API keys on user machines, ever.** Server-side credential injection is the only path. Validates v2's "secrets stay on host" decision and the BB/Shuru TLS-MITM pattern.
3. **Code Mode at the MCP layer.** Instead of exposing 34 GitLab tools that consumed "roughly 15,000 tokens of context window per request," they collapsed them into **two portal-level meta-tools: `portal_codemode_search` and `portal_codemode_execute`.** The agent writes code that calls the underlying APIs via the meta-tools. **Major context-budget unlock.** For cuartel: every additional MCP server today balloons tool count and context cost — Code Mode at our MCP-portal layer would compress this dramatically. *Possibly the single most actionable pattern in this batch.*
4. **AGENTS.md files generated at scale across 3,900+ repos.** Test commands, navigation patterns, coding conventions, boundaries. Reviewed and refined per-repo. For cuartel: first-class UI for editing per-workspace AGENTS.md/CLAUDE.md is a small feature with big payoff.
5. **Engineering Codex** — organizational standards as **agent skills.** "Network Firewall team used a multi-agent consensus process where every requirement was scored COMPLIANT, PARTIAL, or NON-COMPLIANT." Multi-agent consensus is a generalizable review pattern.
6. **Workers AI for cheap models on hot paths.** Their security agent processes 7B tokens/day on Kimi: "77% cheaper than mid-tier proprietary." For cuartel: we can support routing different cuartel-internal tasks (summarization, classification) to cheap local/edge models while reserving frontier models for the core agent loop. Future work.

### 4.9 Modal + OpenAI Agent SDK (`modal.com/blog/building-with-modal-and-the-openai-agent-sdk`)

Modal sandbox + OpenAI Agents SDK reference example for "Parameter Golf" (ML model training challenge). Pattern generalizes well beyond ML.

**Patterns worth adopting:**

1. **Explicit harness definition.** "An Agent is a for-loop with an LLM running tools. The set of tools and state you build around the core Agent loop is often called a 'Harness.'" Good vocabulary; aligns with v2 decisions.
2. **`SubAgentPool` — orchestrator manages multiple parallel subagents via a worker pool.** Subagents have **fresh context windows** — orchestrator stays lean, work splits into short bursts.
3. **`Hooks` track current active tool per subagent + `set_status` tool** — subagents update progress without exiting back to orchestrator. Decouples progress visibility from context flow.
4. **Quota system in the subagent pool.** "Fixed limit of expensive 8x H100s in use." **Prevents cost spiral when you give an LLM the power to spawn agents.** Critical guardrail.
5. **Filesystem snapshots as on-disk memory.** Snapshot the VM, branch subagents from it. "Artifacts from prior agents are implicitly available to successors through shared filesystem context." Decouples memory from session state.
6. **Skills plugins** for selective context loading. Same idea as BB's skills.
7. **`Capability` abstraction** — bind a set of tools to an instance/session.
8. **GPU as a sandbox property** — `ModalSandboxClientOptions` lets developers request GPUs alongside tools.

**Use-case insight:** the example is ML training, but the pattern (sandbox + harness + parallel subagents + quota + GPU) generalizes to **any long-horizon AI work needing compute** — research, optimization, large refactors, security audits, eval runs. *This is one of the more interesting product directions surfaced by this round of research.*

### 4.10 Vercel Open Agents (`github.com/vercel-labs/open-agents`)

Open-source reference app for background coding agents on Vercel. Three-layer split: **Web → Agent workflow → Sandbox VM**. Built on Vercel sandboxes + Workflow SDK.

**Patterns worth noting:**

1. **Agent runtime sits OUTSIDE the sandbox.** "The agent does not run inside the VM. It runs outside the sandbox and interacts with it through tools." *Different from cuartel's v2 decision (D2: ACP server inside sandbox)*. They get away with it because Vercel's sandbox API offers explicit `file/search/shell/task/skill/web` tools — a higher-level surface than ACP. Worth flagging as an alternative architecture: **higher-level sandbox SDK** vs **lower-level VM with ACP server inside.** Each has trade-offs (simpler vs. more flexible).
2. **Vercel Workflow SDK for durable, resumable agent runs.** "Active runs can be resumed by reconnecting to the stream. Sandbox lifecycle can hibernate and resume independently." Decouples agent execution from request lifecycle. For cuartel: we already get this via the long-lived ACP server pattern; Vercel needs Workflow SDK because their compute model is serverless.
3. **Sandbox snapshot + hibernate** for cost.
4. **GitHub OAuth + GitHub App** for repo cloning, branch work, **optional auto-commit + push + PR creation** after a successful run — "ship it" mode.
5. **Read-only session sharing via URL** for non-interactive review by stakeholders.
6. **Voice input via ElevenLabs.**
7. **Fork-and-adapt positioning.** "The repo is meant to be forked." Lower-friction reference than a black-box product. Cuartel could ship templates / starters in the same spirit.

**The interesting tension:** Vercel chose to keep the agent on the host because their sandbox abstraction was high-level enough. We chose ACP-in-sandbox because claude-code-acp doesn't route fs ops via ACP. **If a future ACP server (or a fork) properly routes fs/exec via ACP RPCs, the v2 decision could flip** — agent on host, sandbox is a dumb file/exec backend, swap providers freely. Worth tracking.

### 4.11 OpenAI Harness Engineering — Codex internal beta (`openai.com/index/harness-engineering/`)

OpenAI's most concrete public statement on harness design. Five months of building an internal product with **0 lines of manually-written code**. ~1M lines, 1,500 PRs, 7 engineers, 3.5 PRs/eng/day. Throughput **increases** with team size. Single Codex runs work autonomously for **6+ hours**. ~1/10 the time vs. hand-writing.

**Patterns worth adopting (most are concrete, not aspirational):**

1. **AGENTS.md as table of contents, NOT encyclopedia.** ~100 lines, points to a structured `docs/` directory. The "one big AGENTS.md" approach fails predictably: context is scarce, too-much-guidance becomes non-guidance, monolithic files rot, hard to verify mechanically.
2. **`docs/` as system-of-record with structured layout:** `design-docs/`, `exec-plans/active|completed/`, `tech-debt-tracker.md`, `generated/`, `product-specs/`, `references/`, `ARCHITECTURE.md`, `DESIGN.md`, `FRONTEND.md`, `PLANS.md`, `QUALITY_SCORE.md`, `RELIABILITY.md`, `SECURITY.md`. Plans are **first-class versioned artifacts**, not external trackers.
3. **Per-worktree boot.** "We made the app bootable per git worktree, so Codex could launch and drive one instance per change." Each session gets its own worktree, app instance, observability stack. Maps directly to cuartel's `Workspace::worktrees` and per-session sandbox.
4. **Per-worktree ephemeral observability stack.** Logs (LogQL), metrics (PromQL), traces — all torn down when the task completes. Agents query them as primary skills. For cuartel: the sandbox image could include a minimal observability stack so agents can answer "is this slow because of X?".
5. **Chrome DevTools Protocol as agent skill.** DOM snapshots, screenshots, navigation. The agent reproduces UI bugs and validates fixes itself. Generalizes to **structured observation** — give the agent semantic access to the running system.
6. **Custom lint error messages inject remediation into agent context.** Linters as agent feedback channels, not just human ones. The lint message *is* the teaching signal. Brilliant pattern: every constraint becomes self-explaining to the agent.
7. **"Doc-gardening" / "garbage collection" recurring agents.** Background agents scan for drift, update quality grades, open targeted refactor PRs. "Most can be reviewed in under a minute and automerged." Continuous tech-debt repayment, not annual cleanup.
8. **Layered architecture mechanically enforced** (Types → Config → Repo → Service → Runtime → UI; cross-cutting via single Providers interface). "This is the kind of architecture you usually postpone until you have hundreds of engineers. With coding agents, it's an early prerequisite."
9. **Minimal blocking merge gates.** Test flakes get follow-up runs, not blocking. "In a system where agent throughput far exceeds human attention, corrections are cheap, and waiting is expensive."
10. **Agent legibility as the design goal.** "From the agent's point of view, anything it can't access in-context while running effectively doesn't exist. Knowledge that lives in Google Docs, chat threads, or people's heads are not accessible to the system." Push everything into the repo. Versioned artifacts only. *This is the deepest lesson and reframes how to think about the codebase itself.*

**Counter-intuitive choice:** Reimplement small libraries rather than depend on opaque ones. "Rather than pulling in a generic p-limit-style package, we implemented our own map-with-concurrency helper." Boring/rebuildable > opaque/general.

**The "Ralph Wiggum Loop"** — Codex iterates on its own changes, requests further agent reviews, responds to feedback in a loop until all reviewers are satisfied. This is the multi-agent review pattern, applied to PRs.

### 4.12 Anthropic — Harness Design for Long-Running Apps (`anthropic.com/engineering/harness-design-long-running-apps`)

Most concrete article on long-running harness design from the model lab itself.

**Core definition:** "Every component in a harness encodes an assumption about what the model can't do on its own." Continuously stress-test those assumptions; remove components when models grow into them.

**Patterns worth adopting:**

1. **"Context anxiety" is real.** Models prematurely wrap up work as they approach perceived context limits, even when the task could continue. **Compaction (summarize-in-place) doesn't fix this — context resets via handoff artifacts do.** Clean slate, new agent instance, structured handoff document. Trade orchestration complexity for coherent long-duration execution.
2. **Generator-evaluator separation (GAN-inspired).** A separate evaluator agent reviews the generator's work. Critical insight: **"Tuning a standalone evaluator to be skeptical turns out to be far more tractable than making a generator critical of its own work."** Generators systematically overrate their output; the only reliable critic is an outside agent.
3. **Sprint contracts.** Generator and evaluator **negotiate testable success criteria before coding starts** (e.g., 27 explicit criteria for one sprint). Bridges high-level specs to verifiable implementation. Prevents scope creep mid-task.
4. **Files as inter-agent communication.** Decouples agent sessions and enables clean state transfer. Pairs with context resets — the handoff artifact is a file the next agent reads.
5. **Subjective taste codified into grading criteria.** Design quality, originality, craft, functionality — explicit rubrics applied by both generator and evaluator. **Transforms taste into gradable terms.** Cuartel could ship a per-workspace rubric.
6. **Feature-stubbing failure mode.** Generators often produce display-only implementations of complex features. QA evaluator catches these gaps and forces real implementation.
7. **Stress-test and remove harness components as models improve.** When Opus 4.6 arrived, Anthropic *removed* the sprint construct from one harness because the model handled decomposition natively. **Harness simplification is itself an engineering discipline.**

**Cost-quality framing:** Solo agent run, 20 min, $9 → broken gameplay. Full multi-agent harness, 6 hours, $200 → working features. Quantifies that harness investment is the difference between demo and product.

### 4.13 Anthropic — Managed Agents (`anthropic.com/engineering/managed-agents`)

**This is a hosted product, not a pattern document.** Anthropic explicitly framing the harness/compute split as an OS-level abstraction designed to outlast specific implementations. Direct parallel: "The `read()` command is agnostic as to whether it's accessing a disk pack from the 1970s or a modern SSD."

**Architectural shape — Brain / Hands / Session:**

- **Brain (Harness)** — stateless service. Calls sandboxes via `execute(name, input) → string`. Reads `getSession(id)` on restart. Emits durable events via `emitEvent(id, event)`. Resumes via `wake(sessionId)`.
- **Hands (Sandbox)** — ephemeral, **cattle not pets**. If a container fails, Claude gets a tool-call error and retries with a fresh instance. Sandbox is agnostic to the harness; could be containers, VMs, remote VPCs.
- **Session** — append-only event log. **"A context object that lives outside Claude's context window."** Queried via `getEvents()` for positional slices. The harness applies transformations (e.g. prompt-cache optimization) before feeding context to Claude — without baking assumptions into the session itself.

**Critical refinement for cuartel v2:** v2 has `Workspace + Session` (one object). Anthropic's split is `Workspace + Sandbox + Session` — three pillars. **Session as durable, immutable, queryable event log** is more powerful than v2's session-as-row-in-SQLite. Worth adopting.

**Performance claim:** "Our p50 TTFT dropped roughly 60% and p95 dropped over 90%" by deferring container provisioning until the harness actually calls a sandbox tool. **Lazy `Sandbox::provision`** is a meaningful UX win.

**Two credential patterns codified:**
1. **Bundled at sandbox init.** For git: clone the repo with a stored token, wire the remote, agent never sees the credential.
2. **External vault + proxy delegation.** For runtime tools: OAuth tokens in vault, dedicated proxy injects credentials server-side. Same as Shuru/BB/Cloudflare/cuartel's gateway.

**Quote that vindicates v2's threat model:** "In the coupled design, any untrusted code that Claude generated was run in the same container as credentials" — the structural fix is decoupling. Same insight that drove v2.

**Strategic note (covered in section 11 below):** Managed Agents could be a competitor or a substrate for cuartel. Cuartel doesn't have to *compete* with the brain+hands service — it can be **a great native client** to it. Possible long-term play: cuartel-app speaks ACP today and ACP-or-Managed-Agents-API tomorrow, with the workspace+UI+infra-portability as the differentiator.

### 4.14 Replicas (`replicas.dev` + `docs.replicas.dev`)

Y-Combinator-style coding-agent SaaS, March 2026. Direct competitor to cuartel on the **background-autonomous-coding-agent** axis. Honest read of their docs:

**What it is.** "Autonomous coding agent, designed for engineering-native platforms like Linear and GitHub. Tasks can be assigned to Replicas, and it can complete them just like an engineer would: spin up an isolated workspace (VM + code + dependencies), run apps locally, complete pull requests, react to comments and code reviews, handle CI/CD failures." Built on top of Claude Code / Codex / OpenCode — they orchestrate, they don't write the model.

**Architecture (inferred from docs):**
- **Workspace** = isolated VM with code + dependencies (same primitive as cuartel).
- **Environments** — separate concept (not fully documented publicly, suggests env-specific configurations).
- **Bring-your-own-subscription** — `replicas claude-auth` / `replicas codex-auth` via CLI authenticates to your existing Claude Code or Codex subscription. Or Anthropic API key. Or AWS Bedrock. *Lets users avoid double-paying for LLM compute.*
- **Multi-surface invocation:** Linear (assign issue to @Replicas), GitHub (`@tryreplicas` mentions + auto-responders for reviewer bots like `greptile-apps[bot]`), Slack (`@Replicas` mention), CLI, web dashboard, REST API at `api.replicas.dev`.
- **MCP support** + Automations. Repository Sets for grouping repos.
- Mintlify docs, Stripe-ish UX polish.

**What this means for cuartel.** Replicas is **not infra cuartel can integrate with** — they're a competitor in the same category. But the **brand name "Replica" is a powerful UX concept** that cuartel could adopt independently (no IP conflict; the noun "replica" is generic, and the metaphor is older than this product).

**Critical observation:** Replicas is web-based, multi-tenant SaaS, integration-driven (Linear/Slack/GitHub-first). Cuartel is native macOS, single-user/small-team, bring-your-own-infra (Apple VZ + Hetzner). **These are different products in adjacent markets.** Replicas competes for "team workflow integration"; cuartel competes for "native multi-agent command center." Both can win.

### 4.15 Stripe Minions — Parts 1 + 2 (`stripe.dev/blog/minions-stripes-one-shot-end-to-end-coding-agents`)

Stripe internal coding agent. **>1,300 minion-produced PRs per week** (up from 1,000 between Part 1 and Part 2). Fully unattended, one-shot. Slack-first. Built on a fork of Block's `goose`.

**Patterns worth adopting (this is one of the densest sources we've read):**

1. **Devboxes — pre-warmed cloud dev environments.** AWS EC2 instances, "cattle not pets," ready in **10 seconds** via a warm pool. Pre-loaded with code, Bazel/typecheck caches, code-gen services, repo clones. Engineers run "half a dozen at a time." **Critical insight:** "We built out devboxes for the needs of human engineers, long before LLM coding agents existed. As it turns out, parallelism, predictability, and isolation were also very desirable properties for LLM agents. **What's good for humans is good for agents.**"

2. **Blueprints — agent orchestration as state machine.** A blueprint is "a workflow defined in code that directs a minion run. Blueprints combine the determinism of workflows with agents' flexibility in dealing with the unknown: a given node can run either deterministic code or an agent loop focused on a task." Example nodes: agent-mode `Implement task` / `Fix CI failures`; deterministic-mode `Run configured linters` / `Push changes`. **Per-team customizable** — teams build blueprints for their specialized needs (LLM-assisted migrations etc.). *This is a different orchestration primitive than pure-workflow or pure-agent — and it's exactly what cuartel needs as a first-class concept for "how does this replica work?".*

3. **Toolshed — centralized internal MCP server.** ~500 MCP tools. **All Stripe agents** (minions, no-code internal agent builder, Slack bots, custom agents, third-party CLI agents) share the same MCP server. Add a tool once, all agents get it. **Per-agent curation:** each agent (minions included) gets a *small subset* of Toolshed tools, "tastefully curated"; users can opt into additional thematically-grouped tools for their own minions. Maps directly onto cuartel's MCP-portal idea (Cloudflare Code Mode, 4.8) but as a real implementation, not a future direction.

4. **Cursor rules as the cross-agent rule format** + sync to Claude Code format. "We standardized on a popular rule format that supported [conditional subdirectory rules]—Cursor's." Stripe doesn't fragment per-agent; they pick one format and adapt. For cuartel: the rule-format-compatibility story can be: "we read Cursor rules, AGENTS.md, CLAUDE.md, and feed them all to whatever ACP server runs."

5. **No confirmation prompts when blast radius is contained.** "The quarantined devbox environment means that the agent doesn't need confirmation prompts; any mistakes an agent might make are confined to the limited blast radius of one devbox, so we can safely run the agent with full permissions." Validates cuartel's v2: real-VM sandbox lets us drop the per-tool permission UX in autonomous mode.

6. **Forked an existing OSS coding agent (block/goose) instead of writing from scratch.** "We internally forked Block's goose—one of the first widely used coding agents—and customized it to work within Stripe's LLM infrastructure." Vindicates cuartel's "depend on shuru, fork later if needed" pattern (D6) and "adopt ACP, don't invent a Harness trait" (D1). **Don't write the agent loop yourself.**

7. **Shifting feedback left — pre-push lint hooks + background lint daemon precomputing rule heuristics.** "Sub-second lint feedback on push." Minions inherit this — "they don't have to waste tokens or CI minutes by iterating against an auto-formatter." For cuartel's sandbox image: bake in fast lint feedback so the agent doesn't burn turns on formatting issues.

8. **Bounded CI iteration — only one or two rounds.** "After a minion pushes a change, we run CI and auto-apply any autofixes for failing tests. If there are failures with no autofix, we send the failure back to a blueprint agent node and give the minion one more chance. After the second push and CI run, we send the branch back to its human operator." **Explicit cost/benefit tradeoff:** "diminishing marginal returns if an LLM is running against indefinitely many rounds of a full CI loop." Cuartel's "ship it" mode should similarly cap retries.

9. **Multi-entry-point invocation as a core feature.** Slack-tagging, internal-ticketing-UI button (e.g. "fix this flaky test"), feature-flag-platform integration, internal-docs-platform integration, CLI, web. *"Designed to integrate as ergonomically as possible with where Stripes are."* Strong reinforcement of the Ramp/BB pattern: meet users where they are.

10. **The blueprint flow is visualizable.** Stripe shows it as a diagram: rectangles for deterministic nodes, clouds for agent nodes. **This is exactly the visual workflow editor that Factorio-thesis cuartel could ship** — and Stripe has implicitly proven both the abstraction (state-machine of det+agent nodes) and the visual representation work in production.

**Quote that should be on every cuartel design doc:** *"In aggregate, we find that 'putting LLMs into contained boxes' compounds into system-wide reliability upside. Blueprint machinery makes context engineering of these subagents easy, whether that consists of constraining tools, modifying system prompts, or simplifying the conversation context as required for the subtask at hand."*

### 4.16 Cursor cloud agents with computer use (`cursor.com/blog`, Feb 2026)

Cursor shipped "cloud agents" with full VM environments where agents can drive their own desktop, browser, and produce video/screenshot artifacts. **>30% of internal merged PRs at Cursor are now created by cloud agents.** Demo cases:

- Building a feature, then **recording itself navigating the imported plugin and clicking each component** to verify GitHub source-link behavior.
- Reproducing a clipboard-exfiltration vulnerability: built an HTML demo, started a backend server in the sandbox, loaded the page in Cursor's in-app browser, recorded the full attack flow.
- Lint-label fix: tested two cases in Cursor desktop, recorded itself verifying the "No linter errors" state.
- 45-minute autonomous walkthrough of `cursor.com/docs` checking sidebar, top nav, search, copy page button, theme switching, etc.

**Key technical primitives implied:**
- Each agent gets an isolated VM with display server + browser + dev environment.
- Agent has tools to interact with the desktop / browser programmatically.
- Agent invokes recording start/stop; video is a deterministic artifact attached to the PR.
- Users can take over the agent's remote desktop themselves (live VNC-like control).
- Artifacts (videos, screenshots, logs) ship with the merge-ready PR.

**Multi-surface invocation reaffirmed:** "Available from anywhere you work, including the web, mobile, desktop app, Slack, and GitHub."

**Fit for cuartel:** all of this maps onto cuartel's architecture as **VM-image scaffold additions + MCP tool servers + UI panels**, with **no structural changes**. Concrete plan in section 7.5 ("Computer use / browser control / artifact recording"). Two-tier implementation: Tier 1 = Playwright + microsoft/playwright-mcp (browser-only, ~80% of value, ~weekend of work on top of step 3 + 5); Tier 2 = Xvfb + openbox + xdotool + x11vnc + custom desktop MCP (full desktop control + live VNC takeover). Local Apple VZ uses Xvfb software rendering (no GPU passthrough required); remote Hetzner same image, VNC port-forwarded over Tailscale.

**Strategic significance:** Cursor explicitly frames this as "the biggest shift in how we build software since the move from Tab autocomplete to working synchronously with agents." Validates the autonomous-background-agent direction and the visual-artifact UX. **Cuartel can ship this within v2's roadmap** — it's not a moonshot, it's an MCP server + a few VM packages.

### 4.17 Anthropic — Building a C Compiler with Claude (Carlini, `anthropic.com/engineering/building-c-compiler`)

Concrete proof-of-scale for parallel agent coordination. **16 Claude Opus 4.6 instances, 2 weeks, ~2000 sessions, $20K.** Result: 100K-line Rust C compiler that boots Linux 6.9 on x86/ARM/RISC-V, compiles QEMU/FFmpeg/SQLite/Postgres/Redis. 99% pass rate on most compiler test suites including GCC torture.

**Patterns worth adopting:**

1. **Infinite execution loop pattern.** "When it finishes one task, it immediately picks up the next." Bash loop wrapping Claude with explicit "break it into small pieces, track what you're doing, keep going until perfect."
2. **File-locking task claiming via shared filesystem.** Agents claim work by writing to `current_tasks/` files in a shared bare git repo. Lightweight coordination — no lock service, no queue, just files. Merge conflicts handled by Claude. Maps directly onto cuartel's potential subagent orchestrator.
3. **Known-good oracle pattern.** When the agents kept hitting the same monolithic kernel-compile bugs, switched to **using GCC as a known-good oracle** so each agent could work on different files independently. Generalizes: pair the agent with a working reference system to enable parallelism on the unknown bits.
4. **Role specialization across agents.** Dedicated agents for: dedup, perf optimization, code quality, docs maintenance. Different prompts, same loop. Parallelism via division of labor, not just division of files.
5. **Task verifier quality is critical.** "It's important that the task verifier is nearly perfect, otherwise Claude will solve the wrong problem." For cuartel's "ship it" mode, the test/lint pass criteria *are* the agent's optimization target.
6. **Context-pollution discipline.** "The test harness should not print thousands of useless bytes. At most, it should print a few lines of output and log all important information to a file." Tools the agent calls should default to terse output + log-file-on-disk for retrieval.
7. **Time-blindness fix via deterministic-random subsampling.** `--fast` runs 1-10% of tests, deterministic per-agent but random across instances. "Claude can't tell time and, left alone, will happily spend hours running tests instead of making progress." Cuartel's tooling layer could expose this primitive.
8. **Progress files as first-class.** "Maintain extensive READMEs and progress files that should be updated frequently." Same OpenAI insight — push state into the repo, where the agent can read it on every turn.

**Hard limits surfaced:** generated code lacks efficiency vs production compilers. Failed at 16-bit x86 compact codegen. Failed to reliably implement own assembler/linker. **"New features and bugfixes frequently broke existing functionality"** — agent teams work best with **well-partitioned tasks rather than tightly coupled features**.

### 4.18 Polyscope (`getpolyscope.com`) — the closest competitor we've found

Native macOS app from Beyond Code (the Laravel ecosystem company). Tagline: *"The time where humans write code is over. This is the new cockpit."* Built on Vue + Inertia + Laravel backend; macOS 13.3+. **Same product vision as cuartel** — native Mac, multi-agent parallel coding, workspace-per-task, rich UI — with different infrastructure choices. Already paid product (Paddle billing). Strong testimonial wall from the Laravel community.

**Storage and primitives:**
- `~/.polyscope/polyscope.db` (SQLite) + `~/.polyscope/clones/` (one per workspace)
- Workspace = **CoW clone of repo on host filesystem** (using `clonefile()` / equivalent) + new branch + agent session + random fun name (e.g. "brave-bunny", "golden-raven")
- One special **base-repository workspace** allowed (agent works directly in user's checkout — useful for live dev-server scenarios)
- Login with GitHub (mandatory)
- Supports **vendor CLIs as agent backends:** Claude CLI, Codex CLI, Cursor CLI. No ACP.

**`polyscope.json` config file (in repo root):**
```json
{
  "scripts": { "setup": "...", "archive": "..." },
  "preview": { "url": "https://{{folder}}.test" },
  "tasks": [{ "label": "Security review", "prompt": "..." }]
}
```

**Per-repo settings:** display name, default model, review model, review preference, merge prompt, PR prompt.

**Feature surface (all shipped):**

1. **Visual Editor** — embedded browser with element picker. Click any DOM element on the preview, describe the change in plain language; agent receives the element selector + position + your text, makes the actual code change. **Not VNC — DOM-driven.** Massively cheaper than full computer-use. Cuartel's "Tier 0 visual" should look exactly like this.
2. **Autopilot** — describe a high-level goal → AI generates user stories (US-001, US-002, ...) with title/description/acceptance criteria → user can edit/reorder/delete via drag-and-drop → sequential execution in fresh agent sessions → progress recorded in `.context/progress.md` for next story → crash recovery (paused state on restart). **This is Anthropic's sprint-contract pattern (4.12) operationalized as a UI feature.**
3. **Review** — separate agent session, opens its own tab next to "Activity." Optimized for diff critique using `GetWorkspaceDiff`, `AGENTS.md`/`CLAUDE.md` compliance, `gh` PR context. Per-repo review preference + review model. **This is the generator-evaluator pair (Anthropic 4.12) operationalized as UX.**
4. **Opinions** — multiple models answer the same question, then synthesize a consensus. **This is Cloudflare's multi-agent consensus (4.8) shipped as a user-facing feature.** Page wasn't accessible publicly but referenced in Review docs as the multi-model alternative.
5. **Linked workspaces** — give an agent access to another workspace's context (read-only). Per-prompt link. Use cases: cross-repo coordination, supervisor pattern, cross-referencing prior solutions.
6. **Tasks** — reusable prompts in `polyscope.json`. One-click run from sidebar; spawns a fresh workspace with the task prompt auto-sent. **Lighter than skills — easier to grok.**
7. **Plan mode** — agent proposes a plan before coding. Approval dialog has "Approve" + **"Clear context & Approve"** (start fresh agent session with only the approved plan — cuts context bloat).
8. **GitHub integration** via `gh` CLI: create workspace from issue, create from PR, create PR (or draft PR), monitor CI checks, **auto-fix CI failures** by re-prompting the agent with the failure context. Custom merge/PR prompts per repo.
9. **Nightwatch integration** (Laravel's APM) — error/perf issues become workspace seeds with full context (stack trace, endpoint, etc.).
10. **Laravel Herd integration** — auto-detects Herd, suggests `polyscope.json` with `herd link/secure/unlink/unsecure` scripts and `https://{{folder}}.test` preview URL. **Per-workspace dev environment via host integration.** Generalizable: similar auto-detection for Vite, Next.js, Rails dev servers.
11. **Built-in browser/preview** (`Cmd-P`) with element picker, console, split view, navigation history.
12. **Built-in terminal** (`Cmd-`) per workspace.
13. **Built-in diff viewer** (`Cmd-D`) — unified or split view, inline comments.
14. **File picker** (`Cmd-P`) — fuzzy search.
15. **Rich command palette** (`Cmd-K`) — fuzzy across all commands and workspaces.
16. **Hotkey workspace switching** — `Cmd-1..9` by position, `Cmd-Tab` cycle.
17. **Slash commands** in prompt input (e.g. `/clear` to reset agent context preserving workspace links).
18. **File mentions** with `@`, **image attachments** (drag/drop or paste), **per-prompt model selection**.

**The closest-competitor analysis (CRITICAL):**

| | Polyscope | Cuartel (v2 plan) |
|---|---|---|
| **Sandbox** | CoW clone on host FS — **no isolation** | Real Linux VM (Apple VZ + Hetzner Firecracker) |
| **Agent transport** | Wraps vendor CLIs (Claude CLI, Codex CLI, Cursor CLI) | ACP standard |
| **Stack** | Laravel + Vue + Inertia (web tech in a Mac app) | 100% Rust + GPUI (native, GPU-rendered) |
| **Remote runtime** | None visible (local only) | First-class (Hetzner via Tailscale) |
| **Workspace mobility** | None (local clones only) | Designed for local↔remote moves (Proliferate-style) |
| **Visual element selection** | **Shipped (Visual Editor)** | Roadmap (KB 7.5 computer-use Tier 1) |
| **Multi-version consensus** | **Shipped (Opinions)** | Roadmap |
| **Autopilot / sprint UI** | **Shipped (Autopilot with stories)** | Roadmap (Replicas + Blueprints, 7.8) |
| **Generator-evaluator UX** | **Shipped (Review tab)** | Roadmap |
| **Birds-eye command center** | No (conventional sidebar+chat) | **Roadmap (a16z thesis 7.6)** |
| **Strategy-game UX** | No | **Roadmap** |
| **Pricing model** | Paid (Paddle), already revenue-generating | Pre-product |

**Strategic implications for cuartel — sharp differentiation areas:**

1. **Real VM isolation is a genuine safety story.** Polyscope's CoW-clone-on-host means a misbehaving agent can `rm -rf ~/important-stuff`. Cuartel's VM isolation is a meaningful security advantage and easy to communicate ("your agents can't escape their box"). Especially relevant for long-running unattended sessions.
2. **Remote runtime + workspace mobility is the wedge.** Polyscope is local-only. The user with a Hetzner box can't move long-running work there. Cuartel's `RuntimeLocation = Local | Remote` + Proliferate-style move is unique.
3. **Strategy-game command center vs conventional sidebar.** Polyscope's UX is excellent but conventional (sidebar of workspaces, chat panel, tabs). Cuartel's a16z-thesis command center (birds-eye view, hotkey groups, drag-to-assign, Replicas as units) is a different aesthetic and a real differentiator.
4. **ACP > vendor CLI wrapping.** Polyscope must update for each vendor's CLI changes; cuartel adopting ACP gets new agents "for free" if they ship an ACP server. Long-term this matters more.
5. **Bring-your-own-Hetzner-with-GPU** for ML/research workloads. Polyscope can't do this; cuartel can.

**Lessons to STEAL from Polyscope (high-confidence wins, all shipped at scale):**

1. **`{config}.json` config file in repo root** — `setup`, `archive`, `preview.url`, `tasks`. Cuartel adopts this pattern as `cuartel.json` (or fold into AGENTS.md / similar). The `{{folder}}` placeholder for the workspace name is generalizable.
2. **Tasks** — one-click reusable prompts. Lighter than Skills (which need progressive context loading). Cuartel ships `tasks: []` in workspace config; sidebar dropdown spawns a fresh workspace with the task auto-sent.
3. **Visual Editor (DOM-element-picker → agent context)** — vastly cheaper than VNC for frontend work. Cuartel adds this as a Tier 0 alongside Cursor-style computer-use (KB 7.5). Embedded webview + a small JS overlay that captures the selected element's selector/text/position.
4. **Autopilot story flow** — high-level goal → generated stories → drag-to-reorder → sequential execution → progress.md handoff → crash recovery. *This is the visual UI for Anthropic's sprint contracts (4.12) and Stripe's blueprint workflows (4.15) combined.* Cuartel's Replicas+Blueprints (7.8) should learn from this implementation.
5. **Review tab as separate session** — distinct from main chat, optimized for diff critique with built-in instructions. Operationalizes generator-evaluator (Anthropic) as a one-click feature.
6. **Opinions** (multi-model consensus) — Cloudflare's pattern (4.8) shipped as a UI feature. Cuartel should ship this.
7. **CI auto-fix loop** — when GitHub Actions fails, agent gets re-prompted with the failure. Bounded retries (Stripe pattern). Cuartel's "ship it" mode default.
8. **Linked workspaces** — read-only context sharing across workspaces. Useful for cross-repo coordination + supervisor patterns. Smaller scope than full workspace move; complementary.
9. **Per-repo merge/PR prompts** — small but high-value detail. Cuartel adds per-Workspace prompt overrides.
10. **Plan mode with "Clear context & Approve"** — context-reset (Anthropic 4.12) operationalized at the plan-approval step. Cuartel's plan mode should offer this.
11. **One-base-repo workspace mode** — agent works in user's actual checkout (no clone), for live dev-server scenarios. Useful escape hatch from the always-clone default.
12. **Crash recovery** — Autopilot resets in-progress story to pending on restart, enters paused state, no work lost. General principle for any long-running cuartel automation.

**Polyscope is the project cuartel needs to beat directly.** They've shipped a polished v1 of half cuartel's vision, with a solid Laravel-community foothold and paid revenue. Cuartel wins by going **deeper on infrastructure** (real VMs, remote runtime, workspace mobility) **and bigger on UX** (visual command center, Replicas as units, strategy-game affordances) — not by replicating their feature list.

### 4.19 Paseo (`paseo.sh`) — the most architecturally similar project we've found

Open-source, free, self-hosted multi-agent coding orchestrator. Built by an independent dev ("Mo"). 4.7k GitHub stars. Daemon + multi-client (mobile + desktop + web + CLI). **The architecturally closest project to cuartel** — and they've already shipped half of what v2 plans, validating many bets and surfacing several brilliant features cuartel should steal.

**Architecture (daemon + clients, like Docker):**
- **Daemon** = local server managing agents. Runs anywhere: laptop, Mac Mini, VPS, Docker container. `npm install -g @getpaseo/cli && paseo` starts it headless.
- **Clients** = mobile (iOS, Android), desktop, web (`app.paseo.sh`), CLI — all connect over WebSocket.
- **Two connection modes:**
  - **Relay (recommended):** daemon connects outbound to a relay server; clients meet it there via QR-code pairing. **End-to-end encrypted: ECDH key exchange + AES-256-GCM.** Relay sees only IPs, timing, message sizes — cannot read traffic, forge messages, or replay. Daemon's persistent ECDH keypair lives at `$PASEO_HOME/daemon-keypair.json`.
  - **Direct:** daemon listens on `127.0.0.1:6767` by default (or Tailscale IP, or Unix socket file for max isolation). DNS-rebinding protection via `daemon.hostnames` allowlist.
- **Storage:** `~/.paseo/` (configurable via `PASEO_HOME`). `~/.paseo/config.json` for everything; `~/.paseo/worktrees/<source-hash>/<slug>/` for agent worktrees; `~/.paseo/models/local-speech/` for voice models.

**Providers (subprocess wrapping + ACP):**
- First-class: `claude` (Claude Code), `codex`, `opencode`, `copilot` (via ACP), `pi`. Discovered automatically when CLI is installed and authenticated.
- **Custom providers via `agents.providers` config** — extend any first-class provider:
  - Point Claude at Z.AI (GLM models) via `ANTHROPIC_BASE_URL`
  - Point Claude at Alibaba/Qwen coding plan via Anthropic-compatible endpoint
  - **Multiple profiles:** `claude-work` and `claude-personal` as separate config entries with different API keys
  - Override binary path (nightly build, Docker image, wrapper script)
  - Per-provider `disallowedTools`, custom model lists, `additionalModels` (merge with discovered)
- **ACP-native agents:** `extends: "acp"` + `command` array. Examples shown: Gemini CLI (`gemini --acp`), Hermes. Paseo sends `initialize` JSON-RPC; agent reports capabilities/modes/models at runtime. **They've adopted ACP exactly the way cuartel plans (D1).**

**Worktrees as the isolation primitive:**
- Each agent runs in its own git worktree under `$PASEO_HOME/worktrees/<source-hash>/<slug>/`.
- `paseo.json` at repo root configures: `worktree.setup`, `worktree.teardown`, `worktree.terminals[]`, `scripts: { name: { command, type: "service"?, port? } }`.
- `setup` runs once after worktree creation (a fresh worktree has no installed deps and no ignored files like `.env`); `$PASEO_SOURCE_CHECKOUT_PATH` points to the original repo root for copying configs.
- `teardown` runs on archive.

**Killer feature: deterministic-hostname reverse proxy per branch + service:**
- Each declared service gets a URL: `http://<script>.<branch>.<project>.localhost:<daemon-port>`. On default branch, the branch label is dropped.
- `*.localhost` resolves to 127.0.0.1 on modern OSes — works out of the box.
- WebSocket upgrades supported.
- Each service gets `$PASEO_PORT` (its own assigned port; bind here instead of hardcoding) and **peer-discovery env vars `$PASEO_SERVICE_<NAME>_PORT` and `$PASEO_SERVICE_<NAME>_URL`** for every other service in the workspace. Frontend points at `$PASEO_SERVICE_API_URL` instead of hardcoding `localhost:8080`.
- **Eliminates port conflicts during parallel dev** (each branch's web server gets its own URL). No other product surveyed has this.

**Local voice stack (impressive privacy story):**
- STT: **Parakeet TDT 0.6B v3 INT8** (ONNX on CPU). Default model.
- TTS: **Kokoro-en v0_19** (ONNX on CPU). Default speaker 0.
- Voice LLM orchestration: **hidden agent session** using your configured provider (e.g., Claude Haiku for fast voice responses).
- Tooling path: MCP stdio bridge for voice tools and agent control.
- Optional OpenAI fallback for higher-quality STT/TTS.
- Models downloaded on first daemon start. **All local by default — speech never leaves your network.**

**CLI as first-class control surface:**
- `paseo run "fix the tests"` — start agent (waits for completion). `--detach` for background. `--worktree feature-x` for isolated git worktree. `--provider codex`. `--output-schema schema.json` for structured JSON responses.
- `paseo ls`, `paseo attach <id>` (stream output), `paseo send <id> "follow-up"`, `paseo logs <id> -f`, `paseo stop`, `paseo wait`.
- `paseo permit ls / allow / deny` for permission UX.
- `paseo agent mode <id> bypass|plan|...` for provider-specific modes.
- **Designed to be used BY agents themselves** — enables hierarchical multi-agent workflows. Documented implement+verify loop pattern: Codex implements, Claude verifies via `--output-schema`, `jq` parses the verdict boolean, loop until criteria met.

**Built-in scheduler (cron):**
- `paseo schedule create --cron "0 9 * * 1" "audit the codebase for security issues and open PRs for fixes"`
- `paseo schedule ls / pause / delete`
- **This is "Replica triggers" shipped as a feature.**

**Other notable features:**
- **Output schemas** (`--output-schema`) enable structured agent responses for scripting.
- **Image attachments** in CLI: `paseo send <id> --image screenshot.png "what's wrong here?"`
- **`--name <name>`** for named agent IDs (replaces uuids in scripts).
- **Pairing via QR code** for mobile clients.
- **Headless multi-instance:** `PASEO_HOME` to run multiple isolated daemons.
- **Orchestrator UI** (visible in landing page mockup): named workflow with `Run plan-technical (codex)`, `Run plan-design (claude)`, `Wait for agents`, `Run implement (codex)`, `Run review (claude)` — visual workflow with parallel agent invocations and explicit wait points. Likely an in-app feature; docs page wasn't accessible.

### Architecture comparison: Paseo vs Cuartel

| | Paseo | Cuartel (v2) |
|---|---|---|
| Architecture | **Daemon + multi-client** (Docker-style) | GPUI app with embedded sandbox runner |
| Isolation | git worktree per agent (no VM, host filesystem) | Real Linux VM (Apple VZ + Hetzner Firecracker) |
| Agent transport | Subprocess wrap of CLIs + ACP for ACP-native agents | ACP standard |
| Multi-provider | **Config-only** (`agents.providers` extends first-class) | Through `AgentServerCommand` + Replicas (planned) |
| Multi-platform | **iOS + Android + Desktop + Web + CLI** | Mac-native only |
| Remote runtime | **Daemon on VPS / Mac Mini / Docker** | Hetzner via Tailscale |
| Connection | **E2E-encrypted relay** OR direct (Tailscale, raw, Unix socket) | Tailscale only |
| Voice | **Local-first ONNX (Parakeet + Kokoro)** + optional OpenAI | Roadmap (KB 7.5) |
| Reverse proxy / port mgmt | **Deterministic hostnames per branch + service** | Roadmap (port forwarding exists, no auto-allocation) |
| Scheduler | **Built-in cron** (`paseo schedule`) | Roadmap (Replica triggers) |
| CLI | **Comprehensive, designed for agent self-invocation** | Mostly UI-focused |
| Output schemas | **First-class** (`--output-schema`) | Not planned |
| Per-workspace config | `paseo.json` (services + setup/teardown + terminals) | `cuartel.json` planned (similar shape — KB 4.18) |
| Open source / free | **Yes, GPL/MIT TBC** | TBD |
| Pricing | Free (Sponsor on GitHub) | TBD |

### Brilliant features cuartel should STEAL (high-confidence, all shipped)

1. **Deterministic-hostname reverse proxy with branch-aware routing.** `http://web.fix-auth.my-app.localhost:<daemon-port>`. On default branch, the branch label is dropped. WebSocket upgrades. **The single most clever infrastructure feature in this whole research pass.** Solves the port-conflict problem that plagues parallel dev work. Cuartel implements this *inside the sandbox* (Tier 1) AND on the host for sandbox port-forwarding (Tier 2). Pairs perfectly with cuartel's existing port-forwarding (phase 5e) — instead of just forwarding raw ports, allocate hostnames per workspace × service.

2. **Service-to-service env injection.** `$PASEO_SERVICE_<NAME>_PORT` and `$PASEO_SERVICE_<NAME>_URL` for every peer service. Frontend points at `$PASEO_SERVICE_API_URL` instead of hardcoded port. **Generalizes massively for cross-service dev work.** Cuartel adopts as part of its `cuartel.json` `scripts` section.

3. **Daemon + clients architecture.** Even for a Mac-only product, the separation enables: headless server use (run on a Mac Mini), mobile companion later, multiple-machine setups, lower coupling. **Worth seriously considering for cuartel** — split `cuartel-app` into `cuartel-daemon` + `cuartel-app` (GPUI client). Less work than it sounds because v2 already has clean Workspace/Sandbox/Session abstractions; the daemon just owns state, the client just renders.

4. **Custom providers via config extension** (Paseo's `agents.providers`). Instead of building Replicas as a full data-model addition first, **ship config-based provider extension as a Tier 0 Replica**: extend Claude with a different API endpoint, a different name, different env vars. Examples: `claude-work` vs `claude-personal`, `claude-via-zai`, `claude-via-bedrock`. Replicas as a first-class concept can grow on top of this. *Faster path to "the named agents UX" without fully implementing 7.8.*

5. **Built-in cron scheduler.** `paseo schedule create --cron "0 9 * * 1" "audit the codebase"`. Brilliantly simple. Cuartel ships as part of Replica triggers (KB 7.8) — but the same primitive can ship before Replicas as a standalone scheduler.

6. **`--output-schema` for structured agent responses.** Enables scripting + multi-agent workflows. The implement+verify loop they document (Codex implements, Claude returns `{criteria_met: bool}`, loop until true) is a foundation pattern. Cuartel CLI/API exposes this.

7. **Local voice stack with ONNX models.** Parakeet (STT) + Kokoro (TTS), both CPU-only ONNX. **Privacy/independence story for free** (no cloud round-trip for speech). Hidden agent session for voice LLM orchestration is a clean abstraction. Cuartel ships the same — `Replica { voice_model: Local | OpenAI }`.

8. **End-to-end encrypted relay model** (ECDH + AES-256-GCM, QR-code pairing). For users without Tailscale or who want zero-config remote access. **Cuartel could offer this as an alternative to Tailscale-only remote.** The relay is untrusted by design — even if compromised, can't read traffic. Substantial competitive feature.

9. **Worktrees as the cheap-path isolation tier.** For users who want to skip VM overhead, worktree-based isolation is a valid Tier 0. Polyscope uses this; Paseo uses this. **Cuartel could offer worktree as a `LocalSandbox` mode** — no isolation, just CoW + branch separation. Even users with full VMs available may want it for tiny tasks.

10. **`paseo.json` schema with services + reverse proxy + terminals.** Elegant and complete:
    ```
    { "worktree": { "setup": "...", "teardown": "...", "terminals": [{name, command}] },
      "scripts": { "web": { "type": "service", "command": "...", "port": 3000 } } }
    ```
    Cuartel adopts this exact shape (extending the Polyscope `polyscope.json` shape from KB 4.18) as `cuartel.json`. Setup runs in the VM at workspace boot.

11. **Multi-profile providers in config.** `claude-work` and `claude-personal` as separate entries against `claude` provider with different API keys. Solves "I want different credentials for different projects" cleanly. Cuartel: per-Workspace credential config, per-Replica credential override.

12. **Pairing via QR code for mobile.** If cuartel ever ships mobile (it should — see strategic note below), copy this pattern.

13. **`paseo agent mode <id> bypass|plan|...`** — runtime mode switching. Cuartel's per-session UX should expose this.

14. **Headless multi-instance via `PASEO_HOME`.** Run multiple isolated daemon instances on the same machine. Useful for testing, dev/prod separation, multi-user.

### Architecture validations (Paseo proves these cuartel bets are right)

- **ACP adoption (D1):** Paseo treats ACP as the standard for non-first-class agents. Gemini and Hermes via ACP shown in their docs.
- **Multi-provider config-driven extension (toward Replicas):** their `agents.providers` works exactly like cuartel's planned per-Replica config.
- **Daemon-style separation** (toward our future architecture): proves a clean daemon/client boundary works, including for mobile/web clients.
- **Worktrees work** for isolation when VM is overkill — Polyscope and Paseo both use them. Cuartel can offer this as Tier 0 alongside Tier 1 VM.
- **Voice as local-first**: ONNX models on CPU is the right baseline.

### Cuartel's wedges vs Paseo (where to differentiate)

1. **Real VM isolation.** Paseo's worktrees mean a misbehaving agent on your laptop has full filesystem + network access. Cuartel's VM bet is the safety story — especially relevant for autonomous/scheduled agents.
2. **Native macOS GPU UI.** Paseo's clients are likely web tech (Electron/Tauri/React Native — based on the cross-platform parity story). Cuartel's GPUI is GPU-rendered, native, and *feels different*. Matters for the "Factorio for agents" command-center experience.
3. **Visual command center / strategy-game UX.** Paseo's UX is conventional (sessions list + chat + diff panel + terminal — same as Polyscope). Cuartel's a16z-thesis command-center bet stays differentiated.
4. **Workspace mobility (Proliferate-style local↔remote move).** Paseo has remote daemon but the workspace stays put. Cuartel's `Workspace::relocate(target)` is unique.
5. **GPU sandboxes for ML/research workloads.** Paseo can't do this. Cuartel's bring-your-own-Hetzner-with-GPU is a niche but real wedge.
6. **Replicas as first-class units (UX, not just config).** Paseo's `agents.providers` is config; cuartel's Replicas are visual units in a command center. Different *feel*.

### The most uncomfortable strategic question

Paseo is **free, open source, multi-platform (mobile too), and architecturally strong**. They've shipped many of cuartel's planned features. **What's cuartel's wedge against a free competitor?**

Honest answers:

- **Native macOS GPU UI** as a *feel* differentiator (Linear vs Asana parallel — same features, different feel, different audience).
- **Real VM isolation** matters for autonomous/scheduled work where misbehavior risk is real.
- **Workspace mobility + GPU sandboxes** are unique infra capabilities.
- **Visual command center** is genuinely different UX.
- **Cuartel as a paid product needs to deliver clearly more value than Paseo's free option** for paid users. The strategy-game command center + native polish + bring-your-own-infra options are the value-add.

**Short version:** Paseo's existence raises the bar. Cuartel's UI + isolation + infrastructure portability has to be *visibly better*, not just different. If we can't articulate why someone would pay for cuartel when Paseo is free, we don't have a product. The path forward is to ship the visible-better-UX (command center, Replicas as units, Polyscope-style polish) on the platform-grade infra (Apple VZ + Hetzner + ACP). And consider open-sourcing parts to compete on community.

### 4.19.1 Paseo source-grounded deep dive (`github.com/getpaseo/paseo`, AGPL-3.0)

Cloned and read end-to-end. **License is AGPL-3.0** — patterns are fair game, direct code copying isn't (would force cuartel to AGPL). All citations are file:line in the cloned repo at `/tmp/research/paseo/`.

**Stack:** TypeScript everywhere. Node.js daemon. Expo (React Native) for cross-platform client. Electron wrapper for desktop. Zod for schema validation. tweetnacl for crypto. Commander.js for CLI. Vitest + Biome + oxlint for tooling. Monorepo via npm workspaces.

**Key implementation details worth knowing:**

1. **WebSocket protocol is JSON, NOT binary multiplexed.** `WS_PROTOCOL_VERSION = 1` hardcoded; mismatch raises `InvalidClientVersion`. Discriminated-union Zod schemas (`messages.ts`) for every message. Terminal streams have their own `terminal-stream-protocol.js`. **Simpler than the docs implied** — JSON over WS is enough. Backward compat is enforced through Zod patterns (`.optional()` for new fields, `.passthrough()` for forward-tolerant configs) + a code-review rule, not mechanical tests.

2. **Reverse proxy (the killer feature) — concrete implementation:**
   - `packages/server/src/utils/script-hostname.ts` — `buildScriptHostname()` slugifies `{service}.[branch].{project}.localhost`. Branch label dropped on default branch. Slugify falls back to `"untitled"`.
   - `packages/server/src/server/script-proxy.ts:38-145` — `ScriptRouteStore` is a `Map<hostname, ScriptRouteEntry>` with `{hostname, port, workspaceId, projectSlug, scriptName}`.
   - `findRoute()` (lines 80-101): strips port suffix from `Host` header, exact match first, then walks subdomain hierarchy for parent matches.
   - `createScriptProxyMiddleware()` (lines 167-220): forwards to `http://127.0.0.1:{port}` with `x-forwarded-*` preserved.
   - `createScriptProxyUpgradeHandler()` (lines 226-250+): handles `http.upgrade` events for WebSocket upgrades — reconstructs raw HTTP upgrade and pipes to target.
   - `script-route-branch-handler.ts:17-68` — branch rename triggers route recompute, no port reallocation. **Cuartel can build this in ~1 day on top of existing port-forwarding infrastructure.**

3. **Multi-pass tool-call normalization (Zod discriminated unions).** The biggest cuartel-relevant pattern beyond the proxy.
   - `packages/server/src/server/agent/providers/claude/tool-call-mapper.ts:113-168` — handles the messy reality that the same shell-exec tool is called `bash` / `Bash` / `shell` / `exec_command` across providers.
   - Pass 1: parse raw tool call, normalize callId, trim name.
   - Pass 2: discriminated union over a `Pass2EnvelopeSchema`; maps provider-specific names to canonical kinds (`shell`, `read`, `write`, `edit`, `search`, `fetch`, `speak`, `unknown`).
   - Result: `ToolCallDetail` with `{callId, name, toolKind, input, output, status}`.
   - **Cuartel will hit the same problem** when wrapping ACP servers from different vendors and needs the same multi-pass-Zod approach for the per-tool UI to know what icon/treatment to apply.

4. **Provider mode mapping across vendors** — `mcp-server.ts:102-141` has explicit `CLAUDE_TO_CODEX_MODE` map: `plan → read-only`, `default → auto`, `bypassPermissions → full-access`. `mapModeAcrossProviders()` translates so a parent agent's mode propagates to a spawned child of a different provider. Cuartel needs the same when sessions are routed across Replicas backed by different providers.

5. **MCP server for sub-agent orchestration** — `packages/server/src/server/agent/mcp-server.ts`. The daemon exposes an MCP server that running agents can call to spawn child agents (`createAgentMcpServer()` line 318). Caller-context inheritance: `resolveCallerAgent()` (339-348) gets the parent; `resolveScopedCwd()` (350-370) enforces `lockedCwd` and `allowCustomCwd`; `buildCallerAgentScheduleConfig()` (396+) propagates parent's `thinkingOptionId`, `modeId`, `systemPrompt`, `mcpServers` to child by default. **This is how the `paseo-orchestrator` skill works.** For cuartel, this is the mechanism behind Replica-spawning-Replica + the subagent-pool-with-quota pattern from KB 4.9 (Modal/OAI SDK).

6. **Loop service — orchestrator-as-built-in-feature** — `packages/server/src/server/loop-service.ts`. Worker + verifier agents; each iteration spawns child agents until success criteria met. Verify can be **command-based** (run shell command, check exit code) or **LLM-based** (`LoopVerifyPromptResult` vs `LoopVerifyCheckResult`). `LoopRecord` (lines 67-96) tracks `{status, iterations[], activeWorkerAgentId}`. **Bounded by `maxIterations`** (Stripe Minions pattern, KB 4.15). This is the implement-and-verify-loop shipped as a daemon primitive, not a shell-script pattern. Cuartel ships this as part of the `Blueprint` data model.

7. **Epoch-based timeline reset (instead of compaction)** — `packages/server/src/server/agent/agent-timeline-store.ts:130-191`. Each agent session has an `epoch: string` (UUID per reset) + `rows: AgentTimelineRow[]` with sequential `seq: number`. Default fetch limit `DEFAULT_TIMELINE_FETCH_LIMIT = 200`. When client cursor's epoch ≠ current epoch, server returns `{reset: true, ...full timeline}`. **Beautifully simple.** No incremental compaction logic — clear-and-replay. Cuartel adopts this for its session model.

8. **Relay protocol — concrete crypto:**
   - `packages/relay/src/crypto.ts:72` — `generateKeyPair()` uses `nacl.box.keyPair()` (Curve25519). Public/secret keys 32 bytes each.
   - `packages/relay/src/encrypted-channel.ts:89-225` — handshake:
     - Client → daemon: `{type: "e2ee_hello", key: <base64 client pub>}` in a 1000ms retry loop.
     - Daemon → client: `{type: "e2ee_ready"}` after deriving shared key.
     - All subsequent messages: `[nonce(24)][ciphertext]` via XSalsa20-Poly1305 AEAD (`nacl.box`), base64-encoded over WebSocket text frames.
   - QR code transports daemon's pub key only; client generates its own keypair on first connection. Daemon keypair persisted at `$PASEO_HOME/daemon-keypair.json`.
   - **Both sides verify by deriving the same shared key**; AEAD provides authentication. No separate MAC.

9. **Voice subsystem — hidden-agent pattern:**
   - `packages/server/src/server/agent/mcp-server.ts:98` — `createAgentMcpServer(..., enableVoiceTools, voiceOnly)`. **`voiceOnly: true` flag isolates voice tools from user-visible agents.**
   - The voice LLM runs in a separate hidden agent session (configurable: any installed provider, e.g. Claude Haiku). Speech service bridges MCP stdio → STT/TTS transcoding.
   - ONNX models (Parakeet STT + Kokoro TTS) downloaded on first start to `$PASEO_HOME/speech-models/`. Provider config: `{provider: "local" | "openai"}` per feature (dictation, voice STT, voice TTS).

10. **Schedules / cron** — `packages/server/src/server/schedule/cron.ts`:
    - Custom 5-field cron parser (minute hour DOM month DOW).
    - `parseField()` (19-74) handles `*`, ranges (`1-5`), lists (`1,3,5`), steps (`*/5`).
    - `computeNextRunAt()` (119-149) iterates forward up to 366 days to find next match.
    - On daemon restart: `computeNextRunAt(cadence, lastRunAt)` recalculates next fire; missed runs **silently skipped** (no catch-up). Background scheduler polls.
    - **Implementable for cuartel in 200-300 LOC** — modest scope, big UX win.

11. **CLI's `--output-schema`** — `packages/cli/src/commands/agent/run.ts:59-62, 116-201`:
    - `loadOutputSchema()` reads file or parses inline JSON, ensures object type.
    - `fetchStructuredOutput()` calls `getStructuredAgentResponse()` which **re-prompts the agent up to 2 times** to match the schema. Validates with Zod after each attempt.
    - Used in shell-script implement+verify loops with `jq`. Cuartel exposes the same primitive on its CLI/API.

12. **Provider capability flags** — `packages/server/src/server/agent/providers/acp-agent.ts:92-99`:
    ```
    DEFAULT_ACP_CAPABILITIES = {
      supportsStreaming, supportsSessionPersistence, supportsDynamicModes,
      supportsMcpServers, supportsReasoningStream, supportsToolInvocations
    }
    ```
    Client checks before invoking features; graceful degradation if absent. Cuartel needs the same when supporting heterogeneous ACP servers (some features Claude has that Gemini lacks, etc).

13. **Backwards-compat enforcement is procedural, not mechanical** — `CLAUDE.md` lines 62-67 codify the rule ("does a 6-month-old client still parse this? does a 6-month-old daemon still send something this client accepts?"). Enforced via Zod patterns (`.optional()`, `.passthrough()`) and the ACP capability flags pattern. **No automated old-client-vs-new-daemon test suite that I could find** — relies on code review. Worth a v3 improvement: cuartel could add it.

**AGPL-3.0 implication for cuartel.** Cannot copy Paseo source verbatim into a non-AGPL project. **Patterns and designs are not copyrightable** — we can re-implement freely. If cuartel ever goes AGPL itself, we could vendor pieces directly (e.g. the cron parser, the relay channel impl). For now: clean-room implementation of the patterns from this analysis is the right approach.

### 4.20 portless (`github.com/vercel-labs/portless`, Apache-2.0) — solves the reverse-proxy problem so cuartel doesn't have to

Vercel Labs project. Standalone CLI + library that does **exactly** what Paseo's reverse proxy does (`<name>.<branch>.<project>.localhost`) — but as a polished, separately-maintained, **Apache-2.0** package. **This changes the build-vs-buy calculus on cuartel's reverse-proxy story.**

**What portless is:** "Replace port numbers with stable, named .localhost URLs for local development. For humans and agents." Tagline ends with "for agents" — they know the audience.

**The library API (`packages/portless/src/index.ts`):**
```ts
export * from "./types.js";  // RouteInfo, ProxyServerOptions
export * from "./proxy.js";  // 468 lines — the reverse proxy
export * from "./routes.js"; // 251 lines — route registry
export * from "./utils.js";
export * from "./hosts.js";  // 135 lines — /etc/hosts sync
```

`ProxyServerOptions` (from `types.ts`):
```ts
{
  getRoutes: () => RouteInfo[];     // callback — driven by cuartel's session store
  proxyPort: number;
  tld?: string;                     // "localhost" or "test"
  strict?: boolean;                 // exact-match vs wildcard subdomains
  onError?: (msg: string) => void;
  tls?: { cert, key, ca?, SNICallback? };  // HTTP/2 over TLS, per-hostname certs
}
```

So cuartel can **import portless as a library**, plug in a `getRoutes` callback that reads from the cuartel session/workspace store, and get the full proxy capability. The proxy auto-starts on first app launch.

**What portless does that Paseo's proxy doesn't (the harder parts):**

1. **Local CA generation + system trust** (`certs.ts`, **1096 lines** — substantial). Generates a local CA, adds it to the system trust store, supports macOS/Linux/Windows (`certutil` on Windows; `update-ca-certificates` / `update-ca-trust` on Linux distros). HTTPS works without browser warnings out of the box. **This is genuinely hard to reimplement well.**
2. **HTTP/2 multiplexing.** Overcomes the 6-connection-per-host limit that bottlenecks Vite/Nuxt-style unbundled dev servers. Single TCP connection, all requests multiplexed.
3. **Framework auto-detection and port/host injection.** Most frameworks respect `PORT` env var (Next.js, Express, Nuxt). Frameworks that don't — Vite, VitePlus, Astro, React Router, Angular, Expo, React Native — get `--port` flag auto-injected, plus `--host` where needed (RN gets `--host 127.0.0.1`; Expo gets `--host localhost` outside LAN mode). **This is hand-tuned per framework.** Reimplementing is years of community feedback.
4. **Automatic `/etc/hosts` sync** for Safari (which doesn't auto-resolve `.localhost` subdomains via DNS). On by default (`PORTLESS_SYNC_HOSTS=0` to disable). Custom TLD support (`.test` recommended; warns about `.local` mDNS conflict and `.dev` HSTS).
5. **LAN mode via mDNS** (`mdns.ts` + `lan-ip.ts`). `--lan` flag advertises services as `<name>.local` reachable from any device on the network. Auto-detects LAN IP, follows Wi-Fi/IP changes, can pin via `--ip`. macOS uses `dns-sd` (built in); Linux uses `avahi-publish-address`. **For cuartel: this is exactly the "control my agents from my phone on the same Wi-Fi" feature** without needing Tailscale or the e2e relay.
6. **Proxy loop detection** — returns `508 Loop Detected` when a frontend dev server proxies back without `changeOrigin: true`. Diagnostic-quality error message points at the fix.
7. **Worktree detection** — same as Paseo: in a linked git worktree, prepends branch name as subdomain (`<branch>.<myapp>.localhost`). No config changes needed.
8. **State persistence** at `~/.portless/`. Proxy restart preserves TLS/TLD/LAN settings unless overridden.
9. **Wildcard subdomains** (`--wildcard`) — `tenant1.myapp.localhost` falls back to `myapp` route without explicit registration. Multi-tenant dev.
10. **Subprocess env injection.** Children get `PORT`, `HOST`, `PORTLESS_URL`, `NODE_EXTRA_CA_CERTS` (so Node trusts portless's CA without separate setup).
11. **Cross-app proxying handled.** When app A proxies to app B (via Vite/webpack devServer), portless trusts the CA in the child process and the loop detection prevents infinite recursion if `changeOrigin` is missed.
12. **Production-quality CLI ergonomics.** Reserved-name guarding (`run`, `get`, `alias`, etc. can't be app names). Non-interactive env detection (`CI=1` exits early with descriptive error). `--force` to take over an existing route. `portless trust` subcommand to (re)add CA to system trust.

**License: Apache 2.0.** Cuartel can:
- Depend on portless as a library (recommended, see below)
- Spawn portless as a subprocess (zero-coupling fallback)
- Fork it freely
- Contribute upstream

**The build-vs-buy decision:**

| Option | Pros | Cons |
|---|---|---|
| **A. Depend on `portless` library** | Get all the polish (CA, /etc/hosts, frameworks, mDNS, HTTP/2, loop detection) for free. Vercel-maintained. | Adds Node.js dep to `cuartel-daemon` (we already have Node processes for ACP servers). |
| B. Build our own in Rust | Tighter integration; one less dep | ~6 months of cert mgmt + framework quirks + Win/macOS/Linux trust stores + mDNS to reach feature parity |
| C. Spawn `portless` CLI as subprocess | Lowest coupling | Less fine-grained control; harder to drive `getRoutes` dynamically |
| D. Fork portless | Maximum control | Ongoing maintenance burden for code we didn't write |

**Strong recommendation: Option A.** Same logic as cuartel's "depend on shuru, fork later" decision (D6 in v2). The reverse proxy is necessary infrastructure but not differentiating — portless solves it well, license-friendly, low risk. If portless ever bit-rots or our needs diverge, fork (Apache 2.0 makes this safe). Cuartel-daemon spawns a Node sidecar that imports portless and exposes the route registry over a small WebSocket; cuartel-app's session store pushes route updates. ~1 day of integration work.

**The killer combination:** portless gives cuartel the **branch-named-URL-per-service** feature with **HTTPS by default**, working across **macOS/Linux/Windows** — features that would take months to reimplement. Pair with the existing port-forwarding from phase 5e: portless inside the sandbox VM allocates hostnames for in-VM services; the host-side cuartel-daemon (also running portless) allocates host-side hostnames for forwarded sandbox ports. **Two layers, same pattern.** Or just one layer if we forward only the proxy port and let in-VM portless handle hostnames inside the sandbox.

**Where portless slots into the v2 roadmap:** Step 3 (`AppleVzSandbox`) bakes portless into the sandbox image (in-VM hostnames for in-VM services). Step 5 (host-side persistence + sidebar + command center) integrates host-side portless for sandbox-port-forwarded URLs. **Both are ~1 day each on top of existing infrastructure.**

**Bonus features beyond the proxy:**
- `portless` ships an `AGENTS.md` and a `skills/portless/SKILL.md` (Anthropic Skills format) — they target agent-driven dev workflows explicitly. Cuartel can ship the same skill file in its workspace scaffolds.
- Documented agent rules (no emojis, no en-dashes in prose, document boolean env vars as 0/1 only, etc.) — instructive example of how a project optimizes its codebase for agent legibility (KB 4.11 OpenAI insight).
- Vercel-maintained suggests good test coverage and Windows support (they have a Windows EC2 dev rig described in AGENTS.md for Windows-specific debugging).



Concrete proof-of-scale for parallel agent coordination. **16 Claude Opus 4.6 instances, 2 weeks, ~2000 sessions, $20K.** Result: 100K-line Rust C compiler that boots Linux 6.9 on x86/ARM/RISC-V, compiles QEMU/FFmpeg/SQLite/Postgres/Redis. 99% pass rate on most compiler test suites including GCC torture.

**Patterns worth adopting:**

1. **Infinite execution loop pattern.** "When it finishes one task, it immediately picks up the next." Bash loop wrapping Claude with explicit "break it into small pieces, track what you're doing, keep going until perfect."
2. **File-locking task claiming via shared filesystem.** Agents claim work by writing to `current_tasks/` files in a shared bare git repo. Lightweight coordination — no lock service, no queue, just files. Merge conflicts handled by Claude. Maps directly onto cuartel's potential subagent orchestrator.
3. **Known-good oracle pattern.** When the agents kept hitting the same monolithic kernel-compile bugs, switched to **using GCC as a known-good oracle** so each agent could work on different files independently. Generalizes: pair the agent with a working reference system to enable parallelism on the unknown bits.
4. **Role specialization across agents.** Dedicated agents for: dedup, perf optimization, code quality, docs maintenance. Different prompts, same loop. Parallelism via division of labor, not just division of files.
5. **Task verifier quality is critical.** "It's important that the task verifier is nearly perfect, otherwise Claude will solve the wrong problem." For cuartel's "ship it" mode, the test/lint pass criteria *are* the agent's optimization target.
6. **Context-pollution discipline.** "The test harness should not print thousands of useless bytes. At most, it should print a few lines of output and log all important information to a file." Tools the agent calls should default to terse output + log-file-on-disk for retrieval.
7. **Time-blindness fix via deterministic-random subsampling.** `--fast` runs 1-10% of tests, deterministic per-agent but random across instances. "Claude can't tell time and, left alone, will happily spend hours running tests instead of making progress." Cuartel's tooling layer could expose this primitive.
8. **Progress files as first-class.** "Maintain extensive READMEs and progress files that should be updated frequently." Same OpenAI insight — push state into the repo, where the agent can read it on every turn.

**Hard limits surfaced:** generated code lacks efficiency vs production compilers. Failed at 16-bit x86 compact codegen. Failed to reliably implement own assembler/linker. **"New features and bugfixes frequently broke existing functionality"** — agent teams work best with **well-partitioned tasks rather than tightly coupled features**.

---

## 5. Patterns adopted (cross-referenced)

| Pattern | Source(s) | Where it lands in cuartel |
|---|---|---|
| ACP wire protocol | Zed, claude-code-acp, gemini-cli | `cuartel-acp` crate (new) |
| Workspace + Worktree model | Zed `project/worktree` | `Workspace` in cuartel-core |
| Per-thread worktree scoping | Zed `work_dirs` | `Session.work_dirs: PathList` |
| SQLite + async write queue | Zed `thread_metadata_store` | `cuartel-db` migration |
| Retained-sessions pool with idle eviction | Zed `agent_panel` | cuartel-app session orchestrator |
| Notification windows (floating, cross-session) | Zed `agent_notification` | cuartel-app UI |
| Per-session model selection | Zed `ModelSelectorPopover` | session view |
| Tool permission decisions + hardcoded denylist | Zed `tool_permissions.rs:20` | cuartel-core enforcement layer |
| Apple VZ wrapper via objc2 | Shuru `shuru-darwin` | dependency |
| AF_VSOCK + framed binary protocol | Shuru `shuru-proto` | dependency or fork |
| Virtiofs + overlay rootfs | Shuru | dependency (via Apple VZ config) |
| TLS-MITM credential injection | Shuru `shuru-proxy` + BB proxy | adopt or fork; supersedes some of cuartel's auth gateway |
| Pre-warmed sandbox image refreshed periodically | BB | image build pipeline + cron refresh |
| RBAC + ABAC two-layer access control | BB | per-session permission bundles |
| Permissions scoped by invocation source | BB | different spawn paths carry different policy |
| Runtime relocation (one-click local↔remote) | Proliferate | `Workspace::relocate(target)` |
| Remote-spawn via command transformation | Zed `acp.rs:505–528` | cuartel `RemoteStdio` transport |
| Pre-warmed snapshot refreshed via cron, diff-from-base storage | BB + Ramp/Modal | sandbox image build pipeline |
| Block writes during workspace sync, allow reads | Ramp | provision step in `Sandbox` impl |
| Per-session SQLite databases | Ramp/Cloudflare Durable Objects | `cuartel-db` schema choice |
| Subagent pool with worker-pool semantics | Modal/OAI SDK | future `cuartel-orchestrator` layer |
| Subagent fresh context windows + status updates without context-exit | Modal/OAI SDK | subagent harness contract |
| Quota system for spawn-able subagents (cost guardrail) | Modal/OAI SDK | per-session orchestrator policy |
| Filesystem snapshots as on-disk memory between subagents | Modal/OAI SDK | `Sandbox::snapshot/restore` semantics |
| Code Mode for MCP / tool surface (collapse N tools into meta-tools) | Cloudflare | future `cuartel-mcp-portal` |
| AGENTS.md/CLAUDE.md per-workspace as first-class file | Cloudflare + industry | workspace registry; UI for editing |
| Multi-agent consensus for review scoring | Cloudflare | optional review/audit feature |
| Queue follow-up prompts rather than interrupt | Ramp | session input handler default |
| Voice input | Ramp + Vercel | macOS speech APIs in cuartel-app |
| GitHub App tokens scoped per-repo (not user tokens) | Ramp | git-integration design |
| Optional auto-commit + push + PR ("ship it" mode) | Vercel + Ramp | per-session toggle |
| Read-only session sharing URL | Vercel | future collaboration feature |
| Per-user attribution + cost tracking via gateway | Cloudflare AI Gateway | extend cuartel auth gateway |
| GPU-attached sandboxes (sandbox property, not separate API) | Modal | future `Sandbox` capability flag |
| Visual verification (VNC / live preview / screenshots) | Ramp | future per-session preview surface |
| Structured representation > pixels (DOM trees > screenshots) | Ramp | guideline for any inspection tool |
| **Session as a third pillar (durable append-only event log)** | Anthropic Managed Agents | refines v2: split `Workspace + Sandbox + Session` |
| Lazy sandbox provisioning (defer until first tool call) | Anthropic Managed Agents (60–90% TTFT win) | `Sandbox::provision` semantics |
| Generator-evaluator separation (separate critic agent) | Anthropic harness design | optional "ship it" mode + multi-agent review |
| Sprint contracts (negotiate testable success criteria first) | Anthropic harness design | structured task entry in session |
| Context resets via handoff artifacts (not in-place compaction) | Anthropic harness design | session-rollover semantics |
| Continuous harness simplification (remove components as models grow) | Anthropic harness design | discipline, not feature |
| Subjective taste codified as grading rubric | Anthropic harness design | per-workspace rubric file |
| AGENTS.md as ~100-line table-of-contents pointing into structured `docs/` | OpenAI Codex | first-class workspace file structure |
| `docs/` system-of-record with versioned plans (active/, completed/, tech-debt-tracker.md) | OpenAI Codex | workspace template / scaffold |
| Per-worktree boot of app + observability stack | OpenAI Codex | sandbox image scaffold |
| Custom lints with error messages that inject remediation into agent context | OpenAI Codex | lint-as-teaching-channel pattern |
| Recurring "doc-gardening" / "garbage collection" agents | OpenAI Codex | background agent role |
| Layered architecture mechanically enforced (early prerequisite, not late) | OpenAI Codex | starter scaffold rule |
| Minimal blocking merge gates (corrections cheap, waiting expensive) | OpenAI Codex | "ship it" mode default |
| File-locking task claiming via shared bare git repo | Carlini C compiler | subagent coordination primitive |
| Known-good oracle to enable parallel work on unknown bits | Carlini C compiler | strategy for large refactors |
| Role specialization across parallel agents | Carlini C compiler | subagent-pool config |
| Context-pollution discipline (tools terse-by-default + log-on-disk) | Carlini C compiler | tool-design guideline |
| Time-blindness fix via deterministic-random subsampling | Carlini C compiler | tool primitive |
| **Blueprints — orchestration as state machine of {deterministic, agent} nodes** | Stripe Minions | first-class `Blueprint` concept; per-team customizable; visualizable as graph |
| **Toolshed — single centralized MCP server, per-agent curated tool subsets** | Stripe Minions | cuartel-mcp-portal as single source of truth |
| Pre-warmed devbox pool (10s ready) with code + caches + services | Stripe Minions | sandbox image template |
| Cursor rules format as cross-agent standard (sync to other formats) | Stripe Minions | rule-format compatibility layer |
| No confirmation prompts when blast radius is contained (full-permission autonomous mode) | Stripe Minions | autonomous-mode permission policy |
| Don't write the agent loop yourself — fork an OSS one | Stripe forked goose; cuartel adopts ACP | architectural discipline |
| Shifting feedback left — pre-push lint hooks + bg lint daemon | Stripe Minions | sandbox image scaffold |
| Bounded CI iteration (1–2 rounds, then escalate to human) | Stripe Minions | "ship it" mode default |
| Multi-entry-point invocation as core feature | Stripe + Ramp + BB | UI/integration layer |
| Bring-your-own-LLM-subscription (CLI auth flow) | Replicas | credential model option |
| Web-API for autonomous-agent platforms | Replicas | future cuartel public-API surface |
| **Replica = named persistent configured agent profile** | Replicas brand + BB/Ramp/OpenAI naming patterns | proposed first-class abstraction (see 7.8) |
| **Computer use — agent drives desktop/browser, records video artifacts** | Cursor cloud agents | sandbox image scaffold + bundled MCP servers (Playwright + custom desktop) |
| Live remote control via VNC + noVNC in webview | Cursor "you can also control the agent's desktop" + Ramp | port-forward x11vnc; embed noVNC in GPUI webview |
| Artifacts (video, screenshot, log, HTML) ship with PR | Cursor cloud agents | per-session artifacts panel; virtiofs-mounted /artifacts dir |
| 45-min autonomous UI walkthroughs for QA | Cursor (cursor.com/docs walkthrough) | use case validation for autonomous mode |
| **Visual Editor — DOM-element-picker → agent context** (cheaper than VNC) | Polyscope | Tier 0 of computer-use story (KB 7.5); embedded webview + JS overlay |
| **Autopilot — high-level goal → AI-generated user stories → drag-reorder → sequential exec** | Polyscope (operationalizes Anthropic sprint contracts + Stripe blueprints) | Replicas+Blueprints UI implementation reference (7.8) |
| **Multi-model consensus as user feature ("Opinions")** | Polyscope (operationalizes Cloudflare 4.8) | future cuartel feature; multiple Replicas answer same question, synthesize |
| **Review tab as separate session** | Polyscope (operationalizes Anthropic generator-evaluator) | UI implementation reference for "ship it" mode |
| **Tasks — one-click reusable prompts in workspace config** | Polyscope (lighter than Skills) | `cuartel.json` `tasks: []` array + sidebar dropdown |
| **`cuartel.json` config in repo root** (`scripts.setup/archive`, `preview.url`, `tasks`, `{{folder}}` placeholder) | Polyscope | per-workspace config standard; alternative to AGENTS.md scaffold |
| **Plan mode with "Clear context & Approve"** option | Polyscope (operationalizes Anthropic context-reset 4.12) | plan-approval UX |
| **Linked workspaces — read-only context sharing across workspaces** | Polyscope | smaller-scope cousin to workspace move |
| **Per-repo merge/PR prompts** | Polyscope | small but high-ROI personalization |
| **CI auto-fix loop with bounded retries** | Polyscope + Stripe Minions | "ship it" mode default |
| **One-base-repo workspace mode** (no clone, agent works in user's checkout) | Polyscope | escape hatch for live dev-server scenarios |
| **Crash recovery for long-running automations** | Polyscope (Autopilot) | general principle for any cuartel batch automation |
| **Per-workspace Herd integration** (`herd link/secure` per workspace clone) | Polyscope | template for similar auto-detect of Vite/Next.js/Rails dev servers |
| **Daemon + multi-client architecture** (Docker-style) | Paseo | possible cuartel future split: `cuartel-daemon` + `cuartel-app` (GPUI) + future mobile client |
| **Deterministic-hostname reverse proxy per branch + service** (`web.fix-auth.my-app.localhost`) | Paseo | sandbox image + host-side proxy; eliminates port-conflict pain |
| **Service-to-service env injection** (`PASEO_SERVICE_<NAME>_PORT/_URL`) | Paseo | `cuartel.json` `scripts` section + env injection at process spawn |
| **`paseo.json`-style services config** (`type: "service"`, port, command) | Paseo + Polyscope | `cuartel.json` schema |
| **Local voice stack with ONNX models** (Parakeet STT + Kokoro TTS, CPU-only) | Paseo | bundled voice subsystem, optional OpenAI fallback |
| **Hidden agent session for voice LLM orchestration** | Paseo | clean abstraction for voice mode |
| **Built-in cron scheduler** (`paseo schedule create --cron ...`) | Paseo | first cut of Replica triggers; can ship before full Replicas |
| **`--output-schema` for structured agent responses** | Paseo | CLI/API primitive; enables implement+verify loops |
| **End-to-end encrypted relay** (ECDH + AES-256-GCM, QR-pairing) | Paseo | alternative to Tailscale for zero-config remote |
| **Custom providers via config extension** (extend Claude with different API endpoint, multi-profile) | Paseo | Tier 0 of Replicas — config-only first, data model later |
| **CLI designed for agent self-invocation** (hierarchical multi-agent workflows via shell scripts) | Paseo | cuartel CLI same shape; enables tiny scriptable orchestrators |
| **Pairing via QR code** | Paseo | mobile-companion pattern when cuartel ships mobile |
| **Headless multi-instance** via `PASEO_HOME` | Paseo | dev/prod isolation; multi-user on shared machine |
| **Worktrees as Tier 0 isolation** (alongside VM Tier 1) | Polyscope + Paseo | `LocalSandbox` impl option for users who want zero-overhead local |
| **Multi-pass Zod discriminated-union tool-call normalization** | Paseo `tool-call-mapper.ts` | `cuartel-acp` adapter for canonical-tool-kind UI treatment |
| **Provider mode mapping across vendors** (`plan ↔ read-only ↔ auto ↔ full-access`) | Paseo `mcp-server.ts:102-141` | when a session moves between Replicas backed by different providers |
| **MCP server for sub-agent control** (agents call back to spawn child agents) | Paseo `mcp-server.ts:318+` | substrate for Replica-spawning-Replica + subagent quotas |
| **Caller-context inheritance for spawned agents** (mode/model/MCPs propagate to child) | Paseo `mcp-server.ts:339-394` | child Replica defaults |
| **Loop service as a daemon primitive** (worker + verifier with bounded iterations, command-or-LLM verify) | Paseo `loop-service.ts` | first-class `Blueprint` type in cuartel-db |
| **Epoch-based timeline reset (no compaction)** | Paseo `agent-timeline-store.ts` | session model: per-run epoch UUID, client reloads on mismatch |
| **Hidden-agent pattern for voice / system services** (`voiceOnly: true` flag) | Paseo `mcp-server.ts:98` | clean separation between user-facing and system-internal agent sessions |
| **Provider capability flags + graceful degradation** | Paseo ACP defaults | feature gating for heterogeneous providers |
| **Custom 5-field cron parser, ~200 LOC** | Paseo `schedule/cron.ts` | feasible cuartel implementation reference |
| **Output-schema re-prompting (Zod-validated, max 2 retries)** | Paseo CLI `run.ts:116-201` | structured agent responses for scripting |
| **tweetnacl-based ECDH+AEAD relay channel** (Curve25519 + XSalsa20-Poly1305) | Paseo `relay/encrypted-channel.ts` | relay implementation reference (clean-room re-impl) |
| **portless: branch-named-URL-per-service reverse proxy as a dependency** (Apache-2.0, library API) | Vercel-labs portless | depend on the npm package; plug `getRoutes` callback into cuartel's session store |
| **Local CA generation + system trust** (HTTPS-by-default, macOS/Linux/Windows) | portless `certs.ts` (1096 LOC) | get for free via portless dep |
| **HTTP/2 multiplexing for unbundled dev servers** | portless | get for free via portless dep |
| **Framework-specific port/host injection** (Vite, Astro, Next, Expo, RN, Angular...) | portless | get for free via portless dep |
| **Automatic /etc/hosts sync for Safari + custom TLDs** | portless `hosts.ts` | get for free via portless dep |
| **mDNS-based LAN mode** (`<name>.local` reachable across devices on same Wi-Fi) | portless `mdns.ts`+`lan-ip.ts` | enables phone-controls-laptop without Tailscale |
| **Proxy loop detection** (`508 Loop Detected` with diagnostic message) | portless | UX polish for cross-app proxying |
| **Reserved name guarding + non-interactive env detection** in CLIs | portless | cuartel CLI ergonomics |

---

## 6. Open questions

Numbered for reference — answer in roadmap order.

1. **VM image build & distribution.** Who builds the Linux image with `claude-code-acp` baked in? Where is it cached? Update cadence when claude-code-acp releases? *(BB's pattern — periodic snapshot refresh — is a strong default.)*
2. **VM cold-start time on Apple VZ.** Acceptable session-start latency budget? If multi-second, do we need a warm-VM pool? Zed-style retained sessions help amortize.
3. **Session resume semantics.** Does `claude-code-acp` support ACP's `loadSession`? If not, sandbox restart loses transcript. Verify in step-1 spike.
4. **Transport for remote stdio.** SSH-inside-Tailscale (Zed pattern, simple) vs framed-TCP daemon on Hetzner (one long-lived process)? Pick before Hetzner sandbox.
5. **Workspace move when sandbox is running.** Live migration vs idle-only first? Recommend idle-only.
6. **Multi-repo workspaces.** ACP `work_dirs` carries the list; sandbox image needs a mount-spec format for `/workspace/{repo1,repo2,…}`.
7. **Encrypted workspace mount.** Decrypt on host (push plaintext over vsock) or in VM (push key once)? Security review.
8. **Rivet's role going forward.** If they ship VM-backed sandboxes, do we adopt? Decision deferred — fits the trait either way.
9. **Auth gateway vs Shuru-proxy.** Both implement TLS-MITM secret injection. Adopt shuru-proxy and retire ours, or keep ours for tighter integration with cuartel's keychain layer? Defer until step 4.
10. **Skill system.** Adopt BB's `.cuartel/skills/*.md` per workspace? Or wait for Anthropic Skills as a primitive? Future work, not v1.
11. **MCP portal with Code Mode.** When does context bloat from MCP tool definitions become a real problem for cuartel users? If we expect heavy MCP usage, ship Code Mode early; if not, defer.
12. **Per-session SQLite vs single global DB.** Ramp/Cloudflare proved per-session scales better at hundreds of parallel sessions. For cuartel's expected scale (one user, dozens of concurrent sessions max), a single DB with proper indexing may suffice. Re-evaluate when usage patterns appear.
13. **Background-session lifecycle on macOS.** What happens when the user closes the laptop with running remote sessions? Sandbox keeps running on Hetzner; cuartel-app should reconnect on wake. Notifications need a path (cuartel daemon? push notification? Slack-like external surface?).
14. **Higher-level sandbox SDK as v3 architectural option.** If a future ACP server fully routes fs/exec via ACP RPCs, we could move the agent to the host (Vercel pattern). Worth a periodic re-evaluation (every 6 months?) — a cleaner abstraction may emerge.
15. **GPU sandboxes — first-class or skip?** ML/research use case is real but niche. Don't build until a concrete user asks; design `Sandbox` trait so it's not painful to add later.
16. **Session model: append-only event log vs row-in-SQLite?** Anthropic Managed Agents commits to event-log semantics (`getEvents`, `wake`, position-sliced reads). Adopt early or retrofit later? Recommend: design v2 step-5 (persistence) with event-log semantics from the start; rows-in-SQLite is the *storage* under that interface.
17. **Generator-evaluator split: parallel sessions or single session with role switch?** When implementing "ship it" mode, do we spawn a literal second `Session` for the evaluator, or extend a single session with role-switch turns? Two sessions is cleaner conceptually but doubles UI complexity.
18. **Context-reset semantics in cuartel.** When a session approaches context limits, do we surface a "reset?" button to the user, do it automatically with a handoff doc, or warn-and-wait? Anthropic's pattern is auto-handoff; cuartel could choose either.
19. **AGENTS.md scaffolding by default?** When user creates a workspace, do we scaffold the OpenAI-style `AGENTS.md` + `docs/` structure, suggest it, or stay out of the way? Some users won't want it; some will adopt it as soon as they see the value. Recommend: scaffold opt-in with a one-click template.
20. **Anthropic Managed Agents convergence.** When (not if) MA exposes a public API, does cuartel consume it as another transport for the same `AgentServer` interface, or treat it as a parallel architecture? Recommend: design for transport-pluggability so MA, ACP, and a hypothetical OpenAI Agent SDK can coexist behind one cuartel UI.
21. **Adopt "Replica" as a first-class abstraction (or not).** Section 7.8 makes the case. Open: workspace-scoped vs personal-scoped, naming (Replica vs alternatives — brand-confusion risk with replicas.dev), ordering vs other roadmap work. Recommend: ship in step 5 alongside command-center UI; workspace-scoped first; pick public name closer to launch.
22. **Adopt Blueprints (Stripe pattern) as the orchestration primitive.** A workflow + agent hybrid where each node is either deterministic code or an agent loop. Per-team customizable. Visualizable. Ties into Replica (each Replica references a Blueprint), into "ship it" mode (deterministic linter/test/push nodes guarantee outcomes), and into the visual workflow editor product direction. Recommend yes; design data model in step 5.
23. **Bring-your-own-LLM-subscription via CLI auth.** Replicas pattern: `replicas claude-auth` authenticates with the user's existing Claude Code subscription. For cuartel: do we support this or insist on direct API key only? Affects cost UX significantly. Recommend yes; technically simple if Anthropic's CLI exposes the right hooks.
24. **Build cuartel-mcp-portal (Toolshed pattern) early or late?** Stripe's centralized MCP server with per-agent curated subsets. Critical for the multi-Replica future (each Replica has its own curated subset). If we ship Replicas without it, we'll retrofit painfully. Recommend: even a minimal mcp-portal (config registry + per-replica subset selector) shipped with step 5 prevents the retrofit.

---

## 7. Non-goals (this refactor)

- Custom MCP servers loaded from cuartel. MCP config pass-through to the ACP server is enough.
- Multiplayer / shared sessions.
- Supporting agents that aren't ACP-compliant (Pi's HTTP API: wrap in a thin ACP shim, don't special-case).
- Collaborative buffers / real-time co-editing (out of scope; Zed's `buffer_store` not adopted).
- Cross-platform host (Win/Linux). Mac-only stays.
- Per-domain integrations / service packages (BB pattern). Cuartel is for coding, not internal ops.

---

## 7.5 Product directions surfaced by external research

Brainstormed during the round-2 research pass. Each entry is a *possible* feature direction grounded in what other teams have built and shipped. **None of these are committed work.** They exist so we don't lose the idea-list when prioritization comes around.

Tagged: **[core]** — fits naturally with v2 architecture; **[adjacent]** — expands the product surface meaningfully; **[future]** — plausible but premature.

### Background sessions
**[core]** "Kick off a session at midnight, check the PR in the morning" (Ramp). The macOS GPUI app is for active sessions; long-running sandboxes survive laptop sleep when `RuntimeLocation::Remote`. Needed: notification/badging when an agent finishes or needs input, cross-device handoff (start on laptop, check on phone via web view), and a small notifier daemon that runs even when the main app is closed. Especially compelling for cuartel because we already have the Tailscale + remote-VM substrate.

### Multi-version prompt fan-out
**[core]** "Kick off N variations of the same prompt and pick the winner" (Ramp). Different models (Claude vs Codex vs Gemini), different temperatures, different prompts. Spawns N sandboxes in parallel, surfaces a comparison view in GPUI. Maps cleanly onto v2's `AgentServerCommand` config + multiple `Session`s in the same `Workspace`. Concrete UX win: stop guessing which model to use — try them all.

### Code Mode for MCP / tool surface
**[adjacent]** Borrow Cloudflare's two-meta-tool collapse pattern. Cuartel ships an MCP portal that exposes `mcp_search` + `mcp_execute` to the agent regardless of how many MCP servers are configured. Saves *thousands* of tokens of tool-definition context per turn. Real product differentiation if MCP-heavy users are a target audience.

### AGENTS.md / CLAUDE.md as first-class workspace file
**[core]** UI for editing per-workspace agent context: test commands, navigation conventions, code style, "things this codebase has burned us on before." Every coding agent reads these files; cuartel makes editing/syncing them a native operation rather than a buried text file.

### Visual frontend verification
**[adjacent]** When an agent edits frontend code, automatically spin up a preview port (we already have port forwarding from phase 5e) and screenshot the rendered result. Surface in the session UI as a side-by-side. Inspired by Ramp's VNC-in-sandbox approach but adapted: structured screenshots rather than full VNC. Pairs naturally with cuartel's GPU-rendered UI.

### Voice-driven async kickoff
**[adjacent]** macOS has solid built-in speech APIs (or whisper.cpp locally; or ElevenLabs cloud). "Hey cuartel, while I'm at lunch, fix the flaky tests on PR #234" → spawns a background remote session, notifies on completion. Both Ramp and Vercel ship this; it's table-stakes for the background-agent UX.

### "Ship it" mode (auto-PR)
**[core]** Per-session toggle: when the agent reaches a successful state (tests pass, lints clean), auto-commit, push to a branch, open a PR. Vercel and Ramp both ship this. Removes the most repetitive part of the agent-to-PR loop. Pairs well with GitHub App tokens scoped per-repo (Ramp pattern) so the user's identity isn't on the line for unreviewed code.

### Read-only session sharing
**[adjacent]** Generate a public (or org-internal via Tailscale) URL of a session's transcript + tool calls + final diff for stakeholder review. Vercel pattern. Lightweight; mostly a serialization of what we already have.

### Subagent orchestrator with quota
**[future]** Once cuartel handles a single session well, support agent-spawned subagents with: (a) fresh context per child (Modal pattern), (b) status updates without context-exit, (c) per-session quota on subagent count and total spend. Critical guardrail before letting an LLM spawn LLMs.

### Multi-agent consensus review
**[adjacent]** A user opens a PR; cuartel spawns N agents to score it independently against AGENTS.md / Engineering Codex rules. Output: COMPLIANT / PARTIAL / NON-COMPLIANT per requirement (Cloudflare pattern). Different from cuartel's primary loop but a high-leverage adjacency for code-review workflows.

### Personal AI Gateway
**[adjacent]** Extend cuartel's auth gateway into a full AI control plane (Cloudflare AI Gateway pattern, single-user scale). Per-route policy (which model can call which API), cost tracking per session/workspace, model catalog managed centrally, observability of every LLM call. A power user (or small team) uses cuartel as their universal AI interface, not just a coding-agent UI.

### GPU-attached sandboxes for research workloads
**[future]** Modal makes GPU a sandbox property. For cuartel: declare `Workspace { gpu: Some(GpuSpec) }` and the `HetznerSandbox` impl provisions a GPU-attached VM. Use cases: ML training runs ("Parameter Golf"), large-scale evals, fine-tuning experiments. Niche but high-value; differentiates from cloud-native solutions because users bring their own GPU box. *The research/experiment-running use case the user explicitly flagged interest in.*

### Skills marketplace / registry
**[future]** Skills (BB / Anthropic primitive) as community-shareable artifacts. Workspace imports a skill bundle ("react-testing-best-practices", "hetzner-debugging-runbook") and the agent loads them progressively. A small marketplace layer if and when we have a userbase.

### Cheap-model routing for cuartel-internal tasks
**[future]** Cloudflare runs their security agent on Kimi via Workers AI for 77% cost saving. For cuartel: route summarization, classification, transcript-naming, and other internal LLM calls to a cheaper model (Haiku, local llama.cpp, etc.) while reserving frontier models for the core agent loop. Reduces operating cost meaningfully.

### Higher-level sandbox SDK as alternative to in-VM ACP
**[future architectural]** Vercel's choice — agent on host, sandbox is a dumb file/exec backend reached via SDK — is a real alternative to v2's "ACP server in sandbox" if a future ACP server *properly* routes fs/exec via the existing `fs/read_text_file` etc. RPCs. Worth tracking: if claude-code-acp ever ships an "FS-via-ACP mode," v2's load-bearing decision (D2) could flip.

### Cross-device session handoff
**[future]** Workspace move (already in v2) but extended: start session on laptop, hand it to phone-mode (read-only chat surface on iPhone), or hand to a colleague's cuartel install. Builds on `Workspace::relocate` plus a thin web-view client.

### "Internal ops" generalist agent (BB-style)
**[future, off-mission]** BB is a generalist agent for internal company ops (Slack tickets, Snowflake queries, HubSpot updates). Cuartel could support a similar generalist mode by composing skills + service packages. Probably out of scope — cuartel's identity is "coding agent orchestrator," not "general agent for everything." Flag and move on unless the market signals otherwise.

### Generator-evaluator pair as a "ship it" mode
**[core]** Anthropic's most concrete recommendation: a separate evaluator agent reviews the generator's work. Generators systematically overrate their own output. For cuartel's "ship it" / auto-PR mode: spawn a second session as the evaluator (different model? same model with skeptical prompt?), it reviews against a sprint contract, and only opens the PR when it passes. Maps cleanly to two parallel sessions in the same workspace.

### Sprint contracts as a session entry mode
**[adjacent]** Before coding starts, the user (or a planner agent) and the generator-agent **negotiate testable success criteria**. The contract is checked into the workspace and becomes the session's success metric. Prevents scope drift mid-task. Surfaceable as a dedicated UI: "what does done look like?" → captured as a checklist that the agent must satisfy before declaring complete.

### Context-reset on long sessions
**[core]** Anthropic's "context anxiety" finding means we should design the session abstraction to support **context resets via structured handoff artifacts** — not just in-place compaction. When a session approaches context limits, write a handoff doc, spawn a fresh agent instance, hand the doc + open work-items, continue. Cuartel's `Session` model should support this from day one or it's painful to retrofit.

### Per-workspace AGENTS.md / docs/ scaffold
**[core]** When a user creates a new workspace in cuartel, we scaffold the OpenAI-style structure: ~100-line `AGENTS.md` as TOC + `docs/{design-docs,exec-plans/{active,completed},tech-debt-tracker.md,product-specs,references}`. UI for editing each file. **Plans as first-class repo artifacts** — when an agent starts a task, it writes a plan to `docs/exec-plans/active/<id>.md`; on completion, moves to `completed/`. The UI surfaces these as cards in the workspace view.

### Per-worktree dev-stack scaffold
**[adjacent]** When a session starts, the sandbox boots not just the workspace but also: app dev server (per-port, port-forwarded out), local observability stack (cuartel-bundled minimal Loki + Prometheus), database fixture if the workspace declares one. Agents query LogQL/PromQL as native tools. Inspired by OpenAI's per-worktree-everything pattern. Sandbox image template ships with these capabilities.

### Linter-as-teaching-channel
**[adjacent]** When cuartel exposes any internal linting/checking, format error messages so they're directly useful to agents (specific, actionable, suggest the fix). Surface lint failures *into* the agent's tool-call response, not just to the human. OpenAI's pattern: every constraint becomes self-explaining the moment the agent hits it.

### Doc-gardening / refactor-bot subagent role
**[future]** A scheduled background subagent (per-workspace cron) that scans for: stale docs vs current code, drift from coding conventions, deprecated patterns, dead code. Opens micro-PRs for review. Most automerged in <1 minute. OpenAI calls this "garbage collection." Continuous tech-debt repayment.

### Multi-agent C-compiler-style task coordination
**[future]** Carlini's pattern: N agents in parallel, file-locking task claiming via shared bare git repo, known-good oracle for parallelism on the unknown bits. For cuartel, this is the production-scale subagent orchestrator — N sandboxes work the same workspace, claim tasks via files, push branches, the user reviews PRs in batch. **The most ambitious version of subagent fan-out.**

### Cuartel-bundled minimal observability stack in sandbox image
**[adjacent]** OpenAI ships per-worktree Loki + Prometheus + Tempo. For cuartel: bake a tiny observability stack into the standard sandbox image so agents can ask "is this slow because X?" by querying logs/metrics. Adds maybe 30MB to the image; massive capability uplift for the agent. Especially useful when paired with visual frontend verification.

### Time-aware tool wrappers
**[future]** Carlini: "Claude can't tell time and will happily spend hours running tests." Cuartel could wrap exec tools to: emit progress at intervals, cap by wall-clock, do deterministic-random subsampling for repeatable expensive tools. Tool-level discipline that the agent benefits from without having to remember.

### Birds-eye command center view
**[core]** Single view shows every running session across every workspace: status (running/idle/blocked/needs-permission), model, sandbox kind, runtime location, current tool call, cost-so-far. Click to zoom into one session. **This is the visual abstraction a16z's RFP describes.** Architecturally trivial on top of v2 (the data exists in the sessions table); it's a UI surface that needs deliberate design. Should be a peer of the per-session view, not buried.

### Hotkey groups and batch prompts
**[core]** Strategy-game UX: assign sessions to a numbered group (`Cmd-1`, `Cmd-2`...), recall the group with one keystroke, queue the same prompt to every session in the group at once. Pairs naturally with multi-version-prompt-fan-out. Almost zero architectural cost — it's an input-router on top of existing Sessions.

### Drag-to-assign for skills, MCPs, tools
**[core]** Skills, MCPs, tools become **first-class visual objects** in the workspace UI, not buried text files. Drag a skill onto a session to load it into context; drag an MCP onto a workspace to add it to all that workspace's sessions; drag a tool to scope it tighter. The visual affordance is the entire point of "GUI for Agents."

### Workflow composition graph (visual handoff editor)
**[adjacent]** n8n / Zapier-style visual graph where nodes are agent sessions / skills / artifacts, and edges are handoff documents (Anthropic's context-reset substrate from 4.12). User defines: "session A produces a spec → session B implements it → session C reviews → opens PR." Cuartel runs the graph. **Combines context-reset semantics, generator-evaluator pattern, and visual abstraction in one feature.**

### Workspace blueprints / templates
**[core]** Save a workspace template (sandbox image config + AGENTS.md scaffold + skills + MCPs + access policy + dev stack) and instantiate on any new project with one click. Inspired by Factorio blueprints. Lets users (or organizations) ship "the way we set up agent workspaces here" as a shareable artifact.

### Time-control / session replay
**[adjacent]** Pause an agent mid-turn, scrub through its past tool calls and outputs, fork from any prior point. Strategy-game pause-and-rewind UX. Combined with append-only event log (Session as third pillar) this is mostly a UI on top of `getEvents()`.

### Multiplayer / shared workspaces
**[future v3]** Invite a teammate to a workspace; their cuartel-app sees the same sessions; hand off a session like passing a unit. Tailscale + workspace-as-shareable-object makes this much less work than starting from scratch. Move from non-goal to "design for it now."

### Agent capability cards
**[adjacent]** Visual representation of what each agent can currently do — model, loaded skills, available tools, permissions, cost so far. Like a unit card in a strategy game. Helps users understand "why can't this agent do X?" without reading config files.

### "Construction" vs "battle" mode
**[adjacent]** Explicit UI mode toggle. *Construction:* configure workspaces, install skills, edit AGENTS.md, set up MCPs. *Battle:* hands-off, watch agents work, queue orders. Reduces UI clutter when actively running, surfaces config when setting up.

### Production statistics dashboard
**[adjacent]** Aggregated metrics: cost-per-workspace, turns-per-session, lines-changed, success rate, time-to-PR. The "factory production graph" Factorio shows. Helps users tune their workspace setup.

### Replicas as named persistent agent profiles
**[core]** See section 7.8 for full analysis. A Replica is a named, persistent bundle of `{model, default skills, default MCPs, permission policy, blueprint, triggers}`. Sessions are spawned by a Replica. Subsumes BB's "@bb" generalist, OpenAI's role-specialized agents, the strategy-game "units" metaphor, and the workspace-blueprint scaffold pattern. **Likely the highest-leverage UX feature surfaced by this round of research.** Workspace-scoped first; personal-replicas as templates later.

### Blueprints — orchestration as state machine of {deterministic, agent} nodes
**[core]** Stripe's most important pattern (4.15). A Blueprint is a workflow defined in code: each node either runs deterministic code (linters, push, lint loop) or an agent loop (implement task, fix CI). Per-team customizable. **Visualizable as a graph** — exactly the visual-workflow-editor the Factorio thesis calls for. Maps cleanly onto the visual command center: drag agent-nodes and code-nodes onto a canvas, connect them, save as a reusable Blueprint. A Replica references a Blueprint. **This is the substrate that makes "ship it" mode reliable** — deterministic linter and CI nodes guarantee certain outcomes; agent nodes get the creative work.

### Toolshed — single centralized MCP server with per-replica curated subsets
**[adjacent]** Stripe's pattern (4.15) for MCP at scale. One MCP server, ~500 tools; each agent gets a curated subset. Add a tool once, every replica/agent can use it. For cuartel: ship a `cuartel-mcp-portal` that proxies to user-configured MCP servers, exposes per-replica tool selection, and (future) implements Cloudflare's Code Mode collapse pattern (4.8). Solves the MCP-context-bloat problem at root.

### "Minimum lovable Replica" sprint
**[core]** Concrete near-term work: ship just the manual-trigger Replica (named profile, drag-to-assign, no Blueprint integration, no triggers). Could be done in a weekend on top of v2 primitives once step 5 lands. Validates the abstraction with users before investing in Blueprints/triggers.

### Cursor-rules cross-agent compatibility layer
**[adjacent]** Stripe (4.15) standardized on Cursor's rule format and syncs to Claude Code's format. For cuartel: read `.cursor/rules/`, `.claude/`, `AGENTS.md`, `CLAUDE.md` — feed them all to whatever ACP server runs. Become the rule-format-agnostic UI; users don't have to fragment their rules per agent.

### "What's good for humans is good for agents" — share dev infra
**[core philosophy]** Recurring lesson from Stripe + OpenAI: pre-existing developer-productivity infrastructure (devboxes, linters, rule files, observability stacks) generalizes to agents. Cuartel's sandbox image should bake in: fast linters, pre-push hooks, observability stack, type-checking caches. The agent inherits all of it. **Don't build agent-only infrastructure.**

### Bring-your-own-LLM-subscription via CLI auth
**[adjacent]** Replicas (4.14) lets users authenticate with their existing Claude Code or Codex subscription via CLI — no double-paying for LLM compute. For cuartel: support `cuartel claude-auth` / `cuartel codex-auth` flows. Important for cost-conscious users.

### Visual Editor (DOM-element-picker) as Tier 0 computer use
**[core]** Polyscope's most polished feature (4.18). Embedded webview + a small JavaScript overlay that, on element click, captures the element's CSS selector, text, and position. Send to agent as structured context: "User selected `.hero h1` containing 'Welcome' at (240, 180); they say: 'change this heading to ...'." **Vastly cheaper than VNC** (no display server, no streaming, no Tier 2 desktop stack) and covers the most common frontend-edit case. Cuartel ships this as Tier 0 alongside Tier 1 (Playwright) and Tier 2 (full desktop) computer use. Pairs with the Workspace's `preview.url` config (`{{folder}}` placeholder) for per-session preview.

### Autopilot-style sprint UI (the user-stories experience)
**[core]** Polyscope's Autopilot (4.18) operationalizes Anthropic's sprint contracts (4.12) and Stripe's blueprints (4.15) as a UI feature. Implementation pattern: high-level goal → AI generates US-001..N stories with title/description/acceptance criteria → user reviews/edits/reorders via drag-and-drop → sequential execution in fresh agent sessions → each story records progress in `.context/progress.md` for the next story → crash recovery resets in-progress story to pending. **This is the visual face of Replicas + Blueprints** in cuartel's design (7.8). The user-story breakdown is also a natural unit of multiplayer (assign US-003 to a teammate).

### Multi-model consensus as a user-facing feature ("Opinions")
**[adjacent]** Polyscope's Opinions (4.18) ships Cloudflare's multi-agent consensus (4.8) as a one-click feature: ask the same question to multiple models in parallel, synthesize a consensus answer. For cuartel: trivial on top of multi-version-prompt-fan-out (already in product directions). Add a "synthesize" step at the end. UX as a dedicated tab next to the current session.

### Review tab as a separate session
**[core]** Polyscope's Review (4.18) is the generator-evaluator pattern shipped as UX. Click "Review" → opens a new tab with its own message history → built-in instructions tell the agent to use diff tools + AGENTS.md compliance + GH PR context → produces structured findings. Per-repo "Review Preference" appended to built-in instructions; per-repo "Review Model." For cuartel: literal second `Session` spawned from the same Workspace, marked as "evaluator" role, with hard-coded review prompt prefix.

### Tasks (one-click reusable prompts) as a near-term Skills lite
**[core]** Polyscope's Tasks (4.18) is `tasks: [{label, prompt}]` in the workspace config file. Sidebar shows a dropdown; click → spawn a fresh workspace with the prompt auto-sent. **Lighter than Skills** (which need progressive context loading). For cuartel: add `tasks: []` to the proposed `cuartel.json` (or AGENTS.md scaffold). Skills come later; Tasks come early.

### `cuartel.json` per-workspace config file
**[core]** Polyscope's `polyscope.json` shape is well-tested: `scripts.setup/archive`, `preview.url` with `{{folder}}` placeholder, `tasks: []`. Cuartel adopts the same pattern as `cuartel.json`. Backwards-compatible with OpenAI-style `AGENTS.md` (one is config, the other is documentation). Setup/archive scripts run inside the sandbox VM (different from Polyscope which runs them on host).

### "Clear context & Approve" plan-mode option
**[adjacent]** Polyscope's Plan mode (4.18) approval dialog has two paths: Approve (continue with current context) or **Clear context & Approve (start fresh agent session with only the approved plan)**. Operationalizes Anthropic's context-reset (4.12) at the natural decision boundary. Cuartel adds this to its plan-mode UX.

### Linked workspaces — cross-workspace read-only context sharing
**[adjacent]** Polyscope (4.18) lets Workspace A reference Workspace B's state in a prompt. Per-prompt link, read-only. Use cases: cross-repo coordination (frontend + API), supervisor pattern (one workspace reviews work in others), cross-referencing prior solutions. Smaller scope than full workspace move; complementary feature. Cuartel can ship this cheaply on top of the Workspace registry.

### CI auto-fix loop
**[core]** Polyscope (4.18) + Stripe Minions (4.15): when GitHub Actions checks fail on a workspace's PR, automatically re-prompt the agent with the failure context and push the fix. Bounded retries (Stripe: 1–2 rounds). Cuartel's "ship it" mode default behavior.

### Per-repo merge/PR prompts
**[adjacent]** Polyscope (4.18) lets each repo customize its merge and PR prompts (e.g., "use conventional commits" or "include a test plan"). Small UX detail with high ROI. Cuartel adds per-Workspace prompt overrides + per-Replica overrides.

### One-base-repo workspace mode
**[adjacent]** Polyscope (4.18) allows one workspace per repo to be "base-repo mode" (no clone, agent works directly in the user's checkout) for live dev-server scenarios. Useful escape hatch from always-clone default. Cuartel can offer the same as a per-Workspace flag, with strong warnings (no isolation, all changes immediate).

### Auto-detect dev environment integrations (Herd / Vite / Next / Rails)
**[future]** Polyscope's Laravel Herd integration (4.18) detects Herd setup and suggests a `polyscope.json` with appropriate scripts and preview URL. Generalize: detect common dev environments at workspace-add time and offer scaffold suggestions. Vite (`vite.config.js`), Next.js (`next.config.js`), Rails (`config/application.rb`), Django (`manage.py`), etc. **High-ROI onboarding feature.**

### Deterministic-hostname reverse proxy per branch + service (the Paseo trick)
**[core]** Paseo (4.19) ships `http://web.fix-auth.my-app.localhost:<daemon-port>` — every workspace × service gets a unique URL. WebSocket upgrades supported. **The single most clever infrastructure feature in this whole research pass.** Solves port conflicts during parallel dev work, makes preview URLs predictable, and lets agents pass URLs to each other without coordination. **Cuartel implements this twice:** (a) inside the sandbox image so services in the VM get auto-allocated hostnames, and (b) on the host so port-forwarded sandbox services map to deterministic host hostnames. Pairs with the existing port-forwarding infrastructure (phase 5e). The `*.localhost` resolves to 127.0.0.1 trick means zero DNS configuration on user's machine.

### Service-to-service env injection
**[core]** Paseo (4.19) injects `$PASEO_SERVICE_<NAME>_PORT` and `$PASEO_SERVICE_<NAME>_URL` for every peer service so agents/scripts can reach them without hardcoded ports. Frontend points at `$PASEO_SERVICE_API_URL`. Cuartel adopts as part of `cuartel.json` `scripts` section.

### Daemon + multi-client architectural split
**[strategic]** Paseo (4.19) runs a daemon that exposes a WebSocket API; clients (mobile, desktop, web, CLI) connect to it. Even for a Mac-only product, this separation enables: headless server use (run daemon on Mac Mini), mobile companion later, multi-machine setups, lower coupling, easier testing. **Cuartel could split `cuartel-app` into `cuartel-daemon` (owns Workspace/Session/Sandbox state) + `cuartel-app` (GPUI client that connects via WebSocket).** Substantial refactor but enables: mobile (iOS/Android cuartel app), web client, headless deployment, future multiplayer. Tradeoff: more moving parts vs cleaner abstractions. Worth a serious decision before step 5 lands.

### Local voice stack with ONNX models
**[adjacent]** Paseo (4.19) ships Parakeet-TDT 0.6B v3 INT8 (STT) + Kokoro-en (TTS), both CPU-only ONNX. Voice LLM is a hidden agent session using user's installed provider (e.g., Claude Haiku for fast voice responses). Optional OpenAI fallback. **All local by default — speech never leaves your network.** Cuartel ships the same. Voice + Replicas + scheduler = "Hey cuartel, every Monday at 9am, run the security audit Replica."

### Built-in cron scheduler
**[core]** Paseo (4.19) ships `paseo schedule create --cron "0 9 * * 1" "..."` as a first-class CLI command. Brilliantly simple. Cuartel ships this as part of Replica triggers (KB 7.8) or as a standalone scheduler before Replicas land. The minimum-lovable scheduler is just: cron expression + prompt + workspace ref → executor that calls `cuartel run` at the scheduled time. Pairs with notifications when scheduled work completes/needs-input.

### `--output-schema` for structured agent responses
**[adjacent]** Paseo (4.19) lets you constrain agent output to a JSON schema (`--output-schema schema.json` or inline). Enables: implement-verify loops in shell scripts, multi-agent coordination via structured handoffs, programmatic consumption of agent results. The implement+verify loop they document (Codex implements, Claude verifies as `{criteria_met: bool}`, loop until true) is foundational. Cuartel CLI/API exposes this primitive.

### End-to-end encrypted relay (alternative to Tailscale)
**[adjacent]** Paseo (4.19) offers a relay model: daemon connects outbound to a relay; clients pair via QR code; ECDH key exchange + AES-256-GCM. Relay is untrusted by design — even if compromised, can't read traffic. **Zero-config remote access** for users without Tailscale. Cuartel could offer this for the "remote daemon on Hetzner / Mac Mini" use case alongside Tailscale. Requires running relay infrastructure (small operational cost, $20/mo at small scale).

### Custom providers via config extension (Tier 0 Replicas)
**[core]** Paseo's `agents.providers` (4.19) extends first-class providers with new `id`, `label`, `env`, `models`, `additionalModels`, `disallowedTools`, `command`. Examples: `claude-work` and `claude-personal` with different API keys; `claude-via-zai` with Anthropic-compatible Z.AI endpoint; ACP-native `gemini` with `command: ["gemini", "--acp"]`. **Ship this as a Tier 0 of Replicas before building the full data model:** users get the named-agent UX through config alone. Replicas as a first-class concept can grow on top. Faster path to the visible feature.

### Worktrees as Tier 0 isolation alongside VM Tier 1
**[adjacent]** Polyscope (4.18) and Paseo (4.19) both use git worktrees as the only isolation primitive (no VM). For users who want zero-overhead local execution, **cuartel can offer worktree isolation as a `LocalSandbox` mode** alongside the `AppleVzSandbox` default. Trade-off: faster cold-start, no kernel isolation. Useful for tiny tasks, dev environments, and users who don't want VM overhead.

### CLI designed for agent self-invocation
**[adjacent]** Paseo's CLI (4.19) is structured so agents can call it themselves to spawn sub-agents and wait for results. Documented patterns: `paseo run --detach --name api-agent`; `paseo wait api-agent`; `paseo logs api-agent --tail 5`; structured-output verify loops. **Hierarchical multi-agent orchestration via shell scripts** — much simpler than building a graph orchestrator first. Cuartel's CLI exposes the same primitives.

### MCP-server-as-daemon-API for in-session sub-agent spawning
**[core]** Paseo's `mcp-server.ts` (KB 4.19.1) exposes an MCP server inside the daemon that running agents can call to spawn child agents. **The agent calls a tool** (e.g. `agent.spawn`) **and the daemon handles the lifecycle.** Caller-context inheritance: child agents inherit parent's mode, model, MCPs, system prompt, lockedCwd by default. **This is the substrate** for the Replica-spawning-Replica vision (KB 7.8) and for the subagent-pool-with-quota pattern (KB 4.9 Modal/OAI). Without this MCP server, agents can only orchestrate via shell-out to the CLI; with it, native programmatic spawn. Cuartel ships its own MCP server inside `cuartel-daemon` exposing `replica.spawn`, `replica.send_prompt`, `replica.wait`, `replica.logs`, `replica.permit`. Combined with output-schemas, enables visualizable orchestration *and* shell-script orchestration with the same primitives.

### Loop service as a first-class daemon primitive
**[core]** Paseo's `loop-service.ts` (KB 4.19.1) is a worker-and-verifier orchestrator built into the daemon, not an external script. Each iteration spawns a worker agent + a verifier agent. Verify can be **command-based** (run shell command, check exit code) OR **LLM-based** (verifier agent returns structured output). Bounded by `maxIterations`. **Cuartel ships this as the runtime substrate for `Blueprint`** (Stripe Minions pattern, KB 4.15) — Blueprints are state machines of `{deterministic, agent}` nodes; the Loop Service is the "agent loops with verification" subset. Pairs with `--output-schema` for structured verifier results.

### Multi-pass Zod tool-call normalization
**[core]** Paseo's `tool-call-mapper.ts` (KB 4.19.1) handles the messy reality that the same shell-exec tool is called `bash` / `Bash` / `shell` / `exec_command` across providers. Multi-pass discriminated-union pipeline maps to canonical kinds (`shell`, `read`, `write`, `edit`, `search`, `fetch`). **Cuartel will hit the exact same problem the moment we wrap >1 ACP server.** The UI needs canonical kinds to know what icon/treatment/permission-check to apply per tool call. Build this into `cuartel-acp` from day one — it's much harder to retrofit when N tool variants exist.

### Provider mode mapping table for cross-provider sessions
**[adjacent]** Paseo's `mapModeAcrossProviders()` (KB 4.19.1) maps `claude.plan ↔ codex.read-only`, `claude.default ↔ codex.auto`, `claude.bypassPermissions ↔ codex.full-access`. **When a session migrates between Replicas backed by different providers** (or when subagents spawn under different providers), modes need translation. Cuartel ships a similar map.

### Provider capability flags with graceful degradation
**[core]** Paseo's `DEFAULT_ACP_CAPABILITIES` (KB 4.19.1) declares per-provider flags like `supportsStreaming`, `supportsSessionPersistence`, `supportsDynamicModes`, `supportsMcpServers`, `supportsReasoningStream`, `supportsToolInvocations`. Client checks before invoking; falls back if absent. **Cuartel needs this when supporting heterogeneous ACP servers** — different agents have different capabilities, the UI must hide buttons for missing features rather than crash.

### Hidden-agent pattern for system services
**[adjacent]** Paseo uses a `voiceOnly: true` flag (KB 4.19.1) to isolate voice-mode's hidden agent from the user-visible session. Generalizable: cuartel could use the same pattern for any internal agent task (auto-naming a session from its first turn, generating a PR description, summarizing for the sidebar) — spawn a hidden agent, run it, dispose, never surface in the user's session list.

### Mechanical backwards-compatibility tests
**[future, fix-Paseo's-gap]** Paseo enforces backwards-compat (6-month-old client vs new daemon) **procedurally** via CLAUDE.md rules — no automated test that mechanically verifies. **Cuartel can ship better.** Concrete idea: snapshot the WebSocket protocol schema at every release; CI runs a test that an old-snapshot client can parse a new-daemon's responses and vice versa. Trivial with Zod schemas; substantial competitive trust signal for users running mixed-version setups (mobile lagging behind desktop, etc).

### Branch-named-URL-per-service reverse proxy via portless dependency
**[core]** **Decision: depend on `portless` (npm, Apache-2.0) rather than reimplement** the reverse proxy. Same logic as cuartel's "depend on shuru" decision (D6). What we get for free: HTTPS-by-default with local CA + system-trust auto-install (macOS/Linux/Windows), HTTP/2 multiplexing for unbundled dev servers, framework-specific port/host injection (Vite/Astro/Next/Expo/RN/Angular all hand-tuned), `/etc/hosts` sync for Safari, `508 Loop Detected` UX polish, worktree-aware branch prefix, custom TLD support, wildcard subdomains for multi-tenant dev, state persistence. **Implementation:** cuartel-daemon spawns a small Node sidecar that imports `portless`, exposes route registry over WebSocket; cuartel-app's session store pushes route updates as workspaces/sessions/services change. ~1 day of integration. Bake portless into the sandbox VM image too (in-VM hostnames for in-VM services). KB 4.20.

### LAN mode via mDNS for cross-device control without Tailscale
**[adjacent]** portless ships `--lan` mode (KB 4.20) that advertises services as `<name>.local` via mDNS — reachable from any device on the same Wi-Fi. **For cuartel:** when you're on the same network as your laptop, the mobile app can connect directly via mDNS without needing Tailscale or the e2e relay. Falls back to Tailscale/relay when off-network. Free as part of depending on portless. macOS uses `dns-sd` (built in); Linux uses `avahi-publish-address`.

### Computer use — agent drives the desktop / browser, records video artifacts
**[core, two-tier]** Cursor's most striking shipped feature (4.16). Agents in their VM can drive a browser or full desktop, record video of what they did, attach it to the PR. Cuartel can ship this on top of v2 with **no architectural changes** — purely VM-image scaffold + MCP-server packaging + UI panel work.

**Tier 1 (browser-only, ~weekend on top of step 3+5):**
- Bake **Playwright + Chromium** into the sandbox image.
- Ship `microsoft/playwright-mcp` as a default-bundled MCP server (MIT-licensed).
- Agent gets `navigate`, `click`, `fill`, `screenshot`, `pdf`, `recordVideo` for free.
- Artifacts (`/artifacts/*.mp4`, `*.png`, `*.html`) land in a virtiofs-mounted dir → instantly visible on host.
- New "Artifacts" panel in the per-session UI.

**Tier 2 (full desktop, larger but still no architectural change):**
- Add `Xvfb + openbox + xdotool + ffmpeg + x11vnc` to the sandbox image (~80 MB).
- Custom desktop MCP server (~200 LOC): `screenshot`, `click`, `move_to`, `type`, `key`, `recording.start/stop`, `list_windows`.
- Agent invokes recording around its work; produces a deterministic mp4 artifact.

**Bonus — live remote control (Cursor's "you can also control the agent's desktop"):**
- `x11vnc` exposes the same display server.
- Port-forward 5900 over the existing virtio-serial (Apple VZ) or Tailscale (Hetzner) — phase 5e port forwarding already exists.
- noVNC HTML/JS client embedded in a GPUI webview (cheap path) or `vnc-rs` decoded to a GPUI texture (native, faster).
- Pairs with the "construction vs. battle mode" UX (7.5): switch session view from transcript to live VNC pane to take over the desktop.

**Why this is well-positioned:**
- VM-image work is exactly what step 3 of the v2 roadmap already does. Add packages.
- Per-Replica config (section 7.8) decides which agents get browser/desktop tools — `frontend-claude` gets Playwright by default; `backend-codex` doesn't. No global config sprawl.
- Apple VZ Linux runs Xvfb fine in pure software (no GPU passthrough needed). Verified-relevant caveat: Apple VZ exposes a virtio-gpu device but Linux drivers may fall back to software for Xorg. Either way, agent doesn't need 60fps.
- Validates Cursor's "30% of merged PRs from cloud agents" milestone as architecturally proven.

**Strategic significance:** Cursor frames this as "the biggest shift in how we build software since the move from Tab autocomplete to working synchronously with agents." Cuartel can ship the same capability on the same architecture, with the visual command center on top — a compelling demo and a clear differentiator vs CLI-first tools.

---

## 7.6 Visual command center (a16z "GUI for Agents" thesis)

a16z Speedrun published an explicit RFP for "GUIs for Agents" (early 2026): "we're still in the MS-DOS era of agents today… one broad idea we're excited about are visual abstraction layers for agents… think of a GUI or visual command center inspired by strategy games (ex. Factorio) where agents and workflows are represented graphically." Strategy games already perfected multi-entity-with-imperfect-information UX 25 years ago: zoom, batch orders, hotkey groups, multiplayer.

**Why it's load-bearing for cuartel.** We're the only project surveyed that's both native macOS *and* architecturally ready for multi-agent orchestration. ChatGPT/Claude.app are single-thread chat. Cursor/OpenCode are editor sidebars. Vercel/Modal/E2B are infra without UI. The "native multi-agent command center" lane is wide open, and cuartel's existing Workspace + Session + Sandbox primitives are the correct building blocks.

**Concrete UX patterns to borrow from strategy games:**

- **Birds-eye view** (StarCraft minimap, Factorio map) — single view shows every running session, status, sandbox, model, runtime location. Click to zoom to one session.
- **Hotkey groups / control groups** — assign a key to a set of sessions; "1" recalls them as a unit; batch-issue prompts to the group.
- **Drag-to-assign** (Civilization advisors, RimWorld pawns) — drag a skill onto a session, drag an MCP onto a workspace, drag an agent onto a worktree.
- **Tile / placement abstractions** (Factorio belts, city builders) — visual graph of session → handoff artifact → next session, like a production line. Workflow composition without text.
- **Production statistics overlay** — per-workspace cost/turn, time/turn, lines-of-code-changed/session, success rate. Aggregated across all running agents.
- **Blueprint system** (Factorio) — save a workspace template (sandbox image + skills + MCPs + access policy + scaffold) and instantiate it on any new project.
- **Research / tech tree** — skills as a progression tree; "unlock" new agent capabilities by installing skill bundles.
- **Multiplayer / shared workspaces** — invite a teammate to a workspace; their cuartel sees the same sessions; hand off a session like passing a unit.
- **Time-control** (RTS pause / fast-forward) — pause an agent mid-turn, scrub through its past actions, resume from any point.
- **Construction mode vs. battle mode** — explicit mode toggle: "configuring/setting up" vs. "running with hands off."

**What this means for cuartel's roadmap (re-prioritization, not new architecture):**

- The Zed-style sidebar (one active session) is necessary but not sufficient. Need a **command-center view** as a peer-level UI. Step 5 (persistence + sidebar) should be re-scoped to "persistence + sidebar + command center."
- **Skills, MCPs, tools become first-class visual objects** with editable config, not buried text files. UI for installing/editing/scoping per-workspace.
- **Workflow composition** (visual graph of session handoffs) is plausibly a v2 feature, not v3. Anthropic's "context resets via handoff artifacts" pattern (4.12) is exactly the substrate this needs.
- **Hotkey groups + batch prompts** require almost no architectural work — they're a UI affordance over the existing Session abstraction. Should be early.
- **Workspace blueprints** map to "scaffold AGENTS.md + docs/ + sandbox image config" — fits the OpenAI Codex pattern (4.11).
- **Multiplayer** moves from non-goal to "v3 candidate, design for it now." Tailscale + workspace-as-shareable-object makes this much easier than starting from scratch.

**Strategic positioning question this forces.** Cuartel can be (a) power-user coding command center [vertical wedge], (b) general GUI for agents [a16z swing, much bigger but more competitive], or (c) (a) that becomes (b) over time. Strong recommendation: **(c).** The Workspace + Sandbox + Session + ACP architecture is generic; the macOS-native UI is the moat. Coding is the first workspace template; research/ops/personal-assistant are added as templates later, not rewrites. **Ship the coding wedge with platform-grade primitives.**

**The "Factorio for agents" elevator pitch** that consolidates all of this: *Cuartel is a native macOS command center for orchestrating dozens of AI coding agents in parallel — across local Apple VZ sandboxes, your own Hetzner box, and managed cloud providers — with skills, tools, and workspaces as visual objects you can configure, queue, batch, and share like units in a strategy game.*

That's the product, not just the architecture.

---

## 7.8 The "Replica" abstraction (proposed first-class concept)

The user asked: *is it interesting to offer "replicas" inside cuartel?* Two readings:

1. **Literal — integrate with Replicas (the company) as a backend.** No. They're a direct competitor on the autonomous-coding-agent axis (4.14). Their stack (Claude Code/Codex/OpenCode in a VM, multi-tenant SaaS, Linear/Slack/GitHub-driven) overlaps cuartel's category. Becoming their UI client would mean reselling a competitor's service.
2. **Conceptual — adopt "Replica" as a UX abstraction inside cuartel.** **Yes — and it's one of the highest-leverage product moves we've identified.** It unifies several patterns we've already collected and clicks into the strategy-game UX vision.

### What a Replica would be in cuartel

A **Replica** is a named, persistent, configured agent profile. It bundles:

```rust
pub struct Replica {
    pub id: ReplicaId,
    pub name: String,                       // "frontend-claude", "test-fixer", ...
    pub icon: ReplicaIcon,                  // visual identity (color, glyph)
    pub agent_server: AgentServerCommand,   // which model + binary
    pub default_skills: Vec<SkillId>,       // markdown context loaded by default
    pub default_mcps: Vec<McpServerId>,     // tool surface
    pub permission_policy: AccessPolicy,    // what it can touch, what's auto-approved
    pub blueprint: Option<BlueprintId>,     // orchestration template (Stripe pattern)
    pub triggers: Vec<ReplicaTrigger>,      // GitHub PR opened, Linear issue assigned, cron, manual
    pub workspace_scope: WorkspaceScope,    // which workspaces this replica is available in
}
```

**Sessions are spawned BY a replica** when it's triggered or summoned. The session inherits the replica's defaults but can override per-task. A workspace contains 0..N replicas; each replica is a "unit" the user can assign work to.

### Why this is the right abstraction

It unifies and makes first-class several patterns we've been treating as separate ideas:

| Pattern | Source | How Replica subsumes it |
|---|---|---|
| Named generalist agent ("@bb") | Browserbase | A Replica is the generalized form of "@bb" — but pluralizable per workspace |
| Role specialization (dedup, perf, quality, docs agents) | OpenAI Codex, Carlini | Each role becomes a separate Replica with its own skills + scope |
| Workspace blueprint scaffold | Factorio thesis | Replicas are saveable, shareable units in a workspace template |
| Per-thread permission scoping | Zed `work_dirs` | Replica's `permission_policy` is the per-replica version |
| Generator-evaluator pair | Anthropic harness design | Generator and Evaluator are two replicas; evaluator triggered on generator's PR |
| Strategy-game "units" | a16z thesis | Replicas ARE the units. Hotkey groups select sets of replicas. Drag-to-assign drops a task onto a replica |
| Multi-version prompt fan-out | Ramp | Send the same prompt to N different replicas in parallel |
| Skill system (BB) | BB / Anthropic | Skills attach to replicas (default + per-task) |

### What a user can do with replicas

Concrete UX vignettes:

- **"Tag your replica."** Create `frontend-claude` (Sonnet, frontend skills, design-system MCP, scoped to /frontend), `backend-codex` (Codex, db-migration MCP, scoped to /api), `test-fixer` (Haiku — cheap — test-running skill, no write outside /test). Cuartel's command-center shows three "unit cards." Drag a task onto one to start.
- **"Doc-gardener" replica with cron trigger.** Configure once. Runs daily at 3am. Scans `docs/` for staleness, opens micro-PRs for review. OpenAI Codex pattern, packaged as a reusable Replica.
- **Generator-evaluator pair.** `feature-builder` (Sonnet, sprint-contract skill) and `pr-reviewer` (Opus, evaluator skill). When `feature-builder` opens a PR, the trigger fires `pr-reviewer` automatically. Two replicas, one workflow. Anthropic's harness design (4.12) shipped as a UX feature.
- **Replica blueprint marketplace.** Save your replica config as a shareable template. Import a teammate's `react-test-fixer` replica into your workspace. Community marketplace later.
- **Hotkey groups.** `Cmd-1` selects all your replicas; `Cmd-2` selects only the frontend ones. Queue a prompt to a group. Strategy-game UX, finally clicking.
- **Replica capability cards.** Each replica's UI card shows: model, loaded skills, available MCPs, permission scope, current task (if running), cost-so-far. Like a unit card in StarCraft.

### What this changes architecturally (vs current v2)

**Not much, structurally.** v2 already has Workspace, Session, AgentServerCommand, AccessPolicy. Replica is a *named, persistent bundle* of these — primarily a data-model addition + UI surface. The session model still works:

- v2 today: User → Workspace → Session (inherits Workspace defaults).
- v2 + Replica: User → Workspace → Replica → Session (inherits Replica defaults, which inherit Workspace defaults).

Backwards compatible: a workspace with no replicas defined acts like today (defaults applied directly to session).

**Open architectural question:** does Replica live above or below Workspace?
- **Above:** `User → Replicas → Workspaces` — replicas are personal, work across workspaces. Like "my agents."
- **Below:** `User → Workspace → Replicas → Sessions` — replicas are workspace-scoped. Like "the units stationed in this workspace."
- **Both:** workspace-scoped by default, with a personal-replica option for cross-workspace ones (templates).

Recommend: **start workspace-scoped, add personal-replicas later as templates.** Easier to scope right, easier to extend.

### Naming risk

The name "Replica" is owned by replicas.dev as a product brand. Generic-noun trademark protection is weak (the word predates them by centuries — replicas of art, replicas of legendary swords, replicas of databases). But to avoid confusion in the market, alternatives worth considering:

- **Agent** — already overloaded in the AI space; ambiguous.
- **Crew** — collective noun, implies coordination. Crewmate? Crew member?
- **Minion** — Stripe owns the name internally; cute; might be too cute.
- **Operator** — overloaded by OpenAI Operator product.
- **Replica** — clean noun, fits the vision. Risk: brand confusion with replicas.dev.
- **Pawn** (RimWorld) / **Unit** (StarCraft) / **Worker** (Factorio) — strategy-game-native names. Maybe too gamer-niche.
- **Squad** / **Team** — collective; implies multi-agent.
- **Avatar** — a configurable persona; underused word.

Recommend keeping "Replica" as a working name internally; pick the public name when we ship the feature, with branding/marketing input. The concept is what matters; the name can flex.

### Where this fits in the roadmap

Replica is not a v2 step (it's a UX/data-model addition, not architectural). Best fit: **between current step 5 (persistence + sidebar + command center) and step 6 (Hetzner sandbox).** Concretely:

- **5a.** Replica data model in `cuartel-db` (table: `(id, workspace_id, name, agent_server_id, default_skills, default_mcps, policy, blueprint_id, triggers, created_at, updated_at)`).
- **5b.** Replica UI in command center (cards, drag-to-assign, hotkey-groups).
- **5c.** Trigger system (manual, GitHub webhook, cron, integration triggers — start manual, layer triggers later).
- **5d.** Blueprint integration (Stripe pattern — orchestration as state machine; Replica references a Blueprint).
- **5e.** Skill registry (per-workspace `.cuartel/skills/*.md`, draggable into replica defaults).

Each is incremental. **The minimum lovable Replica** (manual-trigger, no blueprint, no triggers) is implementable in a weekend on top of v2's existing primitives.

---

## 7.7 Strategic context: Anthropic Managed Agents

This deserves a separate section because it changes the long-game positioning.

**What Managed Agents is.** A hosted Anthropic product with three primitives: Brain (stateless harness), Hands (ephemeral sandboxes), Session (durable append-only event log). Same Brain/Hands split cuartel adopted in v2, but with Session as a separate first-class entity. API surface is small and OS-shaped: `execute(name, input)`, `getSession(id)`, `emitEvent`, `wake(sessionId)`, `getEvents()`. Anthropic explicitly designed for decade-scale stability — abstractions that outlast specific implementations.

**Why it matters for cuartel.** Two readings:

1. **Threat reading.** If Anthropic ships Managed Agents as a polished platform with their own UI clients, "build your own coding-agent orchestrator" becomes a smaller market. Devs who would have used cuartel may just use Claude.app or Anthropic's web UI on top of MA.

2. **Opportunity reading (the right one).** Cuartel's value isn't in *being* the brain or the hands. It's in being **the best macOS-native client for whatever sits behind the agent**, with: workspace mobility, your-own-infra options (local Apple VZ, your Hetzner box), parallel sessions across N agents/models, and the editor-grade UI Claude.app won't ship. **Cuartel competes on UX + infrastructure portability, not on agent infrastructure.**

**Concrete strategic implications.**

- **Watch MA's API.** When MA exposes a public interface, cuartel should be able to consume it as another `AgentServer` (or another `ACP transport`). The current decision (D1: ACP) doesn't preclude this — ACP and MA's interface may even converge, or cuartel can support both.
- **The Session-as-third-pillar refinement is non-optional.** If MA's `getEvents()` becomes the standard, cuartel's session model needs the same shape. Update v2 accordingly (next pass).
- **"Bring your own brain"** is the headline. Cuartel runs Claude via local API key, via MA, via Bedrock, via your-own-router — whatever — with the same UI and the same sandbox infra. *That* is the differentiation.
- **OS-virtualization framing should permeate cuartel's vocabulary too.** "Cuartel is to Anthropic Managed Agents what `cat`/`ls` are to a filesystem driver." Not entirely accurate, but it points at the right level of abstraction.

**Don't over-rotate yet.** MA's public availability and pricing aren't clear. Keep the v2 plan intact; revisit when MA ships a usable public API. But when designing the session/event model in step 5 (persistence + sidebar), look at MA's shape and make ours compatible by default.

---

## 8. Roadmap (high-level, from v2)

Each step ships a working artifact. Step 1 takes a day and proves the core hypothesis. Step 3 is the load-bearing refactor. Everything after is additive.

1. **`LocalSandbox` spike** — `claude-code-acp` as a child OS process on the host, hand-rolled ACP client. Proves V8-vs-OS hypothesis. *Throwaway code.*
2. **`cuartel-acp` crate** — wrap `agent-client-protocol`. Implement `fs/*` and `terminal/create` handlers (Gemini calls them even if Claude doesn't).
3. **`AppleVzSandbox` via shuru-vm + shuru-darwin** — Linux VM, ACP server baked in, vsock transport. **Replaces AgentOS secure-exec for local sessions.**
4. **Cut over `session_host.rs`** — `Workspace` + `Sandbox` + `cuartel-acp`. Delete gateway/loopback plumbing. Migrate SQLite for `sessions` table. Retire AgentOS secure-exec.
5. **Persistence + sidebar** — sessions table, retained-sessions pool, archive flow, notification windows. Lift Zed patterns directly.
6. **`HetznerSandbox`** — Firecracker microVM on Hetzner, Tailscale-attached, `RemoteStdio` transport. `RuntimeLocation::Remote(…)` becomes usable.
7. **Workspace move** — snapshot/transfer/rehydrate. Idle sessions only at first.
8. **Second agent** (Gemini or Pi) — config entry + ship binary in image. Validate cuartel code unchanged.
9. **(Future) Managed providers** — E2B and/or Daytona as proof of the abstraction. Each is a real adapter (~1–2 weeks).

---

## 9. Conversation thread (chronological digest)

Captured for future-session resumption. Each item is a turn or topic, not a transcript line.

1. **Why are sandboxes interesting for coding agents?** Discussed blast-radius containment, credential isolation (gateway pattern), reproducibility, safe autonomy, network policy. Surveyed cuartel architecture and confirmed macOS-only constraint.

2. **Senior-engineer review of v1 refactor doc.** Identified critical gaps: (a) FS virtualization for `claude-agent-sdk` not addressed, (b) remote sandbox latency budget missing, (c) tool-RPC protocol under-specified (no streaming, no PTY, no persistent shell), (d) implicit threat model with no MCP coverage, plus open questions on session lifecycle, hooks/permissions, observability, rollback, cost.

3. **User question on harness placement.** "If I have my own Hetzner box, can I run the harness there?" → Introduced the **Local-harness/Remote-sandbox vs Remote-harness/Remote-sandbox** distinction. Settled on **harness placement as a separate axis** from sandbox provider; harness co-located with sandbox eliminates per-tool RTT.

4. **Read Zed parallel-agents post + Proliferate.** Both surfaced **Workspace as a missing first-class concept** above Sessions. Proliferate's one-click runtime move = `RuntimeLocation` becomes architectural.

5. **Source dive: Zed (`agent`, `agent_servers`, `agent_ui`, `acp_thread`, `project`, `worktree`).** Discovered ACP. Mapped Project/Worktree/Thread, AcpThread/AgentConnection, AgentServer trait, retained-threads pool, notification windows, SQLite schema, tool permissions, per-thread worktree scoping. Wrote first "what to copy / what to leave" memo.

6. **Source dive: claude-code-acp + Zed `acp.rs`.** Discovered claude-code-acp does NOT route fs ops via ACP (CLI runs everything locally). Discovered Zed already has the remote-spawn pattern via `project.remote_client().build_command_with_options(..., Interactive::No)`. Concluded: **put the ACP server inside the sandbox** rather than virtualizing FS.

7. **Wrote `ARCHITECTURE_REFACTOR_V2.md`** with: ACP adoption, ACP-in-sandbox decision, `Workspace` first-class, three-axis design, persistence/sidebar/move sections, migration plan, non-goals, references.

8. **One-line cuartel description requested.** Settled on: "Cuartel is a native macOS desktop client that orchestrates parallel coding-agent sessions inside isolated sandbox VMs — local or co-located in the cloud — by speaking ACP, so credentials stay on your laptop and long-running work survives sleep."

9. **User question: "Won't we hit the same hang inside the sandbox?"** Critical catch. v2 had glossed over **V8-isolate vs real-OS distinction**. Patched v2 to add explicit "Sandbox kind: V8 isolate ≠ real OS" section, retired AgentOS secure-exec from agent runtime, named Apple VZ + Firecracker as concrete techs.

10. **Source dive: SuperHQ Shuru.** Found the open-source Rust microVM library with same Apple VZ entitlement and same goal. Apache-2.0. Decided to depend (D6) rather than reimplement.

11. **User question: "Can we build our own library? And how do managed providers fit?"** Two-part answer: (a) not yet — depend on shuru, fork later if needed; (b) managed providers each get their own `Sandbox` impl and `AcpTransport` variant, no common protocol, ~1–2 weeks per integration. Warned about Cloudflare Workers (V8) vs Containers (real).

12. **User shared Browserbase BB article + asked for this knowledge base.** Digested BB patterns, cross-referenced against existing decisions, captured here.

13. **Round-2 source pass: Ramp Inspect (builders + Modal), Cloudflare internal AI stack, Modal + OpenAI Agent SDK, Vercel Open Agents.** Massive industry convergence on the patterns we've already chosen (harness/compute split, snapshot-based fast cold-start, credential brokering). Genuinely new patterns added: Code Mode for MCP context efficiency (Cloudflare); subagent pool with quota (Modal/OAI); per-session SQLite at scale (Ramp); block-writes-allow-reads cold-start UX (Ramp); GitHub App tokens scoped per-repo (Ramp). Surfaced ~15 product directions in section 7.5 — including the multi-version prompt fan-out, voice-driven async kickoff, visual frontend verification, and the GPU-attached sandbox / research-workload use case the user explicitly flagged interest in. Vercel's "agent on host, sandbox via SDK" architecture noted as a v3 option to track.

14. **Round-3 source pass: OpenAI Codex internal beta (harness engineering), Anthropic harness design for long-running apps, Anthropic Managed Agents, Anthropic Carlini's C compiler.** The model labs themselves. Strongest validation yet of v2 (Anthropic Managed Agents has the same Brain+Hands abstraction). Critical refinement: **Session is a third pillar** (append-only event log), not a row in SQLite. Three new architectural patterns to fold in: (a) generator-evaluator separation with sprint contracts (Anthropic), (b) context resets via handoff artifacts to fight "context anxiety" (Anthropic), (c) lazy sandbox provisioning for 60–90% TTFT win (Anthropic). OpenAI's deep insight is **agent legibility** — the codebase optimizes for the agent's reading experience, with `AGENTS.md` as table-of-contents and `docs/` as structured system-of-record. Carlini's C compiler proves multi-agent coordination at 16-instance scale via shared bare git + file-locking. New strategic section 7.7 added on Managed Agents as platform-not-competitor: cuartel's positioning is best-macOS-native-client + workspace mobility + bring-your-own-infra, not building agent infra to compete with the labs.

15. **a16z Speedrun "GUI for Agents" RFP.** Explicit thesis that current agent UX is MS-DOS-era and the visual abstraction layer is a category-defining opportunity. Strategy-game UX (Factorio reference) for multi-agent management. **The single biggest reframing of cuartel's positioning to date.** Cuartel is the only project we've researched that's both native macOS *and* architecturally ready for multi-agent orchestration — the lane is wide open. Re-prioritizes: command-center view becomes peer-level UI (not buried), skills/MCPs/tools become first-class visual objects (not text files), workspace blueprints / hotkey groups / drag-to-assign become near-term, multiplayer moves from non-goal to "design for it now." New section 7.6 added with the strategy-game UX pattern list. New strategic recommendation: ship coding-vertical wedge with platform-grade primitives, generalize to "GUI for all agents" as workspace templates later. Elevator pitch: *Cuartel is a native macOS command center for orchestrating dozens of AI coding agents in parallel — across local Apple VZ sandboxes, your own Hetzner box, and managed cloud providers — with skills, tools, and workspaces as visual objects you can configure, queue, batch, and share like units in a strategy game.*

16. **Round-4 source pass: Replicas (replicas.dev) + Stripe Minions parts 1+2.** Replicas is a direct competitor on the autonomous-coding-agent axis (built on Claude Code/Codex/OpenCode, multi-tenant SaaS, Linear/Slack/GitHub-driven) — *not* infra to integrate with, but the brand-name "Replica" is a powerful UX concept worth adopting independently. Stripe Minions is one of the densest sources we've read: ships >1,300 unattended PRs/week via "Blueprints" (state-machine of deterministic + agent nodes, per-team customizable, visualizable), "Toolshed" (single centralized MCP server with per-agent curated subsets ~500 tools), pre-warmed devbox pool (10s ready), Cursor rules as cross-agent format, no-confirmation autonomous mode in contained sandboxes, bounded CI iteration (1–2 rounds), forked block/goose rather than write own loop. **Two new first-class abstractions proposed in section 7.8 + product directions:** **Replica** (named persistent agent profile bundling model + skills + MCPs + policy + blueprint + triggers — subsumes BB's @bb, OpenAI role specialization, strategy-game units, generator-evaluator pair) and **Blueprint** (Stripe's deterministic+agent state-machine — substrate for "ship it" mode and the visual workflow editor). Both fit step 5 of the v2 roadmap; minimum-lovable Replica is implementable in a weekend on existing v2 primitives. Also: bring-your-own-LLM-subscription via CLI auth (Replicas pattern), cuartel-mcp-portal as cross-replica capability layer (Toolshed pattern), cursor-rules cross-agent compatibility (Stripe pattern).

17. **Round-5 source pass: Cursor cloud agents with computer use (Feb 2026).** Cursor shipped cloud agents that drive their own VM desktop/browser, record video artifacts, and let users take over the remote desktop. ">30% of merged PRs at Cursor are now created by cloud agents." User asked how cuartel could implement this. Answer: **no architectural changes needed** — purely VM-image scaffold + MCP-server packaging + UI panel work on top of v2 step 3 + 5. Two-tier plan: Tier 1 = Playwright + microsoft/playwright-mcp (browser-only, ~weekend, MIT-licensed), Tier 2 = Xvfb + openbox + xdotool + ffmpeg + x11vnc + custom desktop MCP (~200 LOC), full desktop control + live noVNC takeover via existing port-forwarding (phase 5e). Per-Replica config decides which agents get browser/desktop tools (`frontend-claude` gets Playwright, `backend-codex` doesn't). Local Apple VZ uses Xvfb software rendering; remote Hetzner same image, VNC port-forwarded over Tailscale. New section 4.16 + new product direction in 7.5. Strategic note: Cursor frames this as "the biggest shift in how we build software since the move from Tab autocomplete to working synchronously with agents." Cuartel can match the capability with the visual command center as differentiator.

18. **Round-6 source pass: Polyscope (`getpolyscope.com`).** Native macOS multi-agent coding app from Beyond Code (Laravel ecosystem). **The closest competitor we've found** — at the time. Same product vision as cuartel — native Mac, multi-agent parallel, workspace-per-task, rich UI — already paid product (Paddle), strong Laravel-community traction. Different infrastructure choices: CoW clone on host filesystem (no isolation), wraps vendor CLIs (Claude/Codex/Cursor) instead of ACP, Vue+Inertia+Laravel stack instead of Rust+GPUI, local-only (no remote runtime). New section 4.18 added with full feature digest + closest-competitor analysis table + differentiation playbook. Cuartel's wedges vs Polyscope: real VM isolation (security story), remote runtime + workspace mobility (long-running cloud sessions), strategy-game command center UX (a16z thesis differentiator), ACP standard (cleaner add-a-vendor story), GPU sandboxes (research/ML use case). Lots of features to STEAL (high-confidence wins, all shipped at Polyscope's scale): Visual Editor (DOM-element-picker, much cheaper than VNC), Autopilot (sprint contracts operationalized as user-story UI with drag-reorder + crash recovery), Review tab (generator-evaluator as a one-click feature), Opinions (multi-model consensus as user feature), Tasks (one-click reusable prompts in `polyscope.json` — lighter than Skills), `cuartel.json` config file pattern, plan-mode "Clear context & Approve", linked workspaces, CI auto-fix loop, per-repo merge/PR prompts, one-base-repo workspace mode, dev-environment auto-detect (Herd/Vite/Next pattern). 12 new product directions added in 7.5; 13 new pattern rows in section 5. **Strategic conclusion:** Polyscope is the project cuartel needs to beat directly. Win by going **deeper on infrastructure** (VMs, remote runtime, mobility) **and bigger on UX** (command center, Replicas, strategy-game affordances), not by replicating their feature list.

19. **Round-7 source pass: Paseo (`paseo.sh`).** **The most architecturally similar project we've found**, supplanting Polyscope as the most direct comparison. Open source, free, self-hosted, daemon + multi-client (mobile + desktop + web + CLI), 4.7k GitHub stars, built by independent dev "Mo." Validates many cuartel bets (ACP via custom providers, remote-runtime via daemon-on-VPS, multi-provider via config extension, voice via local ONNX models). Surfaces several brilliant features cuartel should steal — most notably: **deterministic-hostname reverse proxy per branch+service** (`web.fix-auth.my-app.localhost`) which eliminates port conflicts in parallel dev (the single cleverest infra feature in this whole research pass); service-to-service env injection; daemon+multi-client architectural separation; built-in cron scheduler; `--output-schema` for structured agent responses; end-to-end encrypted relay alternative to Tailscale; local voice stack with ONNX models; custom providers via config extension (Tier 0 Replicas). New section 4.19 added with full feature digest + architecture comparison table + steal-list + cuartel's-wedges + the uncomfortable strategic question (how does paid cuartel compete with free Paseo?). Honest answer: ship visibly-better-UX (command center + Replicas as units + native polish) on platform-grade infra (Apple VZ + Hetzner + ACP); consider open-sourcing parts to compete on community. **The most consequential strategic finding**: Paseo's existence raises the bar; cuartel must articulate clear paid-value over free Paseo, not just "different feel." 12 new product directions in 7.5 (deterministic hostnames, service env injection, daemon split, voice, scheduler, output schemas, e2e relay, custom-provider Tier-0-Replicas, worktree Tier-0-isolation, CLI for agent self-invocation, etc.). 14 new pattern rows in section 5.

20. **Round-8 source pass: Paseo source-grounded deep dive (cloned `getpaseo/paseo` AGPL-3.0).** Read the actual implementation. New section 4.19.1 with file:line citations for: WebSocket protocol shape (JSON, not binary multiplex; Zod discriminated unions; backward-compat via `.optional()` + `.passthrough()` + ACP capability flags), reverse proxy implementation (`script-proxy.ts:38-145` ScriptRouteStore + `script-hostname.ts` slugifier + `script-route-branch-handler.ts` rename event handler — implementable in cuartel in ~1 day), tool-call normalization via multi-pass Zod discriminated unions (`tool-call-mapper.ts:113-168` — collapses messy provider tool-name variants to canonical kinds; cuartel hits this same problem the moment we wrap >1 ACP server), provider mode mapping (`mcp-server.ts:102-141` — `claude.plan ↔ codex.read-only`), MCP-server-as-daemon-API for sub-agent control (`mcp-server.ts:318+` with caller-context inheritance for child agents — substrate for Replica-spawning-Replica), Loop service as first-class daemon primitive (worker+verifier with bounded iterations, command-or-LLM verify — substrate for cuartel's `Blueprint`), epoch-based timeline reset instead of compaction (`agent-timeline-store.ts:130-191` — beautifully simple), tweetnacl-based ECDH+AEAD relay (`encrypted-channel.ts:89-225` Curve25519 + XSalsa20-Poly1305, daemon keypair persisted, QR transports daemon's pub key only), voice as `voiceOnly: true` hidden-agent isolation, custom 5-field cron parser in ~200 LOC (`schedule/cron.ts` — feasible cuartel re-implementation), `--output-schema` re-prompting (max 2 retries with Zod validation), provider capability flags for graceful degradation (`acp-agent.ts:92-99`). 9 new pattern rows in section 5 (multi-pass tool normalization, provider mode mapping, MCP server for sub-agent control, caller-context inheritance, loop service, epoch timeline reset, hidden-agent pattern, capability flags, cron parser size, output-schema re-prompting, tweetnacl relay). 8 new product directions in 7.5 (MCP-server-as-daemon-API, loop service, multi-pass tool normalization, provider mode mapping, capability flags + graceful degradation, hidden-agent pattern, mechanical backwards-compat tests as a cuartel-better-than-Paseo opportunity). **AGPL implication called out:** patterns are fair game, direct code copying isn't (would force cuartel to AGPL); clean-room re-implementation of patterns is the right approach. **Confirmed surprise:** WebSocket is JSON not binary mux (simpler than docs implied); backward-compat is procedural not mechanical (cuartel's opportunity to ship better).

21. **Round-9 source pass: vercel-labs/portless (Apache-2.0).** User flagged it as relevant to the localhost/port stuff. Turned out to be a major build-vs-buy decision: portless is a polished, Apache-2.0, Vercel-maintained npm library that does **exactly** the deterministic-hostname-per-service reverse proxy that Paseo's `script-proxy.ts` does — but as a separately-maintained package with library API exposed (`packages/portless/src/index.ts` exports `proxy`, `routes`, `hosts`, `types`, `utils`). What it adds beyond Paseo: local CA generation + system trust on macOS/Linux/Windows (`certs.ts` is 1096 LOC — substantial), HTTP/2 multiplexing for unbundled dev servers, hand-tuned framework-specific port/host injection (Vite/Astro/Next/Expo/RN/Angular), automatic `/etc/hosts` sync for Safari, mDNS LAN mode for cross-device access without Tailscale, proxy loop detection (`508 Loop Detected`), custom TLD support, wildcard subdomains. License is Apache-2.0 — cuartel can depend on it directly without copyleft concerns. **Decision (KB 4.20): depend on portless rather than reimplement.** Same logic as cuartel's "depend on shuru" decision (D6). cuartel-daemon spawns a Node sidecar that imports portless, exposes route registry over WebSocket; cuartel-app's session store pushes route updates. ~1 day of integration. Bake into sandbox VM image too. New section 4.20, 8 new pattern rows in section 5, 2 new product directions (portless dependency + LAN mode via mDNS as Tailscale-alternative). Strategic implication: cuartel doesn't need to build the reverse proxy at all; it gets the killer infra feature for free + a Tailscale alternative for same-Wi-Fi mobile control.

---

## 10. Reference index

URLs, file paths, and key citations for quick lookup.

### Cuartel internal docs
- `ARCHITECTURE_REFACTOR.md` — v1, superseded
- `ARCHITECTURE_REFACTOR_V2.md` — current target
- `DEBUGGING_NOTES.md`
- `SPEC.md`
- `KNOWLEDGE_BASE.md` — this file

### Zed
- Repo: `github.com/zed-industries/zed`
- Parallel agents post: `zed.dev/blog/parallel-agents`
- Key crates: `agent`, `agent_servers`, `agent_ui`, `acp_thread`, `acp_tools`, `project`, `worktree`
- Local sparse-checkout (research): `/tmp/zed-research/zed/`

### claude-code-acp
- Repo: `github.com/zed-industries/claude-code-acp`
- Local clone (research): `/tmp/zed-research/claude-code-acp/`
- Key file: `src/acp-agent.ts`

### Shuru
- Repo: `github.com/superhq-ai/shuru`
- License: Apache-2.0
- Local clone (research): `/tmp/zed-research/shuru/`
- Key crates: `shuru-darwin` (Apple VZ), `shuru-vm` (trait), `shuru-proto` (wire), `shuru-proxy` (network/secrets), `shuru-guest` (in-VM agent)

### ACP (Agent Client Protocol)
- Rust crate: `agent-client-protocol`
- Wire format: JSON-RPC over stdio (lines)
- Reference impls: claude-code-acp, gemini-cli

### Inspiration (UX / architecture)
- Proliferate: `proliferate.com` (workspace mobility)
- Browserbase BB: internal-agent architecture post (skills, snapshots, RBAC+ABAC, credential brokering)
- Ramp Inspect builders post: `builders.ramp.com/post/why-we-built-our-background-agent` (background agents, multi-surface UI, ~30–50% PR generation)
- Modal × Ramp case study: `modal.com/blog/how-ramp-built-a-full-context-background-coding-agent-on-modal` (snapshot-diff storage, hundreds of parallel sessions)
- Cloudflare internal AI stack: `blog.cloudflare.com/internal-ai-engineering-stack` (AI Gateway, Code Mode, AGENTS.md at scale, Engineering Codex)
- Modal × OpenAI Agent SDK: `modal.com/blog/building-with-modal-and-the-openai-agent-sdk` (SubAgentPool, quota guardrails, snapshots-as-on-disk-memory, GPU sandboxes)
- Vercel Open Agents: `github.com/vercel-labs/open-agents` (web→workflow→sandbox three-layer split, agent-outside-sandbox alternative, durable workflows)
- OpenAI Codex Harness Engineering: `openai.com/index/harness-engineering/` (100% agent-generated codebase, AGENTS.md as TOC, agent legibility, garbage-collection agents, custom-lints-as-teaching)
- Anthropic harness design for long-running apps: `anthropic.com/engineering/harness-design-long-running-apps` (context anxiety, generator-evaluator, sprint contracts, context resets via handoff artifacts)
- Anthropic Managed Agents: `anthropic.com/engineering/managed-agents` (Brain+Hands+Session three-pillar abstraction; OS-virtualization framing; lazy provisioning; stateless harness with `wake(sessionId)`)
- Anthropic Building a C Compiler with Claude (Carlini): `anthropic.com/engineering/building-c-compiler` (16-agent parallel coordination, file-locking task claiming via shared git, known-good oracle pattern, role specialization, ~$20K for 100K-line working compiler)
- Replicas: `replicas.dev` + `docs.replicas.dev` (autonomous coding-agent SaaS built on Claude Code/Codex/OpenCode, Linear/Slack/GitHub integrations, BYO-LLM-subscription via CLI auth — direct competitor in category; brand name "Replica" adopted as cuartel UX abstraction)
- Stripe Minions Parts 1+2: `stripe.dev/blog/minions-stripes-one-shot-end-to-end-coding-agents` (Blueprints as orchestration state-machine of det+agent nodes, Toolshed centralized MCP, pre-warmed devbox pool ready in 10s, fork of block/goose, no-confirmation autonomous mode in contained sandboxes, 1300+ unattended PRs/week)
- Cursor cloud agents with computer use (Feb 2026): `cursor.com/blog` ("agents can now control their own computers" — VM-resident browser/desktop, video artifacts, live remote control; >30% of internal merged PRs from cloud agents)
- **Polyscope** (closest competitor — native Mac multi-agent coding, paid SaaS, Laravel-community traction): `getpolyscope.com` + `getpolyscope.com/docs`. Built by Beyond Code. CoW workspace clones on host (no VM), wraps Claude/Codex/Cursor CLIs, ships Visual Editor + Autopilot + Review + Opinions + Tasks + Linked workspaces.
- **Paseo** (most architecturally similar — open source, free, daemon + multi-client): `paseo.sh` + `paseo.sh/docs` + `github.com/getpaseo/paseo`. Built by independent dev "Mo." Worktrees as isolation, ACP via custom providers, daemon-on-VPS for remote, mobile + desktop + web + CLI clients, e2e-encrypted relay, local voice (ONNX Parakeet + Kokoro), built-in cron scheduler, deterministic-hostname reverse proxy per branch+service.
- **portless** (Apache-2.0, Vercel-maintained library + CLI for branch-named-URL-per-service reverse proxy): `github.com/vercel-labs/portless`, npm `portless`. KB 4.20. **Cuartel depends on this** rather than reimplementing — gets HTTPS-by-default with system-trust CA, HTTP/2 multiplexing, framework-specific port/host injection, `/etc/hosts` sync, mDNS LAN mode, loop detection. License-friendly; same dependency pattern as shuru.

### Industry direction
- Anthropic Managed Agents
- OpenAI Agent SDK
- Both separate harness from compute — vindicates v2.
- Convergent patterns across Ramp, BB, Cloudflare, Modal, Vercel: harness/compute split, snapshot-based fast cold-start, credential brokering at network layer, per-session compute isolation, multi-surface UI. **The market has settled** on the infrastructure layer.
- a16z Speedrun "GUIs for Agents" RFP (early 2026): the *UI* layer is wide open and explicitly fundable as platform-scale category. Visual command center / strategy-game UX referenced.

---

## 11. How to use this document

- **Starting a new session?** Read sections 1–3 first. Then skim section 9 (conversation thread) for what was discussed before.
- **Designing a new component?** Check sections 4 and 5 for prior art and which patterns we've already committed to.
- **Stuck on a tradeoff?** Check section 6 (open questions) — it may already be flagged. If not, add it.
- **Making an architectural decision?** Add it to section 3 with rationale. Update the relevant rows in the patterns table (section 5).
- **Discovered a new external source?** Add a digest to section 4. Cross-reference adopted patterns into section 5.
- **Roadmap shifted?** Update section 8.

This document is the source of truth for cuartel's design context. The architecture doc (`ARCHITECTURE_REFACTOR_V2.md`) is the source of truth for the *target* design itself. Keep both in sync.
