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
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::mpsc::{self, error::TryRecvError, UnboundedReceiver, UnboundedSender};

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
        runtime.spawn(run_driver(
            driver_config,
            client_slot,
            sidecar_status,
            event_tx,
            cmd_rx,
            env_clone,
        ));

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
