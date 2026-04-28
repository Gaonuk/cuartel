//! Bridge between the Rivet event stream and the cuartel UI.
//!
//! Owns a background tokio driver (spawned on the runtime shared with
//! `SidecarHost`) that:
//!
//! 1. Waits for the sidecar to report `Ready`.
//! 2. Performs `get_or_create_actor` for the `vm` actor.
//! 3. Opens a WebSocket event subscription.
//! 4. Creates a Pi agent session via the `createSession` action.
//! 5. Enters a `tokio::select!` loop that forwards incoming Rivet events to
//!    the GPUI side over a `tokio::mpsc` channel and handles outgoing
//!    commands (`send_prompt`, permission decisions, shutdown).
//!
//! On the GPUI side, a long-running `cx.spawn` task polls the event channel
//! with `try_recv`, drains bursts of events, and dispatches them into the
//! shared `TerminalView` / `PermissionPrompt` entities. The local `Session`
//! state machine from `cuartel-core` is advanced in lockstep so the UI can
//! eventually reflect the lifecycle.

use crate::permission_prompt::{PendingPermission, PermissionPrompt};
use crate::sidecar_host::SidecarStatus;
use cuartel_core::agent::AgentType;
use cuartel_core::session::{Session, SessionEvent as CoreSessionEvent, SessionState};
use cuartel_rivet::client::{GetOrCreateRequest, RivetClient};
use cuartel_rivet::event_decode::{
    decode_bytes_envelope, extract_session_update_text, summarize_permission,
};
use cuartel_rivet::events::{
    EventStream, PermissionRequestPayload, RivetEvent, DEFAULT_CHANNELS,
};
use cuartel_terminal::TerminalView;
use gpui::*;
use parking_lot::Mutex;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::mpsc::{self, error::TryRecvError, UnboundedReceiver, UnboundedSender};

/// Three driver variants today; selected per-session via env vars
/// (and, in a follow-up commit, a per-session UI toggle):
///
///   - **`Rivet`** (default) — the existing AgentOS secure-exec V8
///     sandbox path via `cuartel-rivet`. Untouched while the new
///     paths shake out.
///   - **`Acp`** (`CUARTEL_USE_ACP=1`) — host-direct
///     `claude-code-acp` subprocess via `cuartel-acp::LocalSandbox`.
///     Same isolation tier as Zed/Polyscope/Paseo today; tool calls
///     surface as structured `SessionEvent`s the cuartel UI renders.
///   - **`NativeClaudeCli`** (`CUARTEL_NATIVE_CLAUDE=1`) — bare
///     `claude` CLI in a real PTY. Users see the full Claude Code TUI
///     (boxes, ANSI colors, slash menus) inside cuartel's terminal
///     view. No structured-event extraction; cuartel-terminal renders
///     the raw bytes. Equivalent to running `claude` in a regular
///     terminal, but in a cuartel tab.
///
/// Precedence when multiple are set: NativeClaudeCli > Acp > Rivet.
const ACP_TOGGLE_ENV: &str = "CUARTEL_USE_ACP";
const NATIVE_CLAUDE_TOGGLE_ENV: &str = "CUARTEL_NATIVE_CLAUDE";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentMode {
    Rivet,
    Acp,
    NativeClaudeCli,
}

impl AgentMode {
    /// Compact label for the tab-bar mode picker.
    pub fn short_label(self) -> &'static str {
        match self {
            AgentMode::Rivet => "Rivet",
            AgentMode::Acp => "ACP",
            AgentMode::NativeClaudeCli => "Native",
        }
    }

    pub const ALL: [AgentMode; 3] = [
        AgentMode::Rivet,
        AgentMode::Acp,
        AgentMode::NativeClaudeCli,
    ];

    /// Initial default sourced from env vars at process start. Used by
    /// `CuartelApp` to seed `next_agent_mode`; the per-session UI picker
    /// overrides on subsequent session creations.
    pub fn from_env() -> Self {
        agent_mode_from_env()
    }
}

fn agent_mode_from_env() -> AgentMode {
    if env_truthy(NATIVE_CLAUDE_TOGGLE_ENV) {
        AgentMode::NativeClaudeCli
    } else if env_truthy(ACP_TOGGLE_ENV) {
        AgentMode::Acp
    } else {
        AgentMode::Rivet
    }
}

