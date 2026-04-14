//! Agent harness registry.
//!
//! A "harness" is the glue between cuartel's session lifecycle and a specific
//! agent implementation (Pi, Claude Code, Codex, OpenCode, ...). Each harness
//! describes how to install itself into a fresh VM, how to turn a prompt into
//! events that drive the session state machine, and which credentials it
//! needs injected at run time.
//!
//! This module intentionally stays free of Rivet / IO dependencies. Harnesses
//! describe *what* to run; the Phase 3f integration layer is responsible for
//! *actually* running it against a VM.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::session::SessionEvent;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentType {
    Pi,
    ClaudeCode,
    Codex,
    OpenCode,
    Custom(String),
}

impl AgentType {
    pub fn rivet_name(&self) -> &str {
        match self {
            AgentType::Pi => "pi",
            AgentType::ClaudeCode => "claude-code",
            AgentType::Codex => "codex",
            AgentType::OpenCode => "opencode",
            AgentType::Custom(name) => name,
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            AgentType::Pi => "Pi",
            AgentType::ClaudeCode => "Claude Code",
            AgentType::Codex => "OpenAI Codex",
            AgentType::OpenCode => "OpenCode",
            AgentType::Custom(name) => name,
        }
    }

    pub fn all_builtin() -> Vec<AgentType> {
        vec![
            AgentType::Pi,
            AgentType::ClaudeCode,
            AgentType::Codex,
            AgentType::OpenCode,
        ]
    }
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

/// A single step the harness wants the runner to execute inside the VM to
/// install or update itself. Kept declarative so the integration layer can
/// stream progress back to the UI and cache results between sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallStep {
    pub label: String,
    pub command: Vec<String>,
}

/// Shell command + stdin the runner should execute to kick off the agent
/// against a user prompt. Every harness boils down to "spawn this process,
/// feed it this input, read lines of output".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchCommand {
    pub program: String,
    pub args: Vec<String>,
    /// Prompt text piped to stdin. `None` means the prompt is already baked
    /// into `args` (rare, used by harnesses without a streaming stdin mode).
    pub stdin: Option<String>,
}

/// One raw chunk of output from a running agent. The harness is responsible
/// for turning harness-specific wire formats into this common enum so the
/// rest of the app can be written against a single event shape.
///
/// Note: the serde tag here is `kind` (cuartel's internal persistence /
/// bus format). Individual harnesses — Pi, Claude Code, etc. — receive
/// their own upstream wire formats (Pi uses `type`, for example) and must
/// translate into this enum in `parse_line`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum HarnessEvent {
    /// Plain text output to render in the terminal.
    Output(String),
    /// Agent wants to run a tool; UI should surface the permission prompt.
    /// `input` is the raw tool-call payload as structured JSON so downstream
    /// consumers (permission UI, audit log) can introspect fields without
    /// re-parsing a stringified blob.
    ToolUse {
        name: String,
        input: serde_json::Value,
    },
    /// Agent finished the current prompt successfully.
    Completed,
    /// Agent failed. Payload is a human readable reason.
    Failed(String),
}

impl HarnessEvent {
    /// Map a harness event onto the session state-machine event it should
    /// drive. Returns `None` for events that don't advance the state machine
    /// (pure output, tool-use notifications).
    pub fn to_session_event(&self) -> Option<SessionEvent> {
        match self {
            HarnessEvent::Completed => Some(SessionEvent::PromptCompleted),
            HarnessEvent::Failed(msg) => Some(SessionEvent::Failed(msg.clone())),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HarnessError {
    MissingEnv(String),
    ParseError(String),
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HarnessError::MissingEnv(k) => write!(f, "missing required environment variable: {k}"),
            HarnessError::ParseError(m) => write!(f, "unable to parse harness event: {m}"),
        }
    }
}

impl std::error::Error for HarnessError {}

/// Describes a concrete agent implementation. Implementations are pure:
/// they never do IO themselves — the Phase 3f integration layer calls into
/// them to learn *what* to execute.
pub trait AgentHarness: Send + Sync {
    fn agent_type(&self) -> AgentType;

