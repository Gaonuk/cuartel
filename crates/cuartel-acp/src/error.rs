//! Public error type for `cuartel-acp`.
//!
//! Hand-written enum + `Display` + `std::error::Error` impls per cuartel
//! convention (no `thiserror`). `Result<T>` is the crate-public alias.

use std::fmt;
use std::io;

use agent_client_protocol::ErrorCode;

pub type Result<T> = std::result::Result<T, AcpError>;

#[derive(Debug)]
pub enum AcpError {
    /// Failed to spawn or interact with the ACP server subprocess.
    Spawn { command: String, source: io::Error },

    /// The ACP server's stdout closed before a response arrived.
    /// Usually means the subprocess crashed; pair with stderr capture.
    UnexpectedEof { stderr_tail: String },

    /// JSON-RPC protocol violation (malformed framing, unexpected
    /// message type, etc.). Includes the offending line for debugging.
    Protocol { reason: String, raw: Option<String> },

    /// The ACP server returned an error response. `code` follows
    /// JSON-RPC semantics; `AuthRequired` is special-cased so the UI can
    /// trigger an auth flow rather than treating it as a generic crash.
    AgentError {
        code: ErrorCode,
        message: String,
        data: Option<serde_json::Value>,
    },

    /// The ACP server reports a feature is unavailable (e.g. trying
    /// `loadSession` against a server that didn't advertise it).
    Unsupported { feature: &'static str },

    /// I/O failure on the transport (stdin/stdout pipe, virtio-serial,
    /// SSH channel, …).
    Io(io::Error),

    /// JSON serialization or deserialization failure.
    Serde(serde_json::Error),

    /// Catch-all for upstream errors we don't have a richer mapping for.
    /// Wraps `agent-client-protocol`'s own error type.
    Acp(agent_client_protocol::Error),
}

impl AcpError {
    /// `true` if this error originates from the agent declaring it needs
    /// authentication. The UI should respond by surfacing the auth flow.
    pub fn is_auth_required(&self) -> bool {
        matches!(
            self,
            AcpError::AgentError {
                code: ErrorCode::AuthRequired,
                ..
            }
        )
    }
}

impl fmt::Display for AcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AcpError::Spawn { command, source } => {
                write!(f, "failed to spawn ACP server `{command}`: {source}")
            }
            AcpError::UnexpectedEof { stderr_tail } => {
                if stderr_tail.is_empty() {
                    write!(f, "ACP server stdout closed unexpectedly")
                } else {
                    write!(
                        f,
                        "ACP server stdout closed unexpectedly; stderr tail: {stderr_tail}"
                    )
                }
            }
            AcpError::Protocol { reason, raw } => match raw {
                Some(line) => write!(f, "ACP protocol error: {reason}; raw: {line}"),
                None => write!(f, "ACP protocol error: {reason}"),
            },
            AcpError::AgentError { code, message, .. } => {
                write!(f, "ACP server returned error ({code}): {message}")
            }
            AcpError::Unsupported { feature } => {
                write!(f, "ACP server does not support `{feature}`")
            }
            AcpError::Io(e) => write!(f, "I/O error: {e}"),
            AcpError::Serde(e) => write!(f, "JSON error: {e}"),
            AcpError::Acp(e) => write!(f, "ACP error: {e}"),
        }
    }
}

impl std::error::Error for AcpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AcpError::Spawn { source, .. } => Some(source),
            AcpError::Io(e) => Some(e),
            AcpError::Serde(e) => Some(e),
            AcpError::Acp(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for AcpError {
    fn from(e: io::Error) -> Self {
        AcpError::Io(e)
    }
}

impl From<serde_json::Error> for AcpError {
    fn from(e: serde_json::Error) -> Self {
        AcpError::Serde(e)
    }
}

impl From<agent_client_protocol::Error> for AcpError {
    fn from(e: agent_client_protocol::Error) -> Self {
        // The upstream `Error` is { code, message, data } — public fields,
        // not getters. Always promote to typed variant; we don't lose info.
        AcpError::AgentError {
            code: e.code,
            message: e.message,
            data: e.data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_required_detection() {
        let err = AcpError::AgentError {
            code: ErrorCode::AuthRequired,
            message: "please log in".into(),
            data: None,
        };
        assert!(err.is_auth_required());

        let other = AcpError::Unsupported {
            feature: "loadSession",
        };
        assert!(!other.is_auth_required());
    }

    #[test]
    fn display_includes_command_for_spawn() {
        let err = AcpError::Spawn {
            command: "claude-code-acp".into(),
            source: io::Error::new(io::ErrorKind::NotFound, "no such file"),
        };
        let msg = format!("{err}");
        assert!(msg.contains("claude-code-acp"));
        assert!(msg.contains("no such file"));
    }
}