fn env_truthy(var: &str) -> bool {
    std::env::var(var)
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Working directory for ACP sessions when the toggle is on. Override
/// with `CUARTEL_ACP_CWD=/path/to/repo`. Falls back to the cuartel-app
/// process's current dir. The full Workspace abstraction (with N
/// worktrees + access policy) lands in Phase C3.
///
/// Returns `Err(message)` when the resolved path doesn't exist as a
/// directory — the GPUI status line shows the message so the user can
/// fix their env var without digging through a misleading
/// `posix_spawn ENOENT against /path/to/node` further down the stack.
fn acp_session_cwd() -> std::result::Result<PathBuf, String> {
    let (path, source) = match std::env::var("CUARTEL_ACP_CWD") {
        Ok(p) => (PathBuf::from(p), "CUARTEL_ACP_CWD"),
        Err(_) => (
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            "process cwd",
        ),
    };
    if !path.is_dir() {
        return Err(format!(
            "ACP cwd `{}` (from {source}) does not exist as a directory. \
             Set CUARTEL_ACP_CWD to a real repo path or unset it to use \
             the cuartel-app process's current dir.",
            path.display(),
        ));
    }
    Ok(path)
}

#[derive(Clone, Debug)]
pub struct SessionStateChange {
    pub session_id: String,
    pub state: SessionState,
}

impl EventEmitter<SessionStateChange> for SessionHost {}

const VM_ACTOR_NAME: &str = "vm";
const SERVER_ID: &str = "local";

#[derive(Clone, Debug)]
pub struct SessionHostConfig {
    pub session_id: String,
    /// Wire-format agent name passed to Rivet's `createSession` (e.g.
    /// `"claude"`, `"pi"`). Comes from [`AgentType::rivet_name`].
    pub agent_type: String,
    /// Typed agent identity. Used by the Native PTY mode to pick which
    /// CLI binary to spawn (claude / codex / droid / amp / gemini …).
    /// Defaults to [`AgentType::Custom`] of `agent_type` for legacy
    /// callers that only fill the string field.
    pub agent: AgentType,
    pub actor_key: String,
    pub workspace_id: String,
    /// Which driver backs this session. `None` falls back to the env-var
    /// resolution from [`AgentMode::from_env`] for callers that haven't
    /// been migrated to the per-session picker yet.
    pub agent_mode: Option<AgentMode>,
}

/// Events forwarded from the tokio driver into the GPUI thread.
#[derive(Debug)]
enum SessionHostEvent {
    /// Dim status line ("waiting for sidecar", "creating session...").
    Status(String),
    /// Raw bytes to feed the terminal grid (decoded ProcessOutput).
    Bytes(Vec<u8>),
    /// UTF-8 text to append, CRLF-normalized.
    Text(String),
    /// Red error line.
    Error(String),
    /// A tool-use permission request to queue in the UI.
    Permission(PendingPermission),
    /// A session state machine transition.
    StateEvent(CoreSessionEvent),
    /// Session / stream ended.
    Closed(String),
}

/// Commands sent from the GPUI side into the tokio driver.
#[derive(Debug)]
pub enum SessionHostCommand {
    #[allow(dead_code)]
    SendPrompt(String),
    Decision {
        id: String,
        approve: bool,
    },
    #[allow(dead_code)]
    Shutdown,
}

pub struct SessionHost {
    config: SessionHostConfig,
    terminal: Entity<TerminalView>,
    permission_prompt: Entity<PermissionPrompt>,
    session: Session,
    #[allow(dead_code)]
    env: HashMap<String, String>,
    cmd_tx: UnboundedSender<SessionHostCommand>,
    _driver_task: Task<()>,
}

impl SessionHost {
    pub fn new(
        config: SessionHostConfig,
        runtime: Handle,
        client_slot: Arc<Mutex<Option<RivetClient>>>,
        sidecar_status: Arc<Mutex<SidecarStatus>>,
        terminal: Entity<TerminalView>,
        permission_prompt: Entity<PermissionPrompt>,
        env: HashMap<String, String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel::<SessionHostEvent>();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionHostCommand>();

        let driver_config = config.clone();
        let env_clone = env.clone();
        let mode = driver_config.agent_mode.unwrap_or_else(agent_mode_from_env);
        match mode {
            AgentMode::NativeClaudeCli => {
                log::info!(
                    "[session_host] native CLI mode — spawning {} ({}) in PTY",
                    driver_config.agent.display_name(),
                    driver_config.agent.rivet_name(),
                );
                spawn_native_cli_in_terminal(
                    &driver_config,
                    &terminal,
                    &event_tx,
                    cx,
                );
                // Cmd loop forwards prompt-input submissions to PTY stdin.
                runtime.spawn(run_driver_native_cli(
                    driver_config,
                    event_tx,
                    cmd_rx,
                ));
            }
            AgentMode::Acp => {
                log::info!(
                    "[session_host] {ACP_TOGGLE_ENV}=1 — spawning host-direct ACP driver"
                );
                runtime.spawn(run_driver_acp(driver_config, event_tx, cmd_rx));
            }
            AgentMode::Rivet => {
                runtime.spawn(run_driver(
                    driver_config,
                    client_slot,
                    sidecar_status,
                    event_tx,
                    cmd_rx,
                    env_clone,
                ));
            }
        }

        let poll_task = cx.spawn(async move |this, cx| {
            let mut event_rx = event_rx;
            loop {
                let mut batch: Vec<SessionHostEvent> = Vec::new();
                loop {
                    match event_rx.try_recv() {
                        Ok(ev) => batch.push(ev),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            if !batch.is_empty() {
                                let _ = this.update(cx, |host, cx| host.dispatch_batch(batch, cx));
                            }
                            return;
                        }
                    }
                }
                if !batch.is_empty() {
                    if this
                        .update(cx, |host, cx| host.dispatch_batch(batch, cx))
                        .is_err()
                    {
                        return;
                    }
                }
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
            }
        });

        let session = Session::new(
            config.session_id.clone(),
            config.workspace_id.clone(),
            SERVER_ID.into(),
            config.agent_type.clone(),
        );

        Self {
            config,
            terminal,
            permission_prompt,
            session,
            env,
            cmd_tx,
            _driver_task: poll_task,
        }
    }

    #[allow(dead_code)]
    pub fn config(&self) -> &SessionHostConfig {
        &self.config
    }

    pub fn decide(&self, id: String, approve: bool) {
        if self
            .cmd_tx
            .send(SessionHostCommand::Decision { id, approve })
            .is_err()
        {
            log::warn!("session host command channel closed");
        }
    }

    pub fn send_prompt(&self, text: String) {
        let _ = self.cmd_tx.send(SessionHostCommand::SendPrompt(text));
    }

    #[allow(dead_code)]
    pub fn state(&self) -> &SessionState {
        &self.session.state
    }

    fn dispatch_batch(&mut self, events: Vec<SessionHostEvent>, cx: &mut Context<Self>) {
        for ev in events {
            self.dispatch(ev, cx);
        }
    }

    fn dispatch(&mut self, event: SessionHostEvent, cx: &mut Context<Self>) {
        match event {
            SessionHostEvent::Status(s) => {
                log::info!("[session] {s}");
                self.terminal.update(cx, |t, cx| {
                    // ANSI dim grey for status lines.
                    t.push_text(&format!("\x1b[38;5;242m• {s}\x1b[0m\r\n"), cx);
                });
            }
            SessionHostEvent::Bytes(bytes) => {
                self.terminal.update(cx, |t, cx| t.push_bytes(&bytes, cx));
            }
            SessionHostEvent::Text(text) => {
                self.terminal.update(cx, |t, cx| t.push_text(&text, cx));
            }
            SessionHostEvent::Error(msg) => {
                log::warn!("[session] {msg}");
                self.terminal.update(cx, |t, cx| {
                    t.push_text(&format!("\x1b[38;5;203m✗ {msg}\x1b[0m\r\n"), cx);
                });
            }
            SessionHostEvent::Permission(p) => {
                self.permission_prompt
                    .update(cx, |pp, cx| pp.enqueue(p, cx));
            }
            SessionHostEvent::StateEvent(core_ev) => {
                match self.session.apply(core_ev.clone()) {
                    Ok(state) => {
                        log::info!("[session:{}] state → {state}", self.config.session_id);
                        cx.emit(SessionStateChange {
                            session_id: self.config.session_id.clone(),
                            state: state.clone(),
                        });
                    }
                    Err(e) => {
                        log::debug!("[session] rejected transition {core_ev:?}: {e}");
                    }
                }
            }
            SessionHostEvent::Closed(reason) => {
                log::warn!("[session] closed: {reason}");
                self.terminal.update(cx, |t, cx| {
                    t.push_text(&format!("\x1b[38;5;203m[closed] {reason}\x1b[0m\r\n"), cx);
                });
                let _ = self.session.apply(CoreSessionEvent::Destroy);
            }
        }
    }
}

