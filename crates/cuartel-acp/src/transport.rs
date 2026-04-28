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

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

use crate::error::{AcpError, Result};

/// Walk `$PATH` looking for an executable. Returns `None` if missing.
///
/// Uses the process's current `PATH` env var. Relies on standard
/// `:`-separated path semantics; handles missing/unreadable entries.
/// Does **not** check the executable bit (matches the `which` shell
/// builtin's lax behavior on macOS where `x` perms are usually fine).
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Common locations where Node-related binaries get installed even when
/// the GUI process's `$PATH` is stripped (macOS `.app` bundles, etc.).
/// Probed only as a fallback when [`find_in_path`] returns `None`.
fn fallback_locations(name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);

    // nvm: ~/.nvm/versions/node/*/bin/<name>. Pick the highest-versioned
    // dir lexically — matches nvm's own ordering for default-version
    // resolution closely enough.
    if let Some(home) = &home {
        let nvm_root = home.join(".nvm").join("versions").join("node");
        if let Ok(entries) = std::fs::read_dir(&nvm_root) {
            let mut versions: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_dir())
                .collect();
            versions.sort();
            if let Some(latest) = versions.last() {
                out.push(latest.join("bin").join(name));
            }
        }

        // asdf, fnm, volta — common alternates.
        out.push(home.join(".asdf").join("shims").join(name));
        out.push(home.join(".fnm").join("aliases").join("default").join("bin").join(name));
        out.push(home.join(".volta").join("bin").join(name));
        // pnpm + bun layouts.
        out.push(home.join("Library").join("pnpm").join(name));
        out.push(home.join(".bun").join("bin").join(name));
        out.push(home.join(".local").join("bin").join(name));
    }

    // Homebrew: Apple Silicon then Intel; system /usr/local; system /usr/bin.
    out.push(PathBuf::from("/opt/homebrew/bin").join(name));
    out.push(PathBuf::from("/usr/local/bin").join(name));
    out.push(PathBuf::from("/usr/bin").join(name));

    out
}

/// Resolve an executable name to an absolute path, robust against PATH
/// stripping in GUI-launched processes. Probes (in order):
///   1. `<env_override>` env var if set (e.g. `CUARTEL_NPX_PATH=...`)
///   2. The process's `$PATH`
///   3. A fallback list of common Node install locations (nvm, homebrew,
///      asdf, fnm, volta, pnpm, bun, /usr/local/bin, /usr/bin)
///
/// Returns the absolute path on success, or `None` if not found anywhere.
pub fn resolve_executable(name: &str, env_override: Option<&str>) -> Option<PathBuf> {
    if let Some(env_var) = env_override {
        if let Ok(p) = std::env::var(env_var) {
            let path = PathBuf::from(p);
            if path.is_file() {
                return Some(path);
            }
        }
    }

    if let Some(p) = find_in_path(name) {
        return Some(p);
    }

    fallback_locations(name).into_iter().find(|p| p.is_file())
}

/// Build a `PATH`-like env var that includes the parent dirs of the
/// resolved binaries. Lets the spawned process find sibling binaries
/// (e.g. `npx` finding `node` next to itself) even if the parent
/// process's `$PATH` is stripped.
pub fn build_inherited_path(extra_dirs: &[&Path]) -> String {
    let mut parts: Vec<String> = extra_dirs
        .iter()
        .filter_map(|p| p.to_str().map(|s| s.to_string()))
        .collect();
    if let Some(existing) = std::env::var_os("PATH") {
        if let Some(s) = existing.to_str() {
            parts.push(s.to_string());
        }
    }
    parts.join(":")
}

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
    // posix_spawn returns ENOENT (os error 2) when the cwd doesn't
    // exist, but Rust reports it against the binary path — so a stale
    // CUARTEL_ACP_CWD silently turns into "command not found" errors
    // that look like missing-binary issues. Catch this case up front
    // with a message the user can act on.
    if !opts.cwd.is_dir() {
        return Err(AcpError::Spawn {
            command: format!(
                "{} — cwd `{}` does not exist or is not a directory \
                 (set CUARTEL_ACP_CWD to a real repo path, or unset it \
                 to use the cuartel-app process's current dir)",
                opts.command,
                opts.cwd.display(),
            ),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "spawn cwd missing",
            ),
        });
    }

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

    let mut child = cmd.spawn().map_err(|source| {
        let hint = if source.kind() == std::io::ErrorKind::NotFound
            && opts.command == "npx"
        {
            " — npx was not found on the spawned process's $PATH. \
             Set CUARTEL_NPX_PATH=/absolute/path/to/npx, or run cuartel \
             from a shell where `which npx` succeeds (nvm-installed npx \
             often isn't visible to GUI-launched processes)"
        } else {
            ""
        };
        let command = format!("{}{hint}", opts.command);
        AcpError::Spawn { command, source }
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

    #[test]
    fn find_in_path_locates_a_universally_present_binary() {
        // `sh` exists on every Unix in /bin/sh; if it's not in PATH at
        // all, something's deeply wrong with the test environment.
        let sh = find_in_path("sh");
        assert!(sh.is_some(), "expected to find `sh` somewhere in $PATH");
        if let Some(p) = sh {
            assert!(p.is_file(), "resolved `sh` to non-file: {p:?}");
        }
    }

    #[test]
    fn find_in_path_returns_none_for_obvious_garbage() {
        // Vanishingly unlikely to exist anywhere.
        assert!(find_in_path("totally-not-a-real-binary-zzz").is_none());
    }

    #[test]
    fn build_inherited_path_prepends_extra_dirs() {
        let extra = std::path::Path::new("/opt/foo/bin");
        let combined = build_inherited_path(&[extra]);
        assert!(combined.starts_with("/opt/foo/bin"));
        // The original PATH (if set) should follow.
        if std::env::var("PATH").is_ok() {
            assert!(combined.contains(":"), "expected separator, got {combined}");
        }
    }

    #[tokio::test]
    async fn spawn_with_missing_cwd_errors_clearly_not_blaming_the_binary() {
        // posix_spawn returns ENOENT when cwd is missing but blames the
        // binary path — earned that one in production. Make sure the
        // friendly error fires before we even try to spawn.
        let opts = SpawnOptions {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "true".into()],
            cwd: PathBuf::from("/this/path/should/never/exist"),
            env: Vec::new(),
            clear_env: false,
        };
        let err = match spawn(&opts) {
            Ok(_) => panic!("expected spawn to fail with missing cwd"),
            Err(e) => e,
        };
        let msg = format!("{err}");
        assert!(
            msg.contains("/this/path/should/never/exist"),
            "error should name the missing cwd, got: {msg}",
        );
        assert!(
            msg.contains("does not exist"),
            "error should mention the cwd doesn't exist, got: {msg}",
        );
        // And critically — it should NOT make the binary look like the problem.
        assert!(
            !msg.contains("/bin/sh: No such file"),
            "error should not blame /bin/sh; got: {msg}",
        );
    }

    #[test]
    fn resolve_executable_honors_env_override_first() {
        // Use `sh` (universally present) as the override target via a
        // unique env var so we don't pollute anything else.
        let sh = find_in_path("sh").expect("sh in PATH for this test");
        std::env::set_var("CUARTEL_TEST_RESOLVE_OVERRIDE", &sh);
        let resolved =
            resolve_executable("totally-fake-name", Some("CUARTEL_TEST_RESOLVE_OVERRIDE"));
        assert_eq!(resolved.as_deref(), Some(sh.as_path()));
        std::env::remove_var("CUARTEL_TEST_RESOLVE_OVERRIDE");
    }
}
