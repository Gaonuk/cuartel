//! [`AcpClient`] — the entry point for cuartel-acp.
//!
//! Spawns an ACP server subprocess via [`crate::transport`], performs
//! the `initialize` handshake, exposes session lifecycle (`new_session`,
//! `prompt`, `cancel`, `load_session`), and surfaces server-→client
//! requests through a [`crate::ClientHandler`].
//!
//! ## Status
//!
//! **Scaffold.** The public API surface and types are stable; the
//! actual `agent-client-protocol::Client.builder()....connect_with(...)`
//! plumbing is left as a single TODO until the first `cargo check`
//! against `agent-client-protocol = "=0.11.1"` confirms the exact
//! signatures. Once those are settled, fill in:
//!   - `connect()`: spawn → wrap stdio in `Lines` (after filtering via
//!     [`crate::transport::is_jsonrpc_line`]) → call the ACP `Client`
//!     builder with our `on_receive_request` / `on_receive_notification`
//!     handlers → keep `ConnectionTo<Agent>` in `self.connection`.
//!   - `initialize()`, `new_session()`, `prompt()`, `cancel()`,
//!     `load_session()`: dispatch via the connection's typed RPC helpers.
//!
//! See KB §22 (A1 spike findings) and the parallel-research Bottom Line:
//!
//! ```text
//! Pin agent-client-protocol = "=0.11.1" with features ["unstable"].
//! Implement Client side via builder; register on_receive_request for
//! request_permission, read_text_file, write_text_file, plus terminal/[create|kill|...].
//! Use Lines + own BufReader/stderr split — gives place to drop non-JSON
//! banner lines from claude-code-acp before they reach the parser.
//! Insert sessions into registry BEFORE awaiting session/load.
//! Match SessionUpdate / ContentBlock with _ => arms (#[non_exhaustive]).
//! Bridge acp::Error → AcpError; preserve ErrorCode::AuthRequired.
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::client_handler::ClientHandler;
use crate::error::Result;
use crate::session::{SessionEvent, SessionHandle};
use crate::transport::SpawnOptions;

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
///
/// One `AcpClient` corresponds to one running ACP-server subprocess.
/// `cuartel-daemon` typically spawns one per session (so each session
/// gets its own clean agent process).
pub struct AcpClient {
    #[allow(dead_code)] // used by methods to be filled in
    handler: Arc<dyn ClientHandler>,
    #[allow(dead_code)]
    spawn_opts: SpawnOptions,
    // TODO: hold the live ConnectionTo<Agent> from agent-client-protocol
    // here once we settle on the type's exact name in v0.11.1.
}

impl AcpClient {
    /// Spawn the ACP server and complete the `initialize` handshake.
    ///
    /// On success the returned client is ready to call [`Self::new_session`]
    /// or [`Self::load_session`].
    pub async fn connect(opts: AcpClientOptions) -> Result<Self> {
        // TODO(B1): this is the load-bearing wiring. Steps:
        //   1. crate::transport::spawn(&opts.spawn)?
        //   2. Wrap child.stdout in tokio::io::BufReader, then
        //      AsyncBufReadExt::lines(); filter via
        //      crate::transport::is_jsonrpc_line; reassemble into a
        //      futures-AsyncRead via tokio_util::compat.
        //   3. Build agent-client-protocol's Client via its builder API,
        //      registering handlers that route through self.handler.
        //   4. .connect_with(byte_stream, |conn| async { store conn; pending() })
        //   5. Send the InitializeRequest with the negotiated capabilities.
        //   6. Stash the ConnectionTo<Agent> in self.
        //
        // Until then this is a NotConnected stub for compile / test
        // scaffolding.
        Ok(Self {
            handler: opts.handler,
            spawn_opts: opts.spawn,
        })
    }

    /// Create a new ACP session at the given working directory.
    pub async fn new_session(&self, _cwd: PathBuf) -> Result<SessionHandle> {
        // TODO(B1): connection.send_request(NewSessionRequest { ... }).await
        unimplemented!("AcpClient::new_session — pending B1 ACP wiring")
    }

    /// Resume an existing session by ID. Only valid if the server's
    /// `initialize` response advertised `agentCapabilities.loadSession`.
    /// Returns [`crate::AcpError::Unsupported`] otherwise.
    pub async fn load_session(&self, _id: &str, _cwd: PathBuf) -> Result<SessionHandle> {
        // TODO(B1): check capability flag we cached at connect; then
        //   connection.send_request(LoadSessionRequest { ... }).await.
        unimplemented!("AcpClient::load_session — pending B1 ACP wiring")
    }

    /// Send a prompt to the session. Returns a stream of session events.
    /// The stream completes when the agent reaches a stop condition.
    pub async fn prompt(
        &self,
        _session: &SessionHandle,
        _text: String,
    ) -> Result<mpsc::Receiver<SessionEvent>> {
        // TODO(B1): create channel; spawn task that awaits the
        //   session/prompt RPC and forwards session/update notifications
        //   into the channel as SessionEvents. Final stop_reason closes
        //   the receiver.
        unimplemented!("AcpClient::prompt — pending B1 ACP wiring")
    }

    /// Cancel an in-flight prompt for the session.
    pub async fn cancel(&self, _session: &SessionHandle) -> Result<()> {
        // TODO(B1): connection.send_notification(CancelNotification { sessionId })
        unimplemented!("AcpClient::cancel — pending B1 ACP wiring")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_handler::NoOpClientHandler;
    use crate::transport::SpawnOptions;

    /// Connect-shape compile test: the public API is callable.
    /// Real wire test ships with B1's full impl.
    #[tokio::test]
    async fn connect_options_compose_without_panic() {
        let opts = AcpClientOptions {
            spawn: SpawnOptions::claude_code_acp("/tmp"),
            handler: Arc::new(NoOpClientHandler),
        };
        let _client = AcpClient::connect(opts).await.unwrap();
    }
}
