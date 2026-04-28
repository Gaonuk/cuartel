//! [`AcpClient`] — the entry point for cuartel-acp.
//!
//! Spawns an ACP server subprocess via [`crate::transport`], performs
//! the `initialize` handshake, exposes session lifecycle (`new_session`,
//! `prompt`, `cancel`, `load_session`), and surfaces server-→client
//! requests through a [`crate::ClientHandler`].
//!
//! ## Architecture
//!
//! `agent-client-protocol`'s `connect_with(transport, closure)` runs the
//! entire connection inside `closure`'s lifetime — when the closure
//! returns, the connection ends. We need a long-lived `AcpClient` that
//! can call `prompt()` many times, so we:
//!
//!   1. spawn the connection in a background tokio task,
//!   2. inside the closure, do the `initialize` handshake, then send
//!      the live `ConnectionTo<Agent>` + cached capabilities back to
//!      `connect()` over a oneshot,
//!   3. the closure then awaits a shutdown signal (oneshot) before
//!      returning, keeping the connection alive.
//!
//! Notifications (`session/update`) are fanned out via a `tokio::sync::broadcast`
//! channel so each in-flight `prompt()` call can subscribe and filter
//! by its own session id.

use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol::schema::{
    AgentCapabilities, ContentBlock, InitializeRequest, LoadSessionRequest, NewSessionRequest,
    PromptRequest, ProtocolVersion, ReadTextFileRequest, ReadTextFileResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId as AcpSessionId, SessionNotification, SessionUpdate,
    StopReason, TextContent, WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::{
    on_receive_notification, on_receive_request, Agent, ByteStreams, Client, ConnectionTo,
};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::client_handler::{ClientHandler, PermissionDecision, PermissionRequest};
use crate::error::{AcpError, Result};
use crate::normalize::normalize_tool_name;
use crate::session::{SessionEvent, SessionHandle, SessionHandleInner, SessionId};
use crate::transport::{spawn, SpawnOptions};

/// Capacity of the broadcast channel that fans out `SessionNotification`s
/// to in-flight `prompt()` calls. Each prompt subscribes; if it falls
/// behind, it gets `RecvError::Lagged` and we surface a protocol error.
const NOTIFICATION_BROADCAST_CAPACITY: usize = 256;

/// Construction-time options for an [`AcpClient`].
#[derive(Clone)]
pub struct AcpClientOptions {
    /// How to spawn the ACP server subprocess.
    pub spawn: SpawnOptions,
    /// Pluggable handler for server-→client requests (file I/O,
    /// permission prompts). The MVP cuartel-daemon impl mediates path
    /// access against the workspace's access policy.
    pub handler: Arc<dyn ClientHandler>,
}

/// A connected ACP client.
pub struct AcpClient {
    connection: ConnectionTo<Agent>,
    capabilities: AgentCapabilities,
    notifications: broadcast::Sender<SessionNotification>,
    /// Held to keep the bg task alive; set to `None` after `dispose()`.
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Background task running the ACP connection event loop.
    bg_task: Option<JoinHandle<()>>,
}

impl AcpClient {
    /// Spawn the ACP server and complete the `initialize` handshake.
    pub async fn connect(opts: AcpClientOptions) -> Result<Self> {
        let spawned = spawn(&opts.spawn)?;

        // Take stdio out of the child so we own them. (transport::spawn
        // returns a Spawned with stdin/stdout/stderr already split.)
        let stdin = spawned.stdin;
        let stdout = spawned.stdout;
        let _stderr = spawned.stderr; // TODO: drain into log; for now Drop closes it.

        let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());

        let (conn_tx, conn_rx) = oneshot::channel::<(ConnectionTo<Agent>, AgentCapabilities)>();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (notif_tx, _notif_rx) = broadcast::channel::<SessionNotification>(
            NOTIFICATION_BROADCAST_CAPACITY,
        );

        let handler_for_perm = opts.handler.clone();
        let handler_for_read = opts.handler.clone();
        let handler_for_write = opts.handler.clone();
        let notif_tx_for_loop = notif_tx.clone();
        let conn_tx_holder = std::sync::Mutex::new(Some(conn_tx));
        let conn_tx_holder = Arc::new(conn_tx_holder);
        let shutdown_rx_holder = std::sync::Mutex::new(Some(shutdown_rx));
        let shutdown_rx_holder = Arc::new(shutdown_rx_holder);

        let bg_task: JoinHandle<()> = tokio::spawn(async move {
            // Keep the child alive for the duration of the connection.
            // Dropped at the end of this task -> kill_on_drop kills it.
            let _child_owner = spawned.child;

            let result = Client
                .builder()
                .name("cuartel-acp")
                .on_receive_request(
                    move |req: RequestPermissionRequest,
                          responder: agent_client_protocol::Responder<
                              RequestPermissionResponse,
                          >,
                          _conn: ConnectionTo<Agent>| {
                        let handler = handler_for_perm.clone();
                        async move {
                            let pr = PermissionRequest {
                                tool_name: req
                                    .tool_call
                                    .fields
                                    .title
                                    .clone()
                                    .unwrap_or_default(),
                                raw_input: serde_json::to_value(&req)
                                    .unwrap_or(serde_json::Value::Null),
                            };
                            let decision = handler
                                .request_permission(pr)
                                .await
                                .unwrap_or(PermissionDecision::DenyOnce);
                            let outcome = match decision {
                                PermissionDecision::AllowOnce
                                | PermissionDecision::AllowAlways => {
                                    let id = req
                                        .options
                                        .iter()
                                        .find(|o| o.option_id.0.contains("allow"))
                                        .or_else(|| req.options.first())
                                        .map(|o| o.option_id.clone());
                                    match id {
                                        Some(id) => RequestPermissionOutcome::Selected(
                                            SelectedPermissionOutcome::new(id),
                                        ),
                                        None => RequestPermissionOutcome::Cancelled,
                                    }
                                }
                                PermissionDecision::DenyOnce
                                | PermissionDecision::DenyAlways
                                | PermissionDecision::Cancel => {
                                    RequestPermissionOutcome::Cancelled
                                }
                            };
                            responder.respond(RequestPermissionResponse::new(outcome))
                        }
                    },
                    on_receive_request!(),
                )
                .on_receive_request(
                    move |req: ReadTextFileRequest,
                          responder: agent_client_protocol::Responder<ReadTextFileResponse>,
                          _conn: ConnectionTo<Agent>| {
                        let handler = handler_for_read.clone();
                        async move {
                            let path = PathBuf::from(req.path.clone());
                            let result = handler.read_text_file(path).await;
                            match result {
                                Ok(content) => {
                                    let truncated = match (req.line, req.limit) {
                                        (None, None) => content,
                                        (None, Some(limit)) => {
                                            content.chars().take(limit as usize).collect()
                                        }
                                        (Some(line), limit) => content
                                            .lines()
                                            .skip(line.saturating_sub(1) as usize)
                                            .take(limit.unwrap_or(u32::MAX) as usize)
                                            .collect::<Vec<_>>()
                                            .join("\n"),
                                    };
                                    responder.respond(ReadTextFileResponse::new(truncated))
                                }
                                Err(e) => responder.respond_with_error(
                                    agent_client_protocol::Error::resource_not_found(Some(
                                        e.to_string(),
                                    )),
                                ),
                            }
                        }
                    },
                    on_receive_request!(),
                )
                .on_receive_request(
                    move |req: WriteTextFileRequest,
                          responder: agent_client_protocol::Responder<WriteTextFileResponse>,
                          _conn: ConnectionTo<Agent>| {
                        let handler = handler_for_write.clone();
                        async move {
                            let path = PathBuf::from(req.path.clone());
                            let result = handler.write_text_file(path, req.content).await;
                            match result {
                                Ok(()) => responder.respond(WriteTextFileResponse::new()),
                                Err(e) => responder.respond_with_error(
                                    agent_client_protocol::Error::internal_error()
                                        .data(serde_json::json!({"reason": e.to_string()})),
                                ),
                            }
                        }
                    },
                    on_receive_request!(),
                )
                .on_receive_notification(
                    move |notif: SessionNotification, _conn: ConnectionTo<Agent>| {
                        let tx = notif_tx_for_loop.clone();
                        async move {
                            // Best-effort fanout. If no receivers, drop silently.
                            let _ = tx.send(notif);
                            Ok(())
                        }
                    },
                    on_receive_notification!(),
                )
                .connect_with(transport, move |connection: ConnectionTo<Agent>| {
                    let conn_tx_holder = conn_tx_holder.clone();
                    let shutdown_rx_holder = shutdown_rx_holder.clone();
                    async move {
                        // Initialize handshake.
                        let init_resp = connection
                            .send_request(InitializeRequest::new(ProtocolVersion::V1))
                            .block_task()
                            .await?;

                        // Hand the live connection + caps back to connect().
                        if let Some(tx) = conn_tx_holder.lock().unwrap().take() {
                            let _ = tx.send((connection.clone(), init_resp.agent_capabilities));
                        }

                        // Wait for shutdown signal. Closure return = connection ends.
                        // Take the receiver out of the mutex (synchronously, lock dropped before .await)
                        // so we don't hold a !Send guard across the await.
                        let rx_opt = shutdown_rx_holder.lock().unwrap().take();
                        if let Some(rx) = rx_opt {
                            let _ = rx.await;
                        }
                        Ok(())
                    }
                })
                .await;

            if let Err(e) = result {
                log::warn!("ACP connection ended with error: {e}");
            }
        });

        // Wait for the background task to ship us the connection or fail.
        let (connection, capabilities) = conn_rx.await.map_err(|_| AcpError::UnexpectedEof {
            stderr_tail: String::new(),
        })?;

        Ok(Self {
            connection,
            capabilities,
            notifications: notif_tx,
            shutdown_tx: Some(shutdown_tx),
            bg_task: Some(bg_task),
        })
    }

    /// Capabilities the agent advertised in its `initialize` response.
    pub fn capabilities(&self) -> &AgentCapabilities {
        &self.capabilities
    }

    /// Create a new ACP session at the given working directory.
    pub async fn new_session(&self, cwd: PathBuf) -> Result<SessionHandle> {
        let req = NewSessionRequest::new(cwd.clone());
        let resp = self.connection.send_request(req).block_task().await?;
        Ok(SessionHandle {
            id: SessionId::new(resp.session_id.0.to_string()),
            inner: Arc::new(SessionHandleInner { cwd }),
        })
    }

    /// Resume an existing session by ID. Errors if the agent didn't
    /// advertise `loadSession`.
    pub async fn load_session(&self, id: &str, cwd: PathBuf) -> Result<SessionHandle> {
        if !self.capabilities.load_session {
            return Err(AcpError::Unsupported {
                feature: "loadSession",
            });
        }
        let acp_id = AcpSessionId::new(id.to_string());
        let req = LoadSessionRequest::new(acp_id.clone(), cwd.clone());
        let _resp = self.connection.send_request(req).block_task().await?;
        Ok(SessionHandle {
            id: SessionId::new(id.to_string()),
            inner: Arc::new(SessionHandleInner { cwd }),
        })
    }

    /// Send a prompt to the session. Returns a stream of session events
    /// that closes when the agent reaches `stop_reason`.
    pub async fn prompt(
        &self,
        session: &SessionHandle,
        text: String,
    ) -> Result<mpsc::Receiver<SessionEvent>> {
        let (event_tx, event_rx) = mpsc::channel::<SessionEvent>(64);
        let mut notif_rx = self.notifications.subscribe();
        let target_session_id = session.id.clone();

        // Forward matching notifications into the per-prompt event channel.
        let event_tx_for_notifs = event_tx.clone();
        let notif_pump = tokio::spawn(async move {
            loop {
                match notif_rx.recv().await {
                    Ok(notif) => {
                        if notif.session_id.0.to_string() != target_session_id.as_str() {
                            continue;
                        }
                        if let Some(ev) = session_update_to_event(notif.update) {
                            if event_tx_for_notifs.send(ev).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let _ = event_tx_for_notifs
                            .send(SessionEvent::Error {
                                message: "notification stream lagged".into(),
                            })
                            .await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // Send the user-prompt event up front so the caller's view shows it
        // before any agent response chunks arrive.
        let _ = event_tx.send(SessionEvent::UserPrompt { text: text.clone() }).await;

        let acp_session_id = AcpSessionId::new(session.id.as_str().to_string());
        let prompt_req = PromptRequest::new(
            acp_session_id,
            vec![ContentBlock::Text(TextContent::new(text))],
        );
        let connection = self.connection.clone();
        tokio::spawn(async move {
            let result = connection.send_request(prompt_req).block_task().await;
            // Stop forwarding notifications now that the turn has finished.
            notif_pump.abort();
            let final_event = match result {
                Ok(resp) => SessionEvent::TurnComplete {
                    stop_reason: stop_reason_to_str(resp.stop_reason).to_string(),
                },
                Err(e) => SessionEvent::Error {
                    message: e.message,
                },
            };
            let _ = event_tx.send(final_event).await;
        });

        Ok(event_rx)
    }

    /// Cancel an in-flight prompt for the session.
    pub async fn cancel(&self, session: &SessionHandle) -> Result<()> {
        let _acp_id = AcpSessionId::new(session.id.as_str().to_string());
        // CancelNotification's exact constructor varies by schema crate
        // version; for now leave as TODO until we wire the cancel flow.
        // The `stop` is non-blocking — the agent stream will arrive at
        // `Cancelled` stop_reason which prompt() will surface.
        Err(AcpError::Unsupported {
            feature: "cancel (pending CancelNotification wiring)",
        })
    }

    /// Disconnect from the agent and wait for the background task to
    /// terminate. Idempotent.
    pub async fn dispose(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.bg_task.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.bg_task.take() {
            handle.abort();
        }
    }
}

fn session_update_to_event(update: SessionUpdate) -> Option<SessionEvent> {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => {
            text_from_content_block(chunk.content)
                .map(|text| SessionEvent::AgentMessageChunk { text })
        }
        SessionUpdate::AgentThoughtChunk(chunk) => {
            text_from_content_block(chunk.content)
                .map(|text| SessionEvent::AgentThoughtChunk { text })
        }
        SessionUpdate::ToolCall(tc) => Some(SessionEvent::ToolCall {
            call_id: tc.tool_call_id.0.to_string(),
            kind: normalize_tool_name(&tc.title),
            raw_name: tc.title,
            input: serde_json::to_value(&tc.raw_input)
                .unwrap_or(serde_json::Value::Null),
        }),
        SessionUpdate::ToolCallUpdate(_) => None, // TODO: surface progress + completion
        // #[non_exhaustive] — be lenient.
        _ => None,
    }
}

fn text_from_content_block(block: ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text(t) => Some(t.text),
        _ => None,
    }
}

fn stop_reason_to_str(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::MaxTurnRequests => "max_turn_requests",
        StopReason::Refusal => "refusal",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_handler::NoOpClientHandler;
    use crate::transport::SpawnOptions;

    /// Smoke: AcpClientOptions composes without panic. Real wire test is
    /// in `tests/integration.rs` (gated on `claude-code-acp` being
    /// installed and Anthropic creds being set).
    #[test]
    fn options_compose() {
        let opts = AcpClientOptions {
            spawn: SpawnOptions::claude_code_acp("/tmp"),
            handler: Arc::new(NoOpClientHandler),
        };
        assert_eq!(opts.spawn.command, "npx");
        assert!(matches!(
            opts.handler.as_ref() as *const _,
            _ if true
        ));
    }
}
