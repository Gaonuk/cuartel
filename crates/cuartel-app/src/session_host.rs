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

/// Feature flag for the host-direct ACP path (Phase B2 of v2 doc).
///
/// When `CUARTEL_USE_ACP=1` is set, [`SessionHost::new`] spawns
/// [`run_driver_acp`] instead of the Rivet-backed [`run_driver`]. The
/// ACP path runs `claude-code-acp` as a plain host subprocess via
/// `cuartel-acp::LocalSandbox` — no V8, no Rivet sidecar, no AgentOS
/// secure-exec. Same isolation tier as Zed/Polyscope/Paseo today
/// (host-direct + permission UI as the safety net).
///
/// The Rivet path stays the default until the new path is shaken out;
/// then we swap defaults and remove the Rivet path entirely.
const ACP_TOGGLE_ENV: &str = "CUARTEL_USE_ACP";

fn acp_path_enabled() -> bool {
    std::env::var(ACP_TOGGLE_ENV)
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
    pub agent_type: String,
    pub actor_key: String,
    pub workspace_id: String,
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
        if acp_path_enabled() {
            log::info!(
                "[session_host] {ACP_TOGGLE_ENV}=1 — spawning host-direct ACP driver"
            );
            runtime.spawn(run_driver_acp(driver_config, event_tx, cmd_rx));
        } else {
            runtime.spawn(run_driver(
                driver_config,
                client_slot,
                sidecar_status,
                event_tx,
                cmd_rx,
                env_clone,
            ));
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