// --- Tokio driver --------------------------------------------------------

async fn run_driver(
    config: SessionHostConfig,
    client_slot: Arc<Mutex<Option<RivetClient>>>,
    sidecar_status: Arc<Mutex<SidecarStatus>>,
    event_tx: UnboundedSender<SessionHostEvent>,
    mut cmd_rx: UnboundedReceiver<SessionHostCommand>,
    env: HashMap<String, String>,
) {
    let _ = event_tx.send(SessionHostEvent::Status("waiting for rivet sidecar...".into()));

    let client = match wait_for_ready(&sidecar_status, &client_slot).await {
        Some(c) => c,
        None => {
            let _ = event_tx.send(SessionHostEvent::Error(
                "sidecar failed to start".into(),
            ));
            return;
        }
    };

    let _ = event_tx.send(SessionHostEvent::Status(
        format!("provisioning vm actor (vm/{})...", config.actor_key),
    ));
    let req = GetOrCreateRequest {
        name: VM_ACTOR_NAME,
        key: &config.actor_key,
        runner_name_selector: "default",
        crash_policy: "kill",
    };
    let actor_id = match client.get_or_create_actor(&req).await {
        Ok(r) => {
            let _ = event_tx.send(SessionHostEvent::Status(format!(
                "actor ready: id={} created={}",
                r.actor.actor_id, r.created
            )));
            let _ = event_tx.send(SessionHostEvent::StateEvent(CoreSessionEvent::Boot));
            r.actor.actor_id
        }
        Err(e) => {
            let _ = event_tx.send(SessionHostEvent::Error(format!(
                "PUT /actors failed: {e}"
            )));
            return;
        }
    };

    let _ = event_tx.send(SessionHostEvent::Status(
        "subscribing to event stream...".into(),
    ));
    let mut stream: EventStream = match client
        .subscribe_events(&actor_id, DEFAULT_CHANNELS)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            let _ = event_tx.send(SessionHostEvent::Error(format!(
                "subscribe failed: {e}"
            )));
            return;
        }
    };

    let _ = event_tx.send(SessionHostEvent::Status(
        format!("creating {} session...", config.agent_type),
    ));
    let session_options = if env.is_empty() {
        None
    } else {
        log::info!("[session] createSession env: {:?}", env);
        Some(json!({ "env": env }))
    };
    let session_rec = match client
        .create_session(&actor_id, &config.agent_type, session_options)
        .await
    {
        Ok(r) => {
            let _ = event_tx.send(SessionHostEvent::Status(format!(
                "session ready: {}",
                r.session_id
            )));
            let _ = event_tx.send(SessionHostEvent::StateEvent(
                CoreSessionEvent::BootCompleted,
            ));
            r
        }
        Err(e) => {
            let _ = event_tx.send(SessionHostEvent::Error(format!(
                "createSession failed: {e}"
            )));
            return;
        }
    };

    let session_id = session_rec.session_id;

    loop {
        tokio::select! {
            maybe_event = stream.recv() => {
                match maybe_event {
                    Some(ev) => {
                        for out in translate_event(ev, &session_id) {
                            let _ = event_tx.send(out);
                        }
                    }
                    None => {
                        let _ = event_tx.send(SessionHostEvent::Closed(
                            "event stream closed".into(),
                        ));
                        break;
                    }
                }
            }
            maybe_cmd = cmd_rx.recv() => {
                match maybe_cmd {
                    Some(SessionHostCommand::SendPrompt(text)) => {
                        let _ = event_tx.send(SessionHostEvent::Status(
                            format!("> {text}"),
                        ));
                        let _ = event_tx.send(SessionHostEvent::StateEvent(
                            CoreSessionEvent::PromptSent,
                        ));
                        match client.send_prompt(&actor_id, &session_id, &text).await {
                            Ok(r) => {
                                if !r.text.is_empty() {
                                    let _ = event_tx.send(SessionHostEvent::Text(
                                        format!("{}\n", r.text),
                                    ));
                                }
                                let _ = event_tx.send(SessionHostEvent::StateEvent(
                                    CoreSessionEvent::PromptCompleted,
                                ));
                            }
                            Err(e) => {
                                let _ = event_tx.send(SessionHostEvent::Error(
                                    format!("sendPrompt failed: {e}"),
                                ));
                                let _ = event_tx.send(SessionHostEvent::StateEvent(
                                    CoreSessionEvent::Failed(e.to_string()),
                                ));
                            }
                        }
                    }
                    Some(SessionHostCommand::Decision { id, approve }) => {
                        // TODO phase 3 polish: send JSON-RPC reply back to Pi.
                        // agent-os core expects `request/permission` response
                        // frames on the actor WebSocket, which needs a
                        // bi-directional client. For 3f we just log the
                        // decision so the UI flow is exercisable.
                        log::info!(
                            "[session] TODO wire permission reply: id={id} approve={approve}",
                        );
                        let _ = event_tx.send(SessionHostEvent::Status(format!(
                            "permission {}: {}",
                            if approve { "approved" } else { "denied" },
                            id,
                        )));
                    }
                    Some(SessionHostCommand::Shutdown) => {
                        let _ = client.destroy_session(&actor_id, &session_id).await;
                        let _ = event_tx.send(SessionHostEvent::Closed(
                            "shutdown requested".into(),
                        ));
                        break;
                    }
                    None => break,
                }
            }
        }
    }
}

