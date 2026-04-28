//! Session-side types: opaque `SessionId`, the `SessionHandle` cuartel
//! holds onto for an active session, and `SessionEvent` (the unit of
//! the event-log interface from v2 D9).
//!
//! The actual event-log storage (SQLite-backed) lives in `cuartel-db`;
//! this crate just defines the wire-side event shape and the handle.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::normalize::ToolKind;

/// Opaque ACP session identifier. Wraps the underlying ACP server's
/// session ID (e.g. UUID assigned by claude-code-acp).
///
/// **Note:** ACP servers typically have *two* session-id layers — the
/// ACP-level ID (this) and an internal SDK-level ID (e.g. Claude Code's
/// own `session_id` UUID). cuartel only tracks the ACP-level one;
/// internal IDs are debug noise (KB §22 spike finding).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new(s: impl Into<String>) -> Self {
        SessionId(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A live session held by [`crate::AcpClient`]. Carries the opaque ID
/// and a reference back to the connection so callers can prompt it.
///
/// Cheap to clone (it's an `Arc` of immutable state internally).
#[derive(Debug, Clone)]
pub struct SessionHandle {
    pub id: SessionId,
    pub(crate) inner: Arc<SessionHandleInner>,
}

#[derive(Debug)]
pub(crate) struct SessionHandleInner {
    pub(crate) cwd: std::path::PathBuf,
}

impl SessionHandle {
    /// Working directory the session was created with.
    pub fn cwd(&self) -> &std::path::Path {
        &self.inner.cwd
    }
}

/// Append-only event added to a session's timeline.
///
/// This is what cuartel-db's `SessionEventLog` (v2 D9) stores. Keep it
/// simple and `serde`-friendly so storage backends can serialize freely.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionEvent {
    /// User-authored prompt text.
    UserPrompt { text: String },

    /// A streaming chunk of agent message text.
    AgentMessageChunk { text: String },

    /// Agent's reasoning / thought stream (where supported).
    AgentThoughtChunk { text: String },

    /// A tool call started or progressed. Tool name is normalized via
    /// [`ToolKind`]; raw provider name preserved in `raw_name`.
    ToolCall {
        call_id: String,
        kind: ToolKind,
        raw_name: String,
        input: serde_json::Value,
    },

    /// A tool call completed (success or failure).
    ToolCallResult {
        call_id: String,
        is_error: bool,
        output: serde_json::Value,
    },

    /// Permission requested by the agent. The UI surfaces this; the
    /// handler's reply lands as [`SessionEvent::PermissionResolved`].
    PermissionRequested {
        request_id: String,
        tool: String,
        details: serde_json::Value,
    },

    PermissionResolved {
        request_id: String,
        approved: bool,
    },

    /// The current turn ended.
    TurnComplete { stop_reason: String },

    /// An error occurred during the turn (network, agent error, etc.).
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_round_trips_as_string() {
        let id = SessionId::new("abc-123");
        assert_eq!(id.as_str(), "abc-123");
        assert_eq!(format!("{id}"), "abc-123");
        assert_eq!(id.clone(), id);
    }

    #[test]
    fn session_event_serializes_with_tag() {
        let ev = SessionEvent::UserPrompt {
            text: "hi".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"type\":\"user_prompt\""), "got: {json}");
        assert!(json.contains("\"text\":\"hi\""));
    }

    #[test]
    fn tool_call_event_preserves_raw_and_kind() {
        let ev = SessionEvent::ToolCall {
            call_id: "call-1".into(),
            kind: ToolKind::Shell,
            raw_name: "Bash".into(),
            input: serde_json::json!({"command": "ls"}),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"raw_name\":\"Bash\""));
        // Round-trip
        let back: SessionEvent = serde_json::from_str(&json).unwrap();
        match back {
            SessionEvent::ToolCall { kind, raw_name, .. } => {
                assert_eq!(kind, ToolKind::Shell);
                assert_eq!(raw_name, "Bash");
            }
            _ => panic!("wrong variant"),
        }
    }
}