    /// Env vars that must be present before `launch` can succeed. The
    /// integration layer pulls these from the credential store and injects
    /// them into the VM process.
    fn required_env_keys(&self) -> &'static [&'static str];

    /// Ordered steps to install the harness into a freshly booted VM.
    fn install_steps(&self) -> Vec<InstallStep>;

    /// Build the launch command for a given prompt. Validates that all
    /// `required_env_keys` are present in `env`.
    fn launch(
        &self,
        prompt: &str,
        env: &HashMap<String, String>,
    ) -> Result<LaunchCommand, HarnessError>;

    /// Parse one line of harness-specific output into a `HarnessEvent`.
    /// Returning `Ok(None)` means "no event for this line, keep reading".
    fn parse_line(&self, line: &str) -> Result<Option<HarnessEvent>, HarnessError>;
}

/// Lookup table for registered harnesses. Owns its harnesses behind `Arc`
/// so the app can share a single registry across UI and background tasks.
#[derive(Clone, Default)]
pub struct HarnessRegistry {
    harnesses: HashMap<AgentType, Arc<dyn AgentHarness>>,
}

impl HarnessRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry pre-populated with every built-in harness cuartel ships with.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(PiHarness));
        r.register(Arc::new(ClaudeCodeHarness));
        r.register(Arc::new(CodexHarness));
        r.register(Arc::new(OpenCodeHarness));
        r
    }

    pub fn register(&mut self, harness: Arc<dyn AgentHarness>) {
        self.harnesses.insert(harness.agent_type(), harness);
    }

    pub fn get(&self, agent: &AgentType) -> Option<Arc<dyn AgentHarness>> {
        self.harnesses.get(agent).cloned()
    }

    pub fn contains(&self, agent: &AgentType) -> bool {
        self.harnesses.contains_key(agent)
    }

    pub fn registered(&self) -> Vec<AgentType> {
        self.harnesses.keys().cloned().collect()
    }
}

impl std::fmt::Debug for HarnessRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessRegistry")
            .field("registered", &self.registered())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Pi harness
// ---------------------------------------------------------------------------

/// Pi is cuartel's default harness: a thin `pi` CLI that speaks JSON Lines
/// over stdout. Each line is a JSON object with a `type` discriminator that
/// maps onto `HarnessEvent`.
pub struct PiHarness;

const PI_REQUIRED_ENV: &[&str] = &["ANTHROPIC_API_KEY"];

impl AgentHarness for PiHarness {
    fn agent_type(&self) -> AgentType {
        AgentType::Pi
    }

    fn required_env_keys(&self) -> &'static [&'static str] {
        PI_REQUIRED_ENV
    }

    fn install_steps(&self) -> Vec<InstallStep> {
        // SECURITY: the bootstrap uses `curl | sh`, which is a known
        // supply-chain vector. It is acceptable here because the script is
        // served over TLS from a domain we control and runs inside a
        // throwaway VM — but any change to the install URL or runner host
        // must re-audit this. Production builds should eventually pin a
        // checksum and verify it before executing.
        vec![
            InstallStep {
                label: "Install Pi CLI".into(),
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    "curl -fsSL https://pi.cuartel.dev/install.sh | sh".into(),
                ],
            },
            InstallStep {
                label: "Verify Pi CLI".into(),
                command: vec!["pi".into(), "--version".into()],
            },
        ]
    }

    fn launch(
        &self,
        prompt: &str,
        env: &HashMap<String, String>,
    ) -> Result<LaunchCommand, HarnessError> {
        for key in self.required_env_keys() {
            if !env.contains_key(*key) {
                return Err(HarnessError::MissingEnv((*key).into()));
            }
        }
        Ok(LaunchCommand {
            program: "pi".into(),
            args: vec!["run".into(), "--format".into(), "jsonl".into()],
            stdin: Some(prompt.to_string()),
        })
    }

    fn parse_line(&self, line: &str) -> Result<Option<HarnessEvent>, HarnessError> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| HarnessError::ParseError(e.to_string()))?;
        let kind = value
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HarnessError::ParseError("missing `type` field".into()))?;
        let event = match kind {
            "output" => {
                let text = value
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                HarnessEvent::Output(text)
            }
            "tool_use" => {
                let name = value
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = value
                    .get("input")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                HarnessEvent::ToolUse { name, input }
            }
            "completed" => HarnessEvent::Completed,
            "error" => {
                let msg = value
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown pi error")
                    .to_string();
                HarnessEvent::Failed(msg)
            }
            other => {
                return Err(HarnessError::ParseError(format!(
                    "unknown pi event type `{other}`"
                )))
            }
        };
        Ok(Some(event))
    }
}

