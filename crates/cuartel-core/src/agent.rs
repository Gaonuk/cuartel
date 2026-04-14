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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum HarnessEvent {
    /// Plain text output to render in the terminal.
    Output(String),
    /// Agent wants to run a tool; UI should surface the permission prompt.
    ToolUse { name: String, input: String },
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
    /// Today that's just Pi; 3h will add Claude Code / Codex / OpenCode.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(PiHarness));
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
        for key in PI_REQUIRED_ENV {
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
                    .map(|v| v.to_string())
                    .unwrap_or_default();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with_key() -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert("ANTHROPIC_API_KEY".into(), "sk-test".into());
        env
    }

    #[test]
    fn registry_with_builtins_contains_pi() {
        let r = HarnessRegistry::with_builtins();
        assert!(r.contains(&AgentType::Pi));
        let pi = r.get(&AgentType::Pi).expect("pi harness registered");
        assert_eq!(pi.agent_type(), AgentType::Pi);
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
                assert!(input.contains("ls"));
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

    #[test]
    fn output_events_do_not_advance_state_machine() {
        let ev = HarnessEvent::Output("noise".into());
        assert!(ev.to_session_event().is_none());
        let ev = HarnessEvent::ToolUse {
            name: "bash".into(),
            input: "{}".into(),
        };
        assert!(ev.to_session_event().is_none());
    }
}
