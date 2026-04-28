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

use std::path::Path;

use crate::client::{AcpClient, AcpClientOptions};
use crate::client_handler::{ClientHandler, NoOpClientHandler};
use crate::error::Result;
use crate::transport::{build_inherited_path, resolve_executable, SpawnOptions};

/// Resolve `node` as a sibling of `npx` first (the common case for nvm/
/// homebrew/system installs — npx and node always live in the same bin
/// dir), then fall back to the standard probe order.
fn resolve_node_for(npx: Option<&Path>) -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("CUARTEL_NODE_PATH") {
        let path = std::path::PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(npx) = npx {
        if let Some(parent) = npx.parent() {
            let candidate = parent.join("node");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    resolve_executable("node", Some("CUARTEL_NODE_PATH"))
}

/// If `path` is a symlink, follow it (resolving relative targets against
/// the symlink's parent) and return the resolved absolute path. If
/// canonicalization fails (target doesn't exist, etc.), returns `None`
/// — callers should fall back to using the symlink path directly.
fn resolve_symlink_target(path: &Path) -> Option<std::path::PathBuf> {
    std::fs::canonicalize(path).ok()
}

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
    /// LocalSandbox preset for `claude-code-acp`.
    ///
    /// Robust spawn strategy: resolve `npx` to an absolute path, follow
    /// its symlink to the underlying `npx-cli.js` script, locate `node`
    /// (next to npx, or via PATH/fallbacks), and **spawn `node
    /// <npx-cli.js> claude-code-acp` directly**. This bypasses the
    /// `#!/usr/bin/env node` shebang in npx-cli.js, which otherwise
    /// requires `node` to be on the spawned process's `$PATH` —
    /// surfacing as misleading `npx: No such file or directory` errors
    /// when GUI-launched processes inherit a stripped PATH that doesn't
    /// include the user's nvm/asdf/etc bin dir.
    ///
    /// Probe order for both `npx` and `node`:
    ///   1. `CUARTEL_NPX_PATH` / `CUARTEL_NODE_PATH` env vars
    ///   2. The process's `$PATH`
    ///   3. Sibling-of-npx (node usually lives next to npx)
    ///   4. Common Node install locations (nvm, homebrew, asdf, fnm, volta, pnpm, bun)
    ///
    /// Also injects an extended `PATH` env into the spawned process so
    /// claude-code-acp's child processes (mcp servers etc.) can find
    /// their own `node`/`npx`.
    pub fn claude_code_acp() -> Self {
        let npx_path = resolve_executable("npx", Some("CUARTEL_NPX_PATH"));
        let node_path = resolve_node_for(npx_path.as_deref());

        let (command, args, env) = match (&npx_path, &node_path) {
            // Best path: spawn `node <resolved-npx-cli-script> claude-code-acp`.
            // Bypasses the shebang entirely.
            (Some(npx), Some(node)) => {
                let npx_script = resolve_symlink_target(npx).unwrap_or_else(|| npx.clone());
                let mut env = Vec::new();
                if let Some(dir) = node.parent() {
                    let inherited = build_inherited_path(&[dir]);
                    env.push(("PATH".to_string(), inherited));
                }
                (
                    node.to_string_lossy().into_owned(),
                    vec![
                        npx_script.to_string_lossy().into_owned(),
                        "claude-code-acp".into(),
                    ],
                    env,
                )
            }
            // npx but no node: fall back to spawning npx directly. Inject
            // npx's parent into PATH so the shebang-found env can locate node.
            (Some(npx), None) => {
                let mut env = Vec::new();
                if let Some(dir) = npx.parent() {
                    let inherited = build_inherited_path(&[dir]);
                    env.push(("PATH".to_string(), inherited));
                }
                log::warn!(
                    "cuartel-acp: resolved npx ({}) but could not locate `node`; \
                     spawning via shebang. If you hit `No such file or directory`, \
                     set CUARTEL_NODE_PATH=/absolute/path/to/node",
                    npx.display()
                );
                (
                    npx.to_string_lossy().into_owned(),
                    vec!["claude-code-acp".into()],
                    env,
                )
            }
            // Nothing resolved: literal "npx" + warning.
            (None, _) => {
                log::warn!(
                    "cuartel-acp: could not resolve npx absolute path; \
                     spawn will rely on the spawned process's $PATH. \
                     If you hit `No such file or directory`, set \
                     CUARTEL_NPX_PATH=/absolute/path/to/npx",
                );
                ("npx".to_string(), vec!["claude-code-acp".into()], Vec::new())
            }
        };
        Self {
            spawn_template: SpawnOptions {
                command,
                args,
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
        let cmd = sb.spawn_template.command.as_str();
        let args = &sb.spawn_template.args;
        // Three valid shapes depending on what could be resolved on the
        // dev machine:
        //   - Best:  command=node, args=[<npx-cli.js>, "claude-code-acp"]
        //   - OK:    command=<abs-npx>, args=["claude-code-acp"]
        //   - None:  command="npx",     args=["claude-code-acp"]
        if cmd.ends_with("/node") || cmd == "node" {
            assert_eq!(args.len(), 2, "node spawn should pass npx-cli.js + cmd");
            assert!(
                args[0].ends_with(".js"),
                "first arg should be npx-cli.js path, got {:?}",
                args[0],
            );
            assert_eq!(args[1], "claude-code-acp");
        } else {
            assert!(
                cmd == "npx" || cmd.ends_with("/npx"),
                "expected node, npx (literal), or absolute npx; got {cmd:?}",
            );
            assert_eq!(args, &vec!["claude-code-acp".to_string()]);
        }
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
