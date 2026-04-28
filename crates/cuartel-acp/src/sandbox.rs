//! [`Sandbox`] trait — the agent provisioning abstraction.
//!
//! Three Sandbox impls are planned (KB §4.20 + v2 doc §abstractions):
//!
//! | impl | when | Phase |
//! |---|---|---|
//! | [`LocalSandbox`] | MVP default — claude-code-acp as plain host subprocess | B2 |
//! | `AppleVzSandbox` | opt-in secure mode — Apple VZ Linux VM | D0 |
//! | `HetznerSandbox` | remote secure mode — Firecracker on Hetzner | D1 |
//!
//! All return an [`AcpClient`] when `spawn_agent` succeeds; the caller
//! drives sessions through it. The trait is intentionally narrow: it
//! does not own per-session lifecycle, just the spawn-the-server part.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::client::{AcpClient, AcpClientOptions};
use crate::client_handler::{ClientHandler, NoOpClientHandler};
use crate::error::Result;
use crate::transport::{build_inherited_path, resolve_executable, SpawnOptions};

/// Pluggable sandbox provisioner.
///
/// Each impl knows how to materialize the environment its agent runs
/// in (host process group, local VM, remote VM) and then spawn an ACP
/// server inside it. From the caller's perspective the result is the
/// same: an [`AcpClient`] connected to a live agent.
#[async_trait]
pub trait Sandbox: Send + Sync {
    /// Human-readable kind, for logging / telemetry.
    fn kind(&self) -> &'static str;

    /// Spawn an ACP server connected to a workspace at `cwd` and
    /// return a connected [`AcpClient`]. The handler mediates server-
    /// →client requests (file I/O, permission prompts).
    async fn spawn_agent(
        &self,
        cwd: PathBuf,
        handler: Arc<dyn ClientHandler>,
    ) -> Result<AcpClient>;
}

/// Tier-0 sandbox: claude-code-acp as a plain host OS subprocess. No
/// isolation. Same approach as Zed/Polyscope/Paseo/Cursor for
/// interactive coding sessions where the user-in-the-loop permission
/// UI is the safety net.
///
/// Phase D introduces `AppleVzSandbox` for autonomous/scheduled work
/// where actual VM isolation matters; this stays the MVP default.
#[derive(Debug, Clone)]
pub struct LocalSandbox {
    spawn_template: SpawnOptions,
}

impl LocalSandbox {
    /// LocalSandbox preset for `claude-code-acp` via `npx`. Inherits the
    /// host process env so `~/.claude/` subscription auth (or
    /// `ANTHROPIC_API_KEY`) flows through unchanged.
    ///
    /// Resolves `npx` to an absolute path at construction so the spawn
    /// works even if the calling process's `$PATH` is stripped (macOS
    /// GUI-launched apps, GPUI processes started outside a shell, etc.).
    /// Probe order: `CUARTEL_NPX_PATH` env var → `$PATH` → common Node
    /// install locations (nvm, homebrew, asdf, fnm, volta, pnpm, bun).
    /// If npx isn't found anywhere, falls back to the literal `"npx"`
    /// string and lets the spawn-time error surface the missing binary.
    ///
    /// Also injects an extended `PATH` env into the spawned process so
    /// npx can find its sibling `node` binary in the same directory
    /// even when the parent's `$PATH` was empty.
    pub fn claude_code_acp() -> Self {
        let resolved = resolve_executable("npx", Some("CUARTEL_NPX_PATH"));
        let (command, env) = match resolved {
            Some(abs) => {
                let parent_dir = abs.parent().map(|p| p.to_path_buf());
                let mut env = Vec::new();
                if let Some(dir) = parent_dir.as_deref() {
                    let inherited = build_inherited_path(&[dir]);
                    env.push(("PATH".to_string(), inherited));
                }
                (abs.to_string_lossy().into_owned(), env)
            }
            None => {
                log::warn!(
                    "cuartel-acp: could not resolve npx absolute path; \
                     spawn will rely on the spawned process's $PATH. \
                     If you hit `No such file or directory`, set \
                     CUARTEL_NPX_PATH=/absolute/path/to/npx",
                );
                ("npx".to_string(), Vec::new())
            }
        };
        Self {
            spawn_template: SpawnOptions {
                command,
                args: vec!["claude-code-acp".into()],
                cwd: PathBuf::from("/"), // overridden per-spawn
                env,
                clear_env: false,
            },
        }
    }

    /// Build a LocalSandbox from a custom [`SpawnOptions`] template
    /// (e.g. for `gemini --acp` or a self-built ACP server).
    pub fn from_spawn(template: SpawnOptions) -> Self {
        Self {
            spawn_template: template,
        }
    }
}

impl Default for LocalSandbox {
    fn default() -> Self {
        Self::claude_code_acp()
    }
}

#[async_trait]
impl Sandbox for LocalSandbox {
    fn kind(&self) -> &'static str {
        "local"
    }

    async fn spawn_agent(
        &self,
        cwd: PathBuf,
        handler: Arc<dyn ClientHandler>,
    ) -> Result<AcpClient> {
        let mut spawn = self.spawn_template.clone();
        spawn.cwd = cwd;
        AcpClient::connect(AcpClientOptions { spawn, handler }).await
    }
}

/// Convenience: spawn a LocalSandbox-backed AcpClient with the
/// permissive [`NoOpClientHandler`]. Useful for tests and for the
/// "feature-flagged ACP driver" path in `session_host.rs` until the
/// daemon's workspace-policy-mediated handler lands in Phase C.
pub async fn spawn_local_with_default_handler(cwd: PathBuf) -> Result<AcpClient> {
    LocalSandbox::claude_code_acp()
        .spawn_agent(cwd, Arc::new(NoOpClientHandler))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_sandbox_default_uses_claude_code_acp() {
        let sb = LocalSandbox::default();
        assert_eq!(sb.kind(), "local");
        // Command may be absolute (resolved npx) or literal "npx" if
        // resolution failed; we don't assume the dev machine has npx.
        let cmd = sb.spawn_template.command.as_str();
        assert!(
            cmd == "npx" || cmd.ends_with("/npx"),
            "expected npx (literal or absolute), got {cmd:?}",
        );
        assert_eq!(sb.spawn_template.args, vec!["claude-code-acp"]);
    }

    #[test]
    fn from_spawn_round_trips_template() {
        let template = SpawnOptions {
            command: "/opt/bin/gemini".into(),
            args: vec!["--acp".into()],
            cwd: PathBuf::from("/tmp"),
            env: vec![("MY_VAR".into(), "x".into())],
            clear_env: false,
        };
        let sb = LocalSandbox::from_spawn(template.clone());
        assert_eq!(sb.spawn_template.command, "/opt/bin/gemini");
        assert_eq!(sb.spawn_template.args, vec!["--acp"]);
    }
}
