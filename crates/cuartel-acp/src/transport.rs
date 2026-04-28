//! Transport: spawn the ACP server subprocess and expose framed I/O.
//!
//! Two responsibilities:
//!   1. Spawn an ACP server (`claude-code-acp`, `gemini --acp`, …) with
//!      a chosen working directory and env vars.
//!   2. Filter stdout BEFORE it reaches the JSON-RPC parser. Some ACP
//!      servers (notably claude-code-acp) print debug banners to stdout
//!      mixed with the JSON-RPC frames; the upstream `agent-client-protocol`
//!      crate does not silently skip non-JSON lines, so we strip them
//!      here. (KB §22 spike finding; Zed handles this same way per the
//!      research report.)
//!
//! The actual ACP `connect_with` call lives in [`crate::client`]; this
//! module just gives it well-formed byte streams.

use std::process::Stdio;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

use crate::error::{AcpError, Result};

/// Options for spawning an ACP server subprocess.
#[derive(Debug, Clone)]
pub struct SpawnOptions {
    /// Binary or command name (e.g. `npx`, `claude-code-acp`).
    pub command: String,
    /// Args (e.g. `["claude-code-acp"]` for the npx case).
    pub args: Vec<String>,
    /// Working directory the server starts in. ACP `session/new` will
    /// also pass a `cwd`; this is the spawn-time default.
    pub cwd: std::path::PathBuf,
    /// Env vars to set/override. Existing process env is inherited
    /// unless [`Self::clear_env`] is `true`.
    pub env: Vec<(String, String)>,
    pub clear_env: bool,
}

impl SpawnOptions {
    /// Default options for `claude-code-acp` via `npx`. Inherits env so
    /// `~/.claude/` subscription auth + any `ANTHROPIC_API_KEY` flow
    /// through unchanged.
    pub fn claude_code_acp(cwd: impl Into<std::path::PathBuf>) -> Self {
        Self {
            command: "npx".into(),
            args: vec!["claude-code-acp".into()],
            cwd: cwd.into(),
            env: Vec::new(),
            clear_env: false,
        }
    }
}

/// A live ACP-server subprocess plus split I/O handles.
///
/// Owners are responsible for keeping `child` alive while reading
/// `stdout` / writing `stdin`. Dropping `Spawned` waits for the child
/// to exit (via `Child`'s drop semantics).
pub struct Spawned {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}

/// Spawn an ACP server subprocess with stdio piped in three directions.
pub fn spawn(opts: &SpawnOptions) -> Result<Spawned> {
    let mut cmd = Command::new(&opts.command);
    cmd.args(&opts.args)
        .current_dir(&opts.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    if opts.clear_env {
        cmd.env_clear();
    }
    for (k, v) in &opts.env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().map_err(|source| AcpError::Spawn {
        command: opts.command.clone(),
        source,
    })?;

    let stdin = child.stdin.take().ok_or_else(|| AcpError::Spawn {
        command: opts.command.clone(),
        source: std::io::Error::other("stdin not piped"),
    })?;
    let stdout = child.stdout.take().ok_or_else(|| AcpError::Spawn {
        command: opts.command.clone(),
        source: std::io::Error::other("stdout not piped"),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| AcpError::Spawn {
        command: opts.command.clone(),
        source: std::io::Error::other("stderr not piped"),
    })?;

    Ok(Spawned {
        child,
        stdin,
        stdout,
        stderr,
    })
}

/// `true` if a line from the ACP server's stdout is a valid JSON-RPC
/// frame envelope. Anything else is debug noise that `claude-code-acp`
/// emits and that we must strip before passing to the ACP parser.
///
/// We don't fully validate the JSON-RPC shape here — the ACP parser
/// downstream will catch malformed messages. We just check that the
/// line *looks like* JSON (starts with `{`, parses).
pub fn is_jsonrpc_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
}

/// Trait alias for the kind of byte stream the ACP crate's `Lines`
/// helper expects. Re-exported here so callers don't need to import
/// futures' I/O traits separately.
pub trait AsyncByteStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> AsyncByteStream for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonrpc_filter_accepts_real_frame() {
        let line = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}"#;
        assert!(is_jsonrpc_line(line));
    }

    #[test]
    fn jsonrpc_filter_accepts_with_leading_whitespace() {
        let line = "   {\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{}}";
        assert!(is_jsonrpc_line(line));
    }

    #[test]
    fn jsonrpc_filter_rejects_banner_text() {
        assert!(!is_jsonrpc_line("[ACP] No CLAUDE_API_KEY found"));
        assert!(!is_jsonrpc_line("Shutting down gracefully..."));
    }

    #[test]
    fn jsonrpc_filter_rejects_pretty_printed_dump_lines() {
        // claude-code-acp sometimes pretty-prints message dumps line by
        // line; only the curly-brace start is a JSON object, the indented
        // continuation lines aren't.
        assert!(!is_jsonrpc_line("  \"type\": \"system\","));
        assert!(!is_jsonrpc_line("  ],"));
    }

    #[test]
    fn jsonrpc_filter_rejects_empty() {
        assert!(!is_jsonrpc_line(""));
        assert!(!is_jsonrpc_line("   "));
    }

    #[test]
    fn jsonrpc_filter_rejects_arrays() {
        // JSON-RPC frames are always objects, never bare arrays.
        // (Even batch is wrapped, but ACP doesn't use batch.)
        assert!(!is_jsonrpc_line("[1, 2, 3]"));
    }

    #[test]
    fn spawn_options_defaults_for_claude_code_acp() {
        let opts = SpawnOptions::claude_code_acp("/tmp");
        assert_eq!(opts.command, "npx");
        assert_eq!(opts.args, vec!["claude-code-acp"]);
        assert_eq!(opts.cwd, std::path::PathBuf::from("/tmp"));
        assert!(!opts.clear_env);
    }
}