// ---------------------------------------------------------------------------
// Claude Code harness
// ---------------------------------------------------------------------------

/// Anthropic's `claude` CLI (`@anthropic-ai/claude-code`). Driven via
/// `-p <prompt> --output-format stream-json --verbose`, which emits one JSON
/// envelope per line. `assistant` envelopes carry a `message.content` array
/// of typed blocks; in non-interactive `-p` mode the CLI streams one block
/// per envelope, so we translate the first meaningful block we see.
pub struct ClaudeCodeHarness;

const CLAUDE_CODE_REQUIRED_ENV: &[&str] = &["ANTHROPIC_API_KEY"];

impl AgentHarness for ClaudeCodeHarness {
    fn agent_type(&self) -> AgentType {
        AgentType::ClaudeCode
    }

    fn required_env_keys(&self) -> &'static [&'static str] {
        CLAUDE_CODE_REQUIRED_ENV
    }

    fn install_steps(&self) -> Vec<InstallStep> {
        vec![
            InstallStep {
                label: "Install Claude Code".into(),
                command: vec![
                    "npm".into(),
                    "install".into(),
                    "-g".into(),
                    "@anthropic-ai/claude-code".into(),
                ],
            },
            InstallStep {
                label: "Verify Claude Code".into(),
                command: vec!["claude".into(), "--version".into()],
            },
        ]
    }

    fn launch(
        &self,
        prompt: &str,
        env: &HashMap<String, String>,
    ) -> Result<LaunchCommand, HarnessError> {
        for key in self.required_env_keys() {
            if !env.contains_key(*key) {
                return Err(HarnessError::MissingEnv((*key).into()));
            }
        }
        Ok(LaunchCommand {
            program: "claude".into(),
            args: vec![
                "-p".into(),
                prompt.into(),
                "--output-format".into(),
                "stream-json".into(),
                "--verbose".into(),
            ],
            stdin: None,
        })
    }

    fn parse_line(&self, line: &str) -> Result<Option<HarnessEvent>, HarnessError> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| HarnessError::ParseError(e.to_string()))?;
        let kind = value
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HarnessError::ParseError("missing `type` field".into()))?;
        Ok(match kind {
            // Handshake + echoed user turns carry no output for the terminal.
            "system" | "user" => None,
            "assistant" => value
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
                .and_then(|blocks| blocks.iter().find_map(claude_code_block_to_event)),
            "result" => {
                let subtype = value
                    .get("subtype")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if subtype == "success" {
                    Some(HarnessEvent::Completed)
                } else {
                    let msg = value
                        .get("error")
                        .and_then(|v| v.as_str())
                        .or_else(|| value.get("result").and_then(|v| v.as_str()))
                        .unwrap_or("claude code error")
                        .to_string();
                    Some(HarnessEvent::Failed(msg))
                }
            }
            other => {
                return Err(HarnessError::ParseError(format!(
                    "unknown claude code event type `{other}`"
                )))
            }
        })
    }
}