async fn wait_for_ready(
    sidecar_status: &Arc<Mutex<SidecarStatus>>,
    client_slot: &Arc<Mutex<Option<RivetClient>>>,
) -> Option<RivetClient> {
    loop {
        let status = sidecar_status.lock().clone();
        match status {
            SidecarStatus::Ready => {
                if let Some(c) = client_slot.lock().clone() {
                    return Some(c);
                }
            }
            SidecarStatus::Failed(_) => return None,
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// --- Event translation ---------------------------------------------------

fn translate_event(ev: RivetEvent, current_session: &str) -> Vec<SessionHostEvent> {
    match ev {
        RivetEvent::VmBooted => vec![
            SessionHostEvent::Status("vm booted".into()),
        ],
        RivetEvent::VmShutdown(p) => vec![
            SessionHostEvent::Status(format!("vm shutdown: {}", p.reason)),
            SessionHostEvent::Closed(format!("vm shutdown ({})", p.reason)),
        ],
        RivetEvent::ProcessOutput(value) => match decode_bytes_envelope(&value) {
            Some(bytes) => vec![SessionHostEvent::Bytes(bytes)],
            None => vec![SessionHostEvent::Text(format!(
                "[processOutput] {value}\n",
            ))],
        },
        RivetEvent::ProcessExit(p) => vec![SessionHostEvent::Status(format!(
            "process {} exited (code {})",
            p.pid, p.exit_code
        ))],
        RivetEvent::ShellData(v) => match decode_bytes_envelope(&v) {
            Some(bytes) => vec![SessionHostEvent::Bytes(bytes)],
            None => vec![],
        },
        RivetEvent::SessionEvent(p) => {
            if p.session_id != current_session {
                return vec![];
            }
            let text = extract_session_update_text(&p.event.method, &p.event.params);
            vec![SessionHostEvent::Text(text)]
        }
        RivetEvent::PermissionRequest(p) => {
            if p.session_id != current_session {
                return vec![];
            }
            match build_pending_permission(&p) {
                Some(pending) => vec![SessionHostEvent::Permission(pending)],
                None => vec![SessionHostEvent::Status(
                    "received malformed permission request".into(),
                )],
            }
        }
        RivetEvent::CronEvent(_) => vec![],
        RivetEvent::Other { name, args: _ } => vec![SessionHostEvent::Status(format!(
            "broadcast: {name}"
        ))],
        RivetEvent::Error {
            group,
            code,
            message,
        } => vec![SessionHostEvent::Error(format!(
            "{group}.{code}: {message}"
        ))],
    }
}


fn build_pending_permission(p: &PermissionRequestPayload) -> Option<PendingPermission> {
    let mut summary = summarize_permission(&p.request);
    if summary.id.is_empty() {
        summary.id = format!("perm-{}", uuid::Uuid::new_v4());
    }
    let session_label = SharedString::from(format!(
        "session {}",
        &p.session_id[..8.min(p.session_id.len())]
    ));
    Some(PendingPermission::new(
        summary.id,
        &p.session_id,
        session_label,
        summary.tool_name,
        summary.input,
    ))
}

// ============================================================================
// Host-direct ACP driver (Phase B2 of v2 doc)
//
// Sibling to `run_driver` above. Activated by `CUARTEL_USE_ACP=1`.
// Spawns claude-code-acp via cuartel-acp::LocalSandbox, runs prompts
// through it, forwards events to the same SessionHostEvent channel
// the existing dispatcher consumes — so the rest of the UI is
// unchanged. The Rivet path stays the default until this is shaken
// out; eventually we delete the Rivet path entirely.
// ============================================================================

async fn run_driver_acp(
    config: SessionHostConfig,
    event_tx: UnboundedSender<SessionHostEvent>,
    mut cmd_rx: UnboundedReceiver<SessionHostCommand>,
) {
    let cwd = match acp_session_cwd() {
        Ok(p) => p,
        Err(msg) => {
            let _ = event_tx.send(SessionHostEvent::Error(msg));
            return;
        }
    };

    // The session state machine starts at SessionState::Created and
    // only reaches Ready via Created → Boot → Booting → BootCompleted
    // → Ready. workspace.rs gates the prompt input on Ready, so emit
    // the full sequence (not just BootCompleted) at the appropriate
    // moments so the input unlocks.
    let _ = event_tx.send(SessionHostEvent::StateEvent(CoreSessionEvent::Boot));
    let _ = event_tx.send(SessionHostEvent::Status(format!(
        "ACP path: spawning claude-code-acp in {}",
        cwd.display()
    )));

    let client = match cuartel_acp::spawn_local_with_default_handler(cwd.clone()).await {
        Ok(c) => c,
        Err(e) => {
            let _ = event_tx.send(SessionHostEvent::StateEvent(
                CoreSessionEvent::Failed(format!("ACP spawn failed: {e}")),
            ));
            let _ = event_tx.send(SessionHostEvent::Error(format!(
                "claude-code-acp spawn failed: {e}"
            )));
            return;
        }
    };
    let _ = event_tx.send(SessionHostEvent::Status(format!(
        "ACP server up; loadSession={}",
        client.capabilities().load_session,
    )));

    let session = match client.new_session(cwd).await {
        Ok(s) => s,
        Err(e) => {
            let _ = event_tx.send(SessionHostEvent::StateEvent(
                CoreSessionEvent::Failed(format!("new_session failed: {e}")),
            ));
            let _ = event_tx.send(SessionHostEvent::Error(format!(
                "new_session failed: {e}"
            )));
            return;
        }
    };
    // Booting → Ready. Now the prompt input unlocks.
    let _ = event_tx.send(SessionHostEvent::StateEvent(CoreSessionEvent::BootCompleted));
    let _ = event_tx.send(SessionHostEvent::Status(format!(
        "ACP session ready ({})",
        session.id
    )));

    loop {
        match cmd_rx.recv().await {
            Some(SessionHostCommand::SendPrompt(text)) => {
                let _ = event_tx.send(SessionHostEvent::StateEvent(CoreSessionEvent::PromptSent));
                let mut events = match client.prompt(&session, text).await {
                    Ok(rx) => rx,
                    Err(e) => {
                        let _ = event_tx.send(SessionHostEvent::Error(format!(
                            "prompt request failed: {e}"
                        )));
                        continue;
                    }
                };
                while let Some(ev) = events.recv().await {
                    if !translate_acp_event(ev, &event_tx) {
                        break;
                    }
                }
                let _ = event_tx
                    .send(SessionHostEvent::StateEvent(CoreSessionEvent::PromptCompleted));
            }
            Some(SessionHostCommand::Decision { .. }) => {
                // The MVP NoOpClientHandler auto-approves all permission
                // requests inside the ACP server, so user decisions
                // never reach this driver. Real workspace-policy
                // mediated handler lands in Phase C2; at that point
                // these decisions get forwarded back to the handler
                // via a oneshot/channel. For now, ignore.
                log::debug!("[session_host_acp] decision received but no pending request");
            }
            Some(SessionHostCommand::Shutdown) => {
                let _ = event_tx.send(SessionHostEvent::Closed("shutdown requested".into()));
                break;
            }
            None => {
                // Command channel closed — UI side dropped. Bail.
                break;
            }
        }
    }

    // Drop client; its Drop impl signals the ACP bg task to exit and
    // claude-code-acp gets reaped via tokio::process::Command::kill_on_drop.
    drop(client);
    let _ = config; // suppress unused warning; kept for symmetry with run_driver
}

/// Translate one cuartel-acp SessionEvent into one or more
/// SessionHostEvents for the existing GPUI dispatcher. Returns `false`
/// when the stream should stop (TurnComplete or Error).
fn translate_acp_event(
    ev: cuartel_acp::SessionEvent,
    event_tx: &UnboundedSender<SessionHostEvent>,
) -> bool {
    use cuartel_acp::SessionEvent as SE;
    match ev {
        SE::UserPrompt { .. } => true,
        SE::AgentMessageChunk { text } | SE::AgentThoughtChunk { text } => {
            let _ = event_tx.send(SessionHostEvent::Text(text));
            true
        }
        SE::ToolCall { kind, raw_name, .. } => {
            let _ = event_tx.send(SessionHostEvent::Text(format!(
                "\n[tool: {} ({})]\n",
                raw_name,
                kind.as_str()
            )));
            true
        }
        SE::ToolCallResult { is_error, .. } => {
            if is_error {
                let _ = event_tx.send(SessionHostEvent::Text("[tool: error]\n".into()));
            }
            true
        }
        SE::PermissionRequested { .. } | SE::PermissionResolved { .. } => true,
        SE::TurnComplete { stop_reason } => {
            let _ = event_tx.send(SessionHostEvent::Text(format!(
                "\n[turn complete: {stop_reason}]\n"
            )));
            false
        }
        SE::Error { message } => {
            let _ = event_tx.send(SessionHostEvent::Error(message));
            false
        }
        // SessionEvent is #[non_exhaustive] — keep going on unknown variants.
        _ => true,
    }
}

// ============================================================================
// Native CLI driver (CUARTEL_NATIVE_CLAUDE=1, plus per-session selection).
//
// Spawns the user's chosen agent CLI — `claude`, `codex`, `pi`,
// `opencode`, `droid`, `amp`, or `gemini` — in a real PTY so the user
// sees its full TUI inside cuartel's terminal pane. No structured event
// extraction; equivalent to running the CLI in a regular terminal, but
// in a cuartel tab.
//
// Architecture:
//   1. spawn_native_cli_in_terminal (sync, GPUI side) resolves the CLI
//      binary, wraps it in the user's $SHELL so rc files (.zshrc /
//      .bashrc) load and the terminal looks like the user's normal
//      shell, then creates a PtySession via PtySession::spawn_command
//      and hands it to TerminalView::attach_pty.
//   2. run_driver_native_cli (tokio side) handles SessionHostCommand
//      flow: forwards SendPrompt(text) from the prompt input to PTY
//      stdin via a shared Arc<PtySession>.
//
// Shell wrapping (set CUARTEL_NATIVE_NO_SHELL=1 to disable): the CLI is
// invoked as `$SHELL -i -c "exec <cli>"` so the user's interactive
// shell sources their rc files first. This means PATH, aliases, prompt
// theming, and any CLI-side shell completions all behave exactly as
// they do when the user runs the CLI in their own terminal.
// ============================================================================

use cuartel_terminal::PtySession;

/// Shared between the GPUI-side spawn helper and the tokio-side
/// command-forwarding driver. Set once at session creation;
/// SendPrompt commands look it up to write to PTY stdin.
static NATIVE_CLI_PTYS: std::sync::OnceLock<
    parking_lot::Mutex<std::collections::HashMap<String, std::sync::Arc<PtySession>>>,
> = std::sync::OnceLock::new();

fn native_cli_ptys()
    -> &'static parking_lot::Mutex<std::collections::HashMap<String, std::sync::Arc<PtySession>>>
{
    NATIVE_CLI_PTYS.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()))
}

/// Per-CLI metadata for binary discovery + install hints. Kept local to
/// cuartel-app — the cuartel-core harness layer is heavier (parsers,
/// install steps, env keys) and not all native CLIs have a harness yet.
struct NativeCliSpec {
    /// Binary name to look up on PATH (`"claude"`, `"codex"`, …).
    binary: &'static str,
    /// Optional env var that, if set to a real file path, overrides the
    /// PATH search. Lets users point cuartel at a non-standard install.
    override_env: &'static str,
    /// Extra `~`-relative paths to probe after PATH (homebrew, npm
    /// per-user installs, vendor-specific install dirs).
    home_fallbacks: &'static [&'static str],
    /// Human-readable install hint shown in the error when the binary
    /// can't be found. Usually a copy-paste install command.
    install_hint: &'static str,
}

