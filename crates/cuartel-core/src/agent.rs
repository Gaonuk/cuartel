use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    pub fn required_env_keys(&self) -> Vec<&str> {
        match self {
            AgentType::Pi => vec!["ANTHROPIC_API_KEY"],
            AgentType::ClaudeCode => vec!["ANTHROPIC_API_KEY"],
            AgentType::Codex => vec!["OPENAI_API_KEY"],
            AgentType::OpenCode => vec!["ANTHROPIC_API_KEY"],
            AgentType::Custom(_) => vec![],
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
