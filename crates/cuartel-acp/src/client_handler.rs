//! Server-→client request handlers.
//!
//! ACP servers can ask the client to do work on their behalf:
//! `fs/read_text_file`, `fs/write_text_file`, `request_permission`,
//! and the terminal-control surface (`terminal/create`, `terminal/output`,
//! `terminal/wait`, `terminal/release`, `terminal/kill`). Cuartel
//! provides a [`ClientHandler`] trait so the daemon can plug in a real
//! impl that mediates filesystem access (against the workspace
//! registry's access policy) and surfaces permission prompts in the UI.
//!
//! For the MVP / `LocalSandbox` topology, file access is just direct
//! `tokio::fs` calls scoped to the workspace's worktrees. For the
//! Phase-D `AppleVzSandbox` / `HetznerSandbox` topologies, the handler
//! routes through vsock/SSH to the in-VM filesystem (but the trait stays
//! the same).

use std::path::PathBuf;

use async_trait::async_trait;

use crate::error::Result;

/// Cuartel's reply to a `request_permission` call.
#[derive(Debug, Clone)]
pub enum PermissionDecision {
    AllowOnce,
    AllowAlways,
    DenyOnce,
    DenyAlways,
    Cancel,
}

/// Description of a permission ask, given to the UI.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub tool_name: String,
    pub raw_input: serde_json::Value,
}

/// Pluggable handler for ACP server-→client requests.
///
/// The MVP impl in cuartel-daemon will:
/// - Validate paths against the active workspace's worktrees
///   (prevent reads/writes outside scope; mirror Zed's
///   `project_path_for_absolute_path` pattern, KB §4.1).
/// - Surface permission requests in the GPUI permission window and
///   await the user's decision.
/// - Spawn real PTYs for `terminal/create` (later — Phase E for Tier-2
///   computer use).
#[async_trait]
pub trait ClientHandler: Send + Sync {
    /// Read a text file the agent wants to look at.
    async fn read_text_file(&self, path: PathBuf) -> Result<String>;

    /// Write a text file the agent wants to create or replace.
    async fn write_text_file(&self, path: PathBuf, content: String) -> Result<()>;

    /// Resolve a permission request from the agent. The UI is expected
    /// to surface this and block until the user decides; this trait is
    /// async so the impl can await the user freely.
    async fn request_permission(&self, req: PermissionRequest) -> Result<PermissionDecision>;
}

/// Permissive impl for tests / dev: reads/writes the host filesystem
/// without any path scoping, auto-allows all permission requests.
///
/// **Do NOT use in production.** Workspace-scoped path validation is
/// a load-bearing safety property; this impl trades it for test-loop speed.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpClientHandler;

#[async_trait]
impl ClientHandler for NoOpClientHandler {
    async fn read_text_file(&self, path: PathBuf) -> Result<String> {
        Ok(tokio::fs::read_to_string(path).await?)
    }

    async fn write_text_file(&self, path: PathBuf, content: String) -> Result<()> {
        tokio::fs::write(path, content).await?;
        Ok(())
    }

    async fn request_permission(&self, _req: PermissionRequest) -> Result<PermissionDecision> {
        Ok(PermissionDecision::AllowOnce)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_handler_round_trips_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        let h = NoOpClientHandler;

        h.write_text_file(path.clone(), "world".into()).await.unwrap();
        let read_back = h.read_text_file(path).await.unwrap();
        assert_eq!(read_back, "world");
    }

    #[tokio::test]
    async fn noop_handler_auto_approves_permissions() {
        let h = NoOpClientHandler;
        let req = PermissionRequest {
            tool_name: "Bash".into(),
            raw_input: serde_json::json!({"command": "rm -rf /"}),
        };
        let decision = h.request_permission(req).await.unwrap();
        assert!(matches!(decision, PermissionDecision::AllowOnce));
    }
}
