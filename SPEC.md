# Cuartel

A 100% Rust native macOS app, GPU-rendered with Metal via GPUI. Orchestrates AI coding agent sessions in isolated VMs using Rivet AgentOS, with local and remote execution via Tailscale.

---

## Architecture Overview

```mermaid
graph TB
    subgraph "Mac App (Rust + GPUI/Metal)"
        UI[cuartel-app<br>GPUI Views]
        CORE[cuartel-core<br>Business Logic]
        RC[cuartel-rivet<br>Rivet Client]
        REM[cuartel-remote<br>Tailscale]
        DB[cuartel-db<br>SQLite]
        TERM[cuartel-terminal<br>GPU Terminal]
    end

    subgraph "Local Sidecar (Node.js)"
        RL[Rivet AgentOS<br>localhost:6420]
        VM1[VM: Claude Code]
        VM2[VM: Codex]
        VM3[VM: Pi]
    end

    subgraph "Remote (Hetzner via Tailscale)"
        RR[Rivet AgentOS<br>100.x.y.z:6420]
        VM4[VM: Long-run Job]
        VM5[VM: CI Agent]
    end

    UI --> CORE
    UI --> TERM
    CORE --> RC
    CORE --> REM
    CORE --> DB
    RC -->|HTTP/WS| RL
    RC -->|HTTP/WS via Tailscale| RR
    REM -->|Tailscale API| RR
    RL --> VM1
    RL --> VM2
    RL --> VM3
    RR --> VM4
    RR --> VM5
```

### Key Architectural Decisions

| Decision | Choice | Rationale |
|---|---|---|
| UI Framework | **GPUI** (gpui crate from Zed) | Metal-native GPU rendering, proven terminal support, SuperHQ validates the approach |
| VM/Sandbox | **Rivet AgentOS** | Unified API for local + remote, built-in persistence, multi-agent orchestration, actor model |
| Rivet integration | **Node.js sidecar** managed by the Rust app | AgentOS is a Node.js runtime; Rust app spawns/manages the sidecar, communicates via HTTP/WebSocket |
| Local storage | **SQLite** (AES-256-GCM for secrets) | Workspace config, credentials, server registry, session metadata |
| Remote connectivity | **Tailscale** | Encrypted mesh networking, no port forwarding/VPN setup, Rust crate available (`tailscale-api`) |
| Credential security | **Auth gateway pattern** | Credentials never enter VMs; injected on-the-fly into outgoing API requests by host-side proxy |

---

## Data Flow: Agent Session Lifecycle

```mermaid
sequenceDiagram
    participant U as User
    participant App as cuartel (Rust)
    participant R as Rivet AgentOS
    participant VM as Agent VM

    U->>App: Create session (Pi, workspace X)
    App->>R: getOrCreate(["workspace-X"])
    App->>R: createSession("pi", {env})
    R->>VM: Boot VM + install software
    R-->>App: sessionId
    App->>R: sendPrompt(sid, "Fix bug #42")
    R->>VM: Execute agent
    VM-->>R: SSE stream (events)
    R-->>App: sessionEvent stream
    App-->>U: Terminal output (GPU rendered)
    U->>App: Review changes
    App->>R: readFile("/workspace/...")
    App-->>U: Unified diff view
    U->>App: Approve changes
    App->>App: Apply overlay to host FS
```

---

## Crate Structure

