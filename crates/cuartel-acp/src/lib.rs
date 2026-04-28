//! Cuartel ACP client.
//!
//! Speaks the Agent Client Protocol (ACP) to a subprocess that hosts an
//! ACP server (`claude-code-acp`, `gemini-cli`, etc.). Replaces
//! `cuartel-rivet` as the agent-side transport for cuartel sessions.
//!
//! # Layering
//!
//! - [`AcpClient`] — the entry point. Spawns an ACP server as a child
//!   process, performs the `initialize` handshake, exposes session
//!   lifecycle (`new_session` / `prompt` / `cancel` / `load_session`).
//! - [`ClientHandler`] — handles **server-→client** requests
//!   (`fs/read_text_file`, `fs/write_text_file`, `request_permission`,
//!   terminal/* surface). Provides a default impl; cuartel-app overrides.
//! - [`normalize`] — collapses provider-specific tool names
//!   (`bash` / `Bash` / `shell` / `exec_command`) into canonical
//!   [`ToolKind`] values that the UI can switch on safely.
//! - [`transport`] — owns the child process + a `Lines` byte stream.
//!   Filters non-JSON banner output from `claude-code-acp` before the
//!   ACP parser sees it (one of the spike's main findings).
//! - [`error::AcpError`] — public error type. `Result<T, AcpError>` for
//!   public APIs; `anyhow::Result` for internal glue.
//!
//! # Provider notes
//!
//! - `claude-code-acp` runs all tools in-process and does **not** surface
//!   tool calls via `session/update` notifications (only
//!   `agent_message_chunk` content). Gemini and others do surface them.
//!   Code is written to handle both.
//! - The crate uses ACP `unstable` features (e.g. `loadSession`).
//!   Pin tracks Zed's pin (`=0.11.1`).
//!
//! See `KNOWLEDGE_BASE.md` §22 (A1 spike findings) and v2 doc Phase B1
//! for the design rationale and DoD.

pub mod client;
pub mod client_handler;
pub mod error;
pub mod normalize;
pub mod session;
pub mod transport;

pub use client::{AcpClient, AcpClientOptions};
pub use client_handler::{ClientHandler, NoOpClientHandler, PermissionDecision, PermissionRequest};
pub use error::{AcpError, Result};
pub use normalize::{normalize_tool_name, ToolKind};
pub use session::{SessionEvent, SessionHandle, SessionId};

// Re-export the upstream schema for callers that need raw types.
pub use agent_client_protocol::schema as acp_schema;