fn native_cli_spec(agent: &AgentType) -> NativeCliSpec {
    match agent {
        AgentType::ClaudeCode => NativeCliSpec {
            binary: "claude",
            override_env: "CUARTEL_CLAUDE_PATH",
            home_fallbacks: &[
                ".local/bin/claude",
                ".claude/local/bin/claude",
                ".claude/bin/claude",
            ],
            install_hint: "npm install -g @anthropic-ai/claude-code",
        },
        AgentType::Codex => NativeCliSpec {
            binary: "codex",
            override_env: "CUARTEL_CODEX_PATH",
            home_fallbacks: &[".local/bin/codex"],
            install_hint: "npm install -g @openai/codex",
        },
        AgentType::Pi => NativeCliSpec {
            binary: "pi",
            override_env: "CUARTEL_PI_PATH",
            home_fallbacks: &[".local/bin/pi", ".pi/bin/pi"],
            install_hint: "curl -fsSL https://pi.cuartel.dev/install.sh | sh",
        },
        AgentType::OpenCode => NativeCliSpec {
            binary: "opencode",
            override_env: "CUARTEL_OPENCODE_PATH",
            home_fallbacks: &[".local/bin/opencode", ".opencode/bin/opencode"],
            install_hint: "curl -fsSL https://opencode.ai/install | bash",
        },
        AgentType::Droid => NativeCliSpec {
            binary: "droid",
            override_env: "CUARTEL_DROID_PATH",
            home_fallbacks: &[".local/bin/droid", ".factory/bin/droid"],
            install_hint: "curl -fsSL https://app.factory.ai/cli | sh",
        },
        AgentType::Amp => NativeCliSpec {
            binary: "amp",
            override_env: "CUARTEL_AMP_PATH",
            home_fallbacks: &[".local/bin/amp"],
            install_hint: "npm install -g @sourcegraph/amp",
        },
        AgentType::Gemini => NativeCliSpec {
            binary: "gemini",
            override_env: "CUARTEL_GEMINI_PATH",
            home_fallbacks: &[".local/bin/gemini"],
            install_hint: "npm install -g @google/gemini-cli",
        },
        AgentType::Custom(name) => {
            // Best-effort: no env override, no fallbacks, hint asks the
            // user to put the binary on PATH. Custom is mainly for
            // tests / future plugins.
            let leaked: &'static str = Box::leak(name.clone().into_boxed_str());
            NativeCliSpec {
                binary: leaked,
                override_env: "",
                home_fallbacks: &[],
                install_hint: "(custom agent — ensure binary is on PATH)",
            }
        }
    }
}