```
cuartel/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── cuartel-app/              # Main GPUI application binary
│   │   └── src/
│   │       ├── main.rs           # Entry point, GPUI app init
│   │       ├── app.rs            # Global app state, menu bar
│   │       ├── workspace.rs      # Workspace view (container)
│   │       ├── sidebar.rs        # Session list, server list
│   │       ├── tab_bar.rs        # Agent tabs within workspace
│   │       ├── diff_view.rs      # Unified diff review panel
│   │       ├── ports_panel.rs    # Port forwarding management
│   │       ├── settings.rs       # Settings: keys, agents, servers
│   │       └── theme.rs          # Color scheme, fonts
│   │
│   ├── cuartel-terminal/         # GPU-accelerated terminal emulator
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── terminal.rs       # PTY management, ANSI parsing
│   │       └── renderer.rs       # GPUI element for terminal grid
│   │
│   ├── cuartel-core/             # Core business logic (no UI deps)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── session.rs        # Session lifecycle, state machine
│   │       ├── agent.rs          # Agent harness registry (Pi, CC, etc.)
│   │       ├── checkpoint.rs     # Checkpoint/rewind logic
│   │       ├── overlay.rs        # Overlay FS diff computation
│   │       ├── auth_gateway.rs   # Credential injection proxy
│   │       └── config.rs         # App configuration
│   │
│   ├── cuartel-rivet/            # Rivet AgentOS client (HTTP/WS)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── client.rs         # HTTP + WebSocket client
│   │       ├── sidecar.rs        # Spawn/manage local Node.js process
│   │       ├── vm.rs             # VM CRUD, lifecycle
│   │       ├── session.rs        # Agent sessions, prompts, events
│   │       ├── filesystem.rs     # File read/write/diff
│   │       └── network.rs        # Port forwarding, vmFetch
│   │
│   ├── cuartel-remote/           # Remote server management
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── tailscale.rs      # Tailscale discovery + connectivity
│   │       ├── server.rs         # Remote server registry
│   │       └── sync.rs           # Session push/pull between locations
│   │
│   └── cuartel-db/               # SQLite persistence
│       └── src/
│           ├── lib.rs
│           ├── schema.rs         # Tables: workspaces, credentials, servers
│           └── crypto.rs         # AES-256-GCM for secrets at rest
│
├── rivet/                        # Rivet AgentOS sidecar config
│   ├── package.json              # rivetkit + agent-os packages
│   ├── server.ts                 # AgentOS server entry point
│   └── tsconfig.json
│
├── migrations/                   # SQLite migrations
├── assets/                       # App icon, fonts
├── Info.plist                    # macOS app bundle metadata
├── entitlements.plist            # Virtualization, networking entitlements
└── scripts/
    └── package.sh                # Build + package as .dmg
```

---

## Session State Machine

```mermaid
stateDiagram-v2
    [*] --> Created: User creates session
    Created --> Booting: Start VM
    Booting --> Ready: VM booted
    Ready --> Running: Send prompt
    Running --> Ready: Prompt complete
    Running --> Paused: User pauses
    Paused --> Running: User resumes
    Ready --> Checkpointed: Checkpoint
    Checkpointed --> Ready: Restore
    Checkpointed --> Forked: Fork
    Forked --> Ready: New session
    Ready --> Reviewing: Changes detected
    Reviewing --> Ready: Accept/reject
    Running --> Error: Crash/timeout
    Error --> Ready: Retry
    Ready --> [*]: Destroy
```

---

## Core Features by Phase

### Phase 1 -- Scaffolding (the starting point)
- Rust workspace with all crates stubbed out
- GPUI window with sidebar + main content area
- Basic theme (dark mode)
- SQLite setup with initial schema
- Build script that produces a `.app` bundle