fn claude_code_block_to_event(block: &serde_json::Value) -> Option<HarnessEvent> {
    let btype = block.get("type").and_then(|v| v.as_str())?;
    match btype {
        "text" => {
            let text = block
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(HarnessEvent::Output(text))
        }
        "tool_use" => {
            let name = block
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let input = block
                .get("input")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Some(HarnessEvent::ToolUse { name, input })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Codex harness
// ---------------------------------------------------------------------------

/// OpenAI's `codex` CLI (`@openai/codex`). Invoked as `codex exec --json
/// <prompt>`, emitting JSONL envelopes shaped `{id, msg: {type, ...}}`.
pub struct CodexHarness;

const CODEX_REQUIRED_ENV: &[&str] = &["OPENAI_API_KEY"];

impl AgentHarness for CodexHarness {
    fn agent_type(&self) -> AgentType {
        AgentType::Codex
    }

    fn required_env_keys(&self) -> &'static [&'static str] {
        CODEX_REQUIRED_ENV
    }

    fn install_steps(&self) -> Vec<InstallStep> {
        vec![
            InstallStep {
                label: "Install Codex CLI".into(),
                command: vec![
                    "npm".into(),
                    "install".into(),
                    "-g".into(),
                    "@openai/codex".into(),
                ],
            },
            InstallStep {
                label: "Verify Codex CLI".into(),
                command: vec!["codex".into(), "--version".into()],
            },
        ]
    }

    fn launch(
        &self,
        prompt: &str,
        env: &HashMap<String, String>,
    ) -> Result<LaunchCommand, HarnessError> {
        for key in self.required_env_keys() {
            if !env.contains_key(*key) {
                return Err(HarnessError::MissingEnv((*key).into()));
            }
        }
        Ok(LaunchCommand {
            program: "codex".into(),
            args: vec!["exec".into(), "--json".into(), prompt.into()],
            stdin: None,
        })
    }

    fn parse_line(&self, line: &str) -> Result<Option<HarnessEvent>, HarnessError> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| HarnessError::ParseError(e.to_string()))?;
        let Some(msg) = value.get("msg") else {
            return Ok(None);
        };
        let kind = msg
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HarnessError::ParseError("missing `msg.type` field".into()))?;
        Ok(match kind {
            "agent_message" | "agent_message_delta" => {
                let text = msg
                    .get("message")
                    .or_else(|| msg.get("delta"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(HarnessEvent::Output(text))
            }
            "exec_command_begin" => {
                let command = msg
                    .get("command")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                Some(HarnessEvent::ToolUse {
                    name: "exec".into(),
                    input: command,
                })
            }
            "task_complete" => Some(HarnessEvent::Completed),
            "error" => {
                let m = msg
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("codex error")
                    .to_string();
                Some(HarnessEvent::Failed(m))
            }
            // Lifecycle events (task_started, token_count, ...) carry no
            // terminal output; skip them silently rather than erroring.
            _ => None,
        })
    }
}

// ---------------------------------------------------------------------------
// OpenCode harness
// ---------------------------------------------------------------------------

/// `opencode` CLI (opencode.ai). Invoked as `opencode run --json <prompt>`;
/// emits JSONL lines discriminated by an `event` field.
pub struct OpenCodeHarness;

const OPENCODE_REQUIRED_ENV: &[&str] = &["ANTHROPIC_API_KEY"];

impl AgentHarness for OpenCodeHarness {
    fn agent_type(&self) -> AgentType {
        AgentType::OpenCode
    }

    fn required_env_keys(&self) -> &'static [&'static str] {
        OPENCODE_REQUIRED_ENV
    }

    fn install_steps(&self) -> Vec<InstallStep> {
        // SECURITY: same `curl | sh` caveat as Pi — acceptable inside a
        // throwaway VM, re-audit if the install host ever changes.
        vec![
            InstallStep {
                label: "Install OpenCode".into(),
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    "curl -fsSL https://opencode.ai/install | bash".into(),
                ],
            },
            InstallStep {
                label: "Verify OpenCode".into(),
                command: vec!["opencode".into(), "--version".into()],
            },
        ]
    }

    fn launch(
        &self,
        prompt: &str,
        env: &HashMap<String, String>,
    ) -> Result<LaunchCommand, HarnessError> {
        for key in self.required_env_keys() {
            if !env.contains_key(*key) {
                return Err(HarnessError::MissingEnv((*key).into()));
            }
        }
        Ok(LaunchCommand {
            program: "opencode".into(),
            args: vec!["run".into(), "--json".into(), prompt.into()],
            stdin: None,
        })
    }

    fn parse_line(&self, line: &str) -> Result<Option<HarnessEvent>, HarnessError> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| HarnessError::ParseError(e.to_string()))?;
        let event = value
            .get("event")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HarnessError::ParseError("missing `event` field".into()))?;
        Ok(match event {
            "message" => {
                let text = value
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(HarnessEvent::Output(text))
            }
            "tool" => {
                let name = value
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = value
                    .get("input")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                Some(HarnessEvent::ToolUse { name, input })
            }
            "done" => Some(HarnessEvent::Completed),
            "error" => {
                let m = value
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("opencode error")
                    .to_string();
                Some(HarnessEvent::Failed(m))
            }
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with_key() -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert("ANTHROPIC_API_KEY".into(), "sk-test".into());
        env
    }

    fn env_with(key: &str) -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert(key.into(), "sk-test".into());
        env
    }

    #[test]
    fn registry_with_builtins_contains_all_shipped_harnesses() {
        let r = HarnessRegistry::with_builtins();
        for t in AgentType::all_builtin() {
            assert!(r.contains(&t), "missing builtin harness: {t:?}");
            let h = r.get(&t).expect("harness registered");
            assert_eq!(h.agent_type(), t);
        }
    }

    #[test]
    fn registry_register_and_lookup_custom() {
        struct Dummy;
        impl AgentHarness for Dummy {
            fn agent_type(&self) -> AgentType {
                AgentType::Custom("dummy".into())
            }
            fn required_env_keys(&self) -> &'static [&'static str] {
                &[]
            }
            fn install_steps(&self) -> Vec<InstallStep> {
                vec![]
            }
            fn launch(
                &self,
                _prompt: &str,
                _env: &HashMap<String, String>,
            ) -> Result<LaunchCommand, HarnessError> {
                Ok(LaunchCommand {
                    program: "true".into(),
                    args: vec![],
                    stdin: None,
                })
            }
            fn parse_line(&self, _line: &str) -> Result<Option<HarnessEvent>, HarnessError> {
                Ok(None)
            }
        }
        let mut r = HarnessRegistry::new();
        r.register(Arc::new(Dummy));
        assert!(r.contains(&AgentType::Custom("dummy".into())));
        assert_eq!(r.registered().len(), 1);
    }

    #[test]
    fn pi_launch_requires_api_key() {
        let pi = PiHarness;
        let empty = HashMap::new();
        let err = pi.launch("hello", &empty).unwrap_err();
        assert!(matches!(err, HarnessError::MissingEnv(ref k) if k == "ANTHROPIC_API_KEY"));
    }

    #[test]
    fn pi_launch_with_key_produces_command_and_stdin() {
        let pi = PiHarness;
        let cmd = pi.launch("do the thing", &env_with_key()).unwrap();
        assert_eq!(cmd.program, "pi");
        assert_eq!(cmd.args, vec!["run", "--format", "jsonl"]);
        assert_eq!(cmd.stdin.as_deref(), Some("do the thing"));
    }

    #[test]
    fn pi_install_steps_are_ordered_and_nonempty() {
        let steps = PiHarness.install_steps();
        assert!(!steps.is_empty());
        assert_eq!(steps[0].label, "Install Pi CLI");
        assert!(steps[0].command.first().map(|s| s.as_str()) == Some("sh"));
    }

    #[test]
    fn pi_parse_output_event() {
        let ev = PiHarness
            .parse_line(r#"{"type":"output","text":"hello"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev, HarnessEvent::Output("hello".into()));
    }

    #[test]
    fn pi_parse_tool_use_event() {
        let ev = PiHarness
            .parse_line(r#"{"type":"tool_use","name":"bash","input":{"cmd":"ls"}}"#)
            .unwrap()
            .unwrap();
        match ev {
            HarnessEvent::ToolUse { name, input } => {
                assert_eq!(name, "bash");
                assert_eq!(input.get("cmd").and_then(|v| v.as_str()), Some("ls"));
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn pi_parse_completed_event() {
        let ev = PiHarness
            .parse_line(r#"{"type":"completed"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev, HarnessEvent::Completed);
        assert_eq!(ev.to_session_event(), Some(SessionEvent::PromptCompleted));
    }

    #[test]
    fn pi_parse_error_event_maps_to_failed_session_event() {
        let ev = PiHarness
            .parse_line(r#"{"type":"error","message":"boom"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev, HarnessEvent::Failed("boom".into()));
        assert_eq!(
            ev.to_session_event(),
            Some(SessionEvent::Failed("boom".into()))
        );
    }

    #[test]
    fn pi_parse_blank_line_yields_no_event() {
        assert!(PiHarness.parse_line("   ").unwrap().is_none());
    }

    #[test]
    fn pi_parse_invalid_json_returns_parse_error() {
        let err = PiHarness.parse_line("not json").unwrap_err();
        assert!(matches!(err, HarnessError::ParseError(_)));
    }

    #[test]
    fn pi_parse_unknown_type_returns_parse_error() {
        let err = PiHarness
            .parse_line(r#"{"type":"bogus"}"#)
            .unwrap_err();
        assert!(matches!(err, HarnessError::ParseError(_)));
    }

    // ----- Claude Code -----

    #[test]
    fn claude_code_launch_requires_api_key() {
        let err = ClaudeCodeHarness
            .launch("hi", &HashMap::new())
            .unwrap_err();
        assert!(matches!(err, HarnessError::MissingEnv(ref k) if k == "ANTHROPIC_API_KEY"));
    }

    #[test]
    fn claude_code_launch_builds_stream_json_command() {
        let cmd = ClaudeCodeHarness
            .launch("do it", &env_with_key())
            .unwrap();
        assert_eq!(cmd.program, "claude");
        assert_eq!(
            cmd.args,
            vec!["-p", "do it", "--output-format", "stream-json", "--verbose"]
        );
        assert!(cmd.stdin.is_none());
    }

    #[test]
    fn claude_code_parse_assistant_text_block() {
        let ev = ClaudeCodeHarness
            .parse_line(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#,
            )
            .unwrap()
            .unwrap();
        assert_eq!(ev, HarnessEvent::Output("hi".into()));
    }

    #[test]
    fn claude_code_parse_assistant_tool_use_block() {
        let ev = ClaudeCodeHarness
            .parse_line(
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
            )
            .unwrap()
            .unwrap();
        match ev {
            HarnessEvent::ToolUse { name, input } => {
                assert_eq!(name, "Bash");
                assert_eq!(
                    input.get("command").and_then(|v| v.as_str()),
                    Some("ls")
                );
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn claude_code_parse_system_init_yields_no_event() {
        assert!(ClaudeCodeHarness
            .parse_line(r#"{"type":"system","subtype":"init"}"#)
            .unwrap()
            .is_none());
    }

    #[test]
    fn claude_code_parse_result_success_completes() {
        let ev = ClaudeCodeHarness
            .parse_line(r#"{"type":"result","subtype":"success","result":"done"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev, HarnessEvent::Completed);
    }

    #[test]
    fn claude_code_parse_result_error_fails() {
        let ev = ClaudeCodeHarness
            .parse_line(r#"{"type":"result","subtype":"error","error":"rate limited"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev, HarnessEvent::Failed("rate limited".into()));
    }

    #[test]
    fn claude_code_parse_unknown_type_errors() {
        let err = ClaudeCodeHarness
            .parse_line(r#"{"type":"bogus"}"#)
            .unwrap_err();
        assert!(matches!(err, HarnessError::ParseError(_)));
    }

    #[test]
    fn claude_code_install_steps_use_npm() {
        let steps = ClaudeCodeHarness.install_steps();
        assert_eq!(steps[0].command[0], "npm");
        assert!(steps[0].command.contains(&"@anthropic-ai/claude-code".to_string()));
    }

    // ----- Codex -----

    #[test]
    fn codex_launch_requires_openai_key() {
        let err = CodexHarness.launch("hi", &HashMap::new()).unwrap_err();
        assert!(matches!(err, HarnessError::MissingEnv(ref k) if k == "OPENAI_API_KEY"));
    }

    #[test]
    fn codex_launch_builds_exec_json_command() {
        let cmd = CodexHarness
            .launch("ship it", &env_with("OPENAI_API_KEY"))
            .unwrap();
        assert_eq!(cmd.program, "codex");
        assert_eq!(cmd.args, vec!["exec", "--json", "ship it"]);
    }

    #[test]
    fn codex_parse_agent_message() {
        let ev = CodexHarness
            .parse_line(r#"{"id":"1","msg":{"type":"agent_message","message":"hello"}}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev, HarnessEvent::Output("hello".into()));
    }

    #[test]
    fn codex_parse_exec_command_begin_is_tool_use() {
        let ev = CodexHarness
            .parse_line(
                r#"{"id":"2","msg":{"type":"exec_command_begin","command":["ls","-la"]}}"#,
            )
            .unwrap()
            .unwrap();
        match ev {
            HarnessEvent::ToolUse { name, input } => {
                assert_eq!(name, "exec");
                assert_eq!(
                    input.as_array().map(|a| a.len()),
                    Some(2)
                );
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn codex_parse_task_complete() {
        let ev = CodexHarness
            .parse_line(r#"{"id":"3","msg":{"type":"task_complete"}}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev, HarnessEvent::Completed);
    }

    #[test]
    fn codex_parse_error_event() {
        let ev = CodexHarness
            .parse_line(r#"{"id":"4","msg":{"type":"error","message":"boom"}}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev, HarnessEvent::Failed("boom".into()));
    }

    #[test]
    fn codex_parse_unknown_lifecycle_event_is_noop() {
        assert!(CodexHarness
            .parse_line(r#"{"id":"5","msg":{"type":"token_count","total":42}}"#)
            .unwrap()
            .is_none());
    }

    #[test]
    fn codex_parse_envelope_without_msg_is_noop() {
        assert!(CodexHarness
            .parse_line(r#"{"id":"6"}"#)
            .unwrap()
            .is_none());
    }

    // ----- OpenCode -----

    #[test]
    fn opencode_launch_requires_api_key() {
        let err = OpenCodeHarness.launch("hi", &HashMap::new()).unwrap_err();
        assert!(matches!(err, HarnessError::MissingEnv(ref k) if k == "ANTHROPIC_API_KEY"));
    }

    #[test]
    fn opencode_launch_builds_run_json_command() {
        let cmd = OpenCodeHarness
            .launch("fix it", &env_with_key())
            .unwrap();
        assert_eq!(cmd.program, "opencode");
        assert_eq!(cmd.args, vec!["run", "--json", "fix it"]);
    }

    #[test]
    fn opencode_parse_message_event() {
        let ev = OpenCodeHarness
            .parse_line(r#"{"event":"message","text":"hi"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev, HarnessEvent::Output("hi".into()));
    }

    #[test]
    fn opencode_parse_tool_event() {
        let ev = OpenCodeHarness
            .parse_line(r#"{"event":"tool","name":"edit","input":{"path":"a.rs"}}"#)
            .unwrap()
            .unwrap();
        match ev {
            HarnessEvent::ToolUse { name, input } => {
                assert_eq!(name, "edit");
                assert_eq!(input.get("path").and_then(|v| v.as_str()), Some("a.rs"));
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn opencode_parse_done_completes() {
        let ev = OpenCodeHarness
            .parse_line(r#"{"event":"done"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev, HarnessEvent::Completed);
    }

    #[test]
    fn opencode_parse_error_event() {
        let ev = OpenCodeHarness
            .parse_line(r#"{"event":"error","message":"nope"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev, HarnessEvent::Failed("nope".into()));
    }

    #[test]
    fn opencode_parse_missing_event_field_errors() {
        let err = OpenCodeHarness.parse_line(r#"{"foo":"bar"}"#).unwrap_err();
        assert!(matches!(err, HarnessError::ParseError(_)));
    }

    #[test]
    fn opencode_install_uses_curl_script() {
        let steps = OpenCodeHarness.install_steps();
        assert_eq!(steps[0].command[0], "sh");
        assert!(steps[0].command[2].contains("opencode.ai/install"));
    }

    #[test]
    fn output_events_do_not_advance_state_machine() {
        let ev = HarnessEvent::Output("noise".into());
        assert!(ev.to_session_event().is_none());
        let ev = HarnessEvent::ToolUse {
            name: "bash".into(),
            input: serde_json::json!({}),
        };
        assert!(ev.to_session_event().is_none());
    }
}