/// Resolve the absolute path to a CLI binary by name. Search order:
/// override env var → PATH → user-home fallbacks → system fallbacks.
fn resolve_native_cli_binary(spec: &NativeCliSpec) -> Option<PathBuf> {
    if !spec.override_env.is_empty() {
        if let Ok(p) = std::env::var(spec.override_env) {
            let path = PathBuf::from(p);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(spec.binary);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(h) = home.as_ref() {
        for rel in spec.home_fallbacks {
            candidates.push(h.join(rel));
        }
    }
    // Standard macOS install prefixes for Homebrew + manually installed
    // binaries. Cheap to check and covers most setups when PATH was
    // sanitized by Finder-launched cuartel-app.
    candidates.push(PathBuf::from(format!("/opt/homebrew/bin/{}", spec.binary)));
    candidates.push(PathBuf::from(format!("/usr/local/bin/{}", spec.binary)));
    candidates.into_iter().find(|p| p.is_file())
}

/// Build the (program, args) pair to spawn for a given CLI binary.
///
/// By default wraps in `$SHELL -i -c "exec <cli>"` so the user's
/// interactive shell sources rc files first. Set `CUARTEL_NATIVE_NO_SHELL=1`
/// to bypass and spawn the CLI directly (debug / minimal-env mode).
fn build_native_cli_spawn(cli_path: &std::path::Path) -> (PathBuf, Vec<String>) {
    if env_truthy("CUARTEL_NATIVE_NO_SHELL") {
        return (cli_path.to_path_buf(), Vec::new());
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    // `exec` replaces the shell with the CLI process so closing the
    // CLI also closes the PTY. Single-quoting the path is safe: real
    // CLI paths never contain `'` and we already verified it `is_file`.
    let inner = format!("exec '{}'", cli_path.display());
    (
        PathBuf::from(shell),
        vec!["-i".to_string(), "-c".to_string(), inner],
    )
}

fn spawn_native_cli_in_terminal(
    config: &SessionHostConfig,
    terminal: &Entity<TerminalView>,
    event_tx: &UnboundedSender<SessionHostEvent>,
    cx: &mut Context<SessionHost>,
) {
    let cwd = match acp_session_cwd() {
        Ok(p) => p,
        Err(msg) => {
            let _ = event_tx.send(SessionHostEvent::Error(msg));
            return;
        }
    };

    let spec = native_cli_spec(&config.agent);
    let cli_path = match resolve_native_cli_binary(&spec) {
        Some(p) => p,
        None => {
            let _ = event_tx.send(SessionHostEvent::Error(format!(
                "{} CLI not found. Install with `{}` or set {}=/absolute/path/to/{}.",
                config.agent.display_name(),
                spec.install_hint,
                spec.override_env,
                spec.binary,
            )));
            return;
        }
    };

    let (program, args) = build_native_cli_spawn(&cli_path);

    let _ = event_tx.send(SessionHostEvent::StateEvent(CoreSessionEvent::Boot));
    let _ = event_tx.send(SessionHostEvent::Status(format!(
        "native {}: spawning `{}` in {} (via {})",
        config.agent.display_name(),
        cli_path.display(),
        cwd.display(),
        program.display(),
    )));

    let session_arc = match PtySession::spawn_command(
        &program,
        &args,
        &cwd,
        &std::collections::HashMap::new(),
        40, // rows — TerminalView resizes the PTY on layout
        120,
    ) {
        Ok(s) => std::sync::Arc::new(s),
        Err(e) => {
            let _ = event_tx.send(SessionHostEvent::StateEvent(
                CoreSessionEvent::Failed(format!("PTY spawn failed: {e}")),
            ));
            let _ = event_tx.send(SessionHostEvent::Error(format!(
                "failed to spawn {} in PTY: {e}",
                spec.binary,
            )));
            return;
        }
    };

    native_cli_ptys()
        .lock()
        .insert(config.session_id.clone(), session_arc.clone());

    terminal.update(cx, |t, cx| t.attach_pty(session_arc, cx));

    let _ = event_tx.send(SessionHostEvent::StateEvent(CoreSessionEvent::BootCompleted));
    let _ = event_tx.send(SessionHostEvent::Status(format!(
        "native {} session ready (type into the terminal or use the prompt input)",
        config.agent.display_name(),
    )));
}

async fn run_driver_native_cli(
    config: SessionHostConfig,
    event_tx: UnboundedSender<SessionHostEvent>,
    mut cmd_rx: UnboundedReceiver<SessionHostCommand>,
) {
    loop {
        match cmd_rx.recv().await {
            Some(SessionHostCommand::SendPrompt(text)) => {
                // Forward prompt-input box submissions to PTY stdin so
                // both the in-terminal typing AND the input box at the
                // bottom of the workspace work in native mode.
                let pty = native_cli_ptys()
                    .lock()
                    .get(&config.session_id)
                    .cloned();
                match pty {
                    Some(p) => {
                        let mut payload = text.into_bytes();
                        payload.push(b'\n');
                        p.write(&payload);
                    }
                    None => {
                        let _ = event_tx.send(SessionHostEvent::Error(
                            "native CLI PTY missing — session lost".into(),
                        ));
                        break;
                    }
                }
            }
            Some(SessionHostCommand::Decision { .. }) => {
                // No structured permission flow in native mode — the
                // CLI's own TUI prompts the user.
            }
            Some(SessionHostCommand::Shutdown) => {
                native_cli_ptys().lock().remove(&config.session_id);
                let _ = event_tx.send(SessionHostEvent::Closed("shutdown requested".into()));
                break;
            }
            None => {
                native_cli_ptys().lock().remove(&config.session_id);
                break;
            }
        }
    }
}