### Phase 2 -- Terminal + Sidecar
- GPU-accelerated terminal emulator in GPUI (adapt patterns from SuperHQ's `gpui-terminal` crate)
- Node.js sidecar management: auto-install `rivetkit` deps, spawn/monitor the Rivet server process
- Rust HTTP client for Rivet AgentOS API (using `reqwest` + `tokio-tungstenite` for WebSocket)

### Phase 3 -- Agent Sessions

Tasks in this phase are split so multiple can run in parallel. Each task lists its crate, deps, and whether it blocks others.

| ID | Task | Crate(s) | Depends on | Parallel group |
|---|---|---|---|---|
| 3a | Session state machine (Created→Booting→Ready→Running→Paused→Error) as pure logic with unit tests | `cuartel-core` | — | A |
| 3b | Rivet session API wrappers: `createSession`, `sendPrompt`, `destroySession` | `cuartel-rivet` | — | A |
| 3c | Rivet event stream client: WS/SSE subscription, typed `SessionEvent` enum | `cuartel-rivet` | — | A |
| 3d | Sidebar session list view with status indicators (static model, fixture data) | `cuartel-app` | — | A |
| 3e | Agent harness registry trait + Pi implementation | `cuartel-core` | 3a | B |
| 3f | Wire event stream into terminal output (end-to-end Pi session) | `cuartel-app`, `cuartel-terminal` | 3b, 3c, 3e | C |
| 3g | Permission prompt UI (approve/deny tool use) | `cuartel-app` | 3c | B |
| 3h | Add Claude Code / Codex / OpenCode harness implementations | `cuartel-core` | 3e | D |

**Group A** tasks (3a–3d) can all start in parallel today. Group B starts once 3a/3c land. Group C is the integration milestone. Group D is additive once 3e defines the harness trait.

### Phase 4 -- Workspaces + Review

| ID | Task | Crate(s) | Depends on | Parallel group |
|---|---|---|---|---|
| 4a | Workspace model + project directory mapping (DB-backed) | `cuartel-core`, `cuartel-db` | — | A |
| 4b | Overlay FS diff computation using `similar` (pure function: base tree + overlay tree → unified diff) | `cuartel-core` | — | A |
| 4c | Diff review panel UI built against fixture diffs | `cuartel-app` | — | A |
| 4d | Rivet file read/write/list wrappers for overlay snapshotting | `cuartel-rivet` | — | A |
| 4e | Mount project at `/workspace` inside VM via Rivet filesystem API | `cuartel-core` | 4a, 4d | B |
| 4f | Accept/reject per-file and per-hunk application to host FS | `cuartel-core`, `cuartel-app` | 4b, 4c, 4e | C |
| 4g | Multiple tabs per workspace (multi-agent same project) | `cuartel-app` | 4a, 3f | C |

**Group A** (4a–4d) is fully parallel. 4c in particular can ship without any VM — just render fixture diffs.

### Phase 5 -- Security + Ports

Split cleanly into three independent tracks; any two can be built in parallel by different agents.

| ID | Task | Crate(s) | Depends on | Track |
|---|---|---|---|---|
| 5a | Encrypted credential storage: AES-256-GCM wrapper + `credentials` table + CRUD | `cuartel-db`, `cuartel-core` | — | Storage |
| 5b | Settings UI for managing API keys / OAuth tokens | `cuartel-app` | 5a | Storage |
| 5c | Auth gateway reverse proxy: intercept outgoing VM requests, inject credentials by hostname rule | `cuartel-core` (new `auth_gateway.rs`) | 5a | Gateway |
| 5d | Audit log of credential-injected requests | `cuartel-core`, `cuartel-db` | 5c | Gateway |
| 5e | Port forwarding: sandbox→host and host→sandbox, opt-in per port | `cuartel-rivet`, `cuartel-app` | — | Ports |
| 5f | Firewall rules ensuring VMs cannot reach credential storage | `cuartel-core` | 5c | Gateway |

**Storage**, **Gateway**, and **Ports** are independent tracks. 5a and 5e can start the same day.

### Phase 6 -- Checkpoint + Rewind

| ID | Task | Crate(s) | Depends on | Parallel group |
|---|---|---|---|---|
| 6a | Rivet checkpoint API client (create, list, restore, delete) | `cuartel-rivet` | — | A |
| 6b | Checkpoint metadata table + core API | `cuartel-core`, `cuartel-db` | — | A |
| 6c | Timeline UI rendering checkpoint history | `cuartel-app` | 6b | B |
| 6d | Fork-from-checkpoint flow (spawns new session branch) | `cuartel-core` | 6a, 6b, 3a | B |

### Phase 7 -- Remote via Tailscale

| ID | Task | Crate(s) | Depends on | Parallel group |
|---|---|---|---|---|
| 7a | Tailscale discovery: list tailnet peers, reachability check | `cuartel-remote` | — | A |
| 7b | Server registry table + CRUD (local + remote entries) | `cuartel-db`, `cuartel-remote` | — | A |
| 7c | Server list UI in sidebar | `cuartel-app` | 7b | B |
| 7d | Point rivet client at configurable base URL (local vs remote) | `cuartel-rivet` | — | A |
| 7e | Session sync: push/pull session state between servers | `cuartel-remote` | 7a, 7b, 7d, 3b | C |

### Phase 8 -- Orchestration

| ID | Task | Crate(s) | Depends on | Parallel group |
|---|---|---|---|---|
| 8a | Multi-agent pipeline DAG (coder → reviewer → tester) | `cuartel-core` | 3h | A |
| 8b | Cron scheduler for agents | `cuartel-core` | 3a | A |
| 8c | Durable workflow wrapper over Rivet's workflow engine | `cuartel-rivet`, `cuartel-core` | — | A |
| 8d | Agent-to-agent file passing protocol | `cuartel-core` | 4e | B |

---

## Parallelism Quick Reference

At any given moment, these tasks have no shared files and can be built in separate worktrees:

- **Right now:** 3a, 3b, 3c, 3d, 4a, 4b, 4c, 4d, 5a, 5e, 6a, 6b, 7a, 7b, 7d
- **Bottleneck tasks** (many others wait on them): 3a (state machine), 3e (harness trait), 5a (credential storage), 3f (first end-to-end Pi integration)
- **Integration-only tasks** (must be done serially by a single agent): 3f, 4f, 5d, 7e

---

## Key Dependencies

| Crate | Purpose |
|---|---|
| `gpui` (unofficial) | GPU-accelerated UI framework via Metal |
| `reqwest` | HTTP client for Rivet API |
| `tokio-tungstenite` | WebSocket client for real-time event streaming |
| `rusqlite` | SQLite for local persistence |
| `ring` or `aes-gcm` | AES-256-GCM encryption for secrets |
| `tailscale-api` | Tailscale network discovery and management |
| `similar` | Diff computation for review panel |
| `alacritty_terminal` | Terminal emulation (VT100/ANSI parsing) |
| `serde` / `serde_json` | Serialization for Rivet API protocol |
| `tokio` | Async runtime |
| `notify` | Filesystem watching for overlay changes |

---

## Rivet AgentOS Integration Detail

The Rust app does NOT embed Rivet (it's Node.js). Instead:

1. **Local**: On first launch, `cuartel` checks for Node.js, installs the `rivet/` sidecar deps (`npm install`), then spawns `npx tsx server.ts` as a managed child process. The Rust app connects to `http://localhost:6420`.

2. **Remote**: User configures a Hetzner/any server in settings. The server runs its own Rivet AgentOS instance. The Rust app connects to it via Tailscale at `http://100.x.y.z:6420`.

3. **API Surface** (Rust client wraps these):
   - `POST /vm/getOrCreate` -- create/get VM instance
   - `POST /vm/{id}/createSession` -- start agent session
   - `POST /vm/{id}/sendPrompt` -- send prompt to agent
   - `WS /vm/{id}/events` -- stream session events
   - `GET /vm/{id}/readFile` -- read file from VM
   - `POST /vm/{id}/writeFile` -- write file to VM
   - `POST /vm/{id}/exec` -- execute command in VM

---

## Security Model

```
+-------------------------------------------+
|  cuartel (Host)                           |
|  +--------------+  +----------------+     |
|  | Auth Gateway  |  | Encrypted DB   |     |
|  | (injects keys |  | (AES-256-GCM)  |     |
|  |  on-the-fly)  |  | API keys,      |     |
|  +------+-------+  | OAuth tokens   |     |
|         |          +----------------+     |
|         v                                 |
|  +--------------+                         |
|  | Rivet AgentOS|                         |
|  | (no secrets) |                         |
|  +------+-------+                         |
|         |                                 |
|  +------v-------+                         |
|  |  Agent VM    | <- no API keys here     |
|  |  (isolated)  | <- outgoing requests    |
|  |              |   go through gateway    |
|  +--------------+                         |
+-------------------------------------------+
```

- Credentials stored in encrypted SQLite, never passed to VMs
- Auth gateway intercepts outgoing API calls and injects credentials
- VMs have no network access to credential storage
- Audit log of all credential-injected requests

---

## What to Build First

Start with **Phase 1 + Phase 2** together: get a GPUI window with a working terminal and a running Rivet sidecar. This validates the entire stack end-to-end (Rust -> GPUI -> Metal rendering + Node.js sidecar -> Rivet AgentOS) before investing in features.
