//! Multi-pass tool-call name normalization.
//!
//! Different ACP server backends name the same conceptual tool
//! differently. claude-code-acp surfaces `Read` / `Bash` / `Edit` /
//! `Write` (PascalCase, capital initial). Other providers use
//! lowercase variants like `read` / `bash` / `shell`. Anthropic's
//! agent SDK occasionally emits internal aliases like `exec_command`.
//!
//! For the cuartel UI to render consistent icons, permission prompts,
//! and timeline entries, we collapse provider-specific names into a
//! canonical [`ToolKind`]. Providers that surface novel tools fall
//! through to [`ToolKind::Other`] with the original name preserved.
//!
//! Pattern lifted from Paseo's `packages/server/src/server/agent/providers/claude/tool-call-mapper.ts`
//! (KB §4.19.1) and the spike findings in KB §22.

/// Canonical kinds the cuartel UI knows how to render.
///
/// `#[non_exhaustive]` so we can add kinds without a breaking change to
/// downstream `match` arms in cuartel-app.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolKind {
    /// Shell / bash command execution.
    Shell,
    /// Reading a file's contents.
    Read,
    /// Writing a file (overwrite or create).
    Write,
    /// Editing a file (search-and-replace, patch).
    Edit,
    /// Searching the workspace (grep / regex).
    Search,
    /// Listing files (glob / find / ls).
    Glob,
    /// Fetching a URL (web fetch / web search / HTTP).
    Fetch,
    /// Spawning or controlling a subagent.
    SpawnAgent,
    /// MCP tool invocation (the actual MCP tool's identity is in the
    /// raw name; this kind says "this is an MCP tool of some sort").
    Mcp,
    /// Browser / Playwright control (computer use Tier 1).
    Browser,
    /// Desktop automation (computer use Tier 2 — xdotool / pyautogui).
    Desktop,
    /// Recognized as something but not categorized.
    Other(String),
}

impl ToolKind {
    /// Stable identifier for telemetry / config keys / icons.
    pub fn as_str(&self) -> &str {
        match self {
            ToolKind::Shell => "shell",
            ToolKind::Read => "read",
            ToolKind::Write => "write",
            ToolKind::Edit => "edit",
            ToolKind::Search => "search",
            ToolKind::Glob => "glob",
            ToolKind::Fetch => "fetch",
            ToolKind::SpawnAgent => "spawn_agent",
            ToolKind::Mcp => "mcp",
            ToolKind::Browser => "browser",
            ToolKind::Desktop => "desktop",
            ToolKind::Other(name) => name,
        }
    }
}

/// Collapse a provider-specific tool name into a canonical [`ToolKind`].
///
/// The pipeline is:
///   1. trim whitespace
///   2. lowercase a copy for matching (preserve original for `Other`)
///   3. exact-match lookup against known names per kind
///   4. prefix/contains heuristics for namespaced MCP tools (`mcp__foo__bar`)
///   5. fall through to `ToolKind::Other(original)`
pub fn normalize_tool_name(raw: &str) -> ToolKind {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();

    // MCP-namespaced tools come in shapes like `mcp__github__create_issue`
    // or `mcp::server::tool` depending on the provider. Match those before
    // the per-kind tables to avoid spurious classification.
    if lower.starts_with("mcp__") || lower.starts_with("mcp::") || lower.starts_with("mcp.") {
        return ToolKind::Mcp;
    }

    // Order matters when a name could plausibly belong to two kinds:
    // check more-specific kinds first.
    if SHELL_NAMES.iter().any(|n| *n == lower) {
        return ToolKind::Shell;
    }
    if EDIT_NAMES.iter().any(|n| *n == lower) {
        return ToolKind::Edit;
    }
    if WRITE_NAMES.iter().any(|n| *n == lower) {
        return ToolKind::Write;
    }
    if READ_NAMES.iter().any(|n| *n == lower) {
        return ToolKind::Read;
    }
    if SEARCH_NAMES.iter().any(|n| *n == lower) {
        return ToolKind::Search;
    }
    if GLOB_NAMES.iter().any(|n| *n == lower) {
        return ToolKind::Glob;
    }
    if FETCH_NAMES.iter().any(|n| *n == lower) {
        return ToolKind::Fetch;
    }
    if SPAWN_AGENT_NAMES.iter().any(|n| *n == lower) {
        return ToolKind::SpawnAgent;
    }
    if BROWSER_NAMES.iter().any(|n| *n == lower) || lower.starts_with("browser_") || lower.starts_with("playwright_") {
        return ToolKind::Browser;
    }
    if DESKTOP_NAMES.iter().any(|n| *n == lower) || lower.starts_with("desktop_") || lower.starts_with("xdo_") {
        return ToolKind::Desktop;
    }

    ToolKind::Other(trimmed.to_string())
}

const SHELL_NAMES: &[&str] = &[
    "bash",
    "shell",
    "exec",
    "exec_command",
    "execute_command",
    "run_command",
    "command",
    "terminal",
    "sh",
    "zsh",
];

const READ_NAMES: &[&str] = &[
    "read",
    "read_file",
    "readfile",
    "read_text_file",
    "view",
    "view_file",
    "cat",
    "open",
    "open_file",
];

const WRITE_NAMES: &[&str] = &[
    "write",
    "write_file",
    "writefile",
    "write_text_file",
    "create",
    "create_file",
    "save",
    "save_file",
];

const EDIT_NAMES: &[&str] = &[
    "edit",
    "edit_file",
    "patch",
    "patch_file",
    "multiedit",
    "multi_edit",
    "apply_patch",
    "str_replace",
    "str_replace_editor",
    "search_replace",
    "modify",
    "modify_file",
    "notebookedit",
    "notebook_edit",
];

const SEARCH_NAMES: &[&str] = &[
    "grep",
    "search",
    "search_files",
    "search_text",
    "ripgrep",
    "rg",
    "find_in_files",
    "code_search",
];

const GLOB_NAMES: &[&str] = &[
    "glob",
    "ls",
    "list",
    "list_files",
    "list_dir",
    "list_directory",
    "find",
    "ls_dir",
];

const FETCH_NAMES: &[&str] = &[
    "fetch",
    "webfetch",
    "web_fetch",
    "websearch",
    "web_search",
    "http_get",
    "url_fetch",
    "curl",
];

const SPAWN_AGENT_NAMES: &[&str] = &[
    "task",
    "subagent",
    "sub_agent",
    "spawn_agent",
    "delegate",
    "agent_spawn",
    "agent.spawn",
];

const BROWSER_NAMES: &[&str] = &[
    "browser",
    "navigate",
    "click",
    "type",
    "screenshot",
    "playwright",
    "browser_navigate",
    "browser_click",
];

const DESKTOP_NAMES: &[&str] = &[
    "desktop",
    "key",
    "mouse",
    "xdotool",
    "pyautogui",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The headline test: every variant of the shell-exec tool must
    /// collapse to `ToolKind::Shell`. This is the spike's single most
    /// important normalization invariant (see KB §22) and Paseo
    /// `tool-call-mapper.ts:113-168`.
    #[test]
    fn shell_variants_collapse_to_shell() {
        for raw in ["bash", "Bash", "BASH", "  bash  ", "shell", "Shell", "exec_command", "execute_command", "run_command"] {
            assert_eq!(
                normalize_tool_name(raw),
                ToolKind::Shell,
                "expected {raw:?} to normalize to Shell",
            );
        }
    }

    #[test]
    fn read_variants_collapse() {
        for raw in ["Read", "read", "read_file", "read_text_file", "view"] {
            assert_eq!(normalize_tool_name(raw), ToolKind::Read, "raw={raw:?}");
        }
    }

    #[test]
    fn edit_variants_collapse() {
        for raw in ["Edit", "edit", "MultiEdit", "apply_patch", "str_replace"] {
            assert_eq!(normalize_tool_name(raw), ToolKind::Edit, "raw={raw:?}");
        }
    }

    #[test]
    fn write_variants_collapse() {
        for raw in ["Write", "write", "create_file", "save_file"] {
            assert_eq!(normalize_tool_name(raw), ToolKind::Write, "raw={raw:?}");
        }
    }

    #[test]
    fn search_and_glob_distinct() {
        assert_eq!(normalize_tool_name("Grep"), ToolKind::Search);
        assert_eq!(normalize_tool_name("Glob"), ToolKind::Glob);
        assert_eq!(normalize_tool_name("ripgrep"), ToolKind::Search);
        assert_eq!(normalize_tool_name("ls"), ToolKind::Glob);
    }

    #[test]
    fn fetch_variants() {
        assert_eq!(normalize_tool_name("WebFetch"), ToolKind::Fetch);
        assert_eq!(normalize_tool_name("WebSearch"), ToolKind::Fetch);
    }

    #[test]
    fn spawn_agent_variants() {
        assert_eq!(normalize_tool_name("Task"), ToolKind::SpawnAgent);
        assert_eq!(normalize_tool_name("subagent"), ToolKind::SpawnAgent);
    }

    #[test]
    fn mcp_namespaced_tools_classify_as_mcp() {
        assert_eq!(normalize_tool_name("mcp__github__create_issue"), ToolKind::Mcp);
        assert_eq!(normalize_tool_name("MCP__foo__bar"), ToolKind::Mcp);
        assert_eq!(normalize_tool_name("mcp::server::tool"), ToolKind::Mcp);
    }

    #[test]
    fn browser_prefix_classifies_as_browser() {
        assert_eq!(normalize_tool_name("browser_navigate"), ToolKind::Browser);
        assert_eq!(normalize_tool_name("playwright_click"), ToolKind::Browser);
    }

    #[test]
    fn unknown_tools_preserve_original_name() {
        match normalize_tool_name("SomeNovelTool") {
            ToolKind::Other(name) => assert_eq!(name, "SomeNovelTool"),
            other => panic!("expected Other(SomeNovelTool), got {other:?}"),
        }
    }

    #[test]
    fn as_str_round_trips_canonical_kinds() {
        // Sanity: the as_str outputs are stable identifiers we can use in
        // telemetry. Adding a kind without updating as_str would compile
        // (Other has its own arm) but produce wrong telemetry; this test
        // ensures the well-known kinds at least each have a unique string.
        let kinds = [
            ToolKind::Shell, ToolKind::Read, ToolKind::Write, ToolKind::Edit,
            ToolKind::Search, ToolKind::Glob, ToolKind::Fetch,
            ToolKind::SpawnAgent, ToolKind::Mcp, ToolKind::Browser,
            ToolKind::Desktop,
        ];
        let strs: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
        let unique: std::collections::HashSet<&&str> = strs.iter().collect();
        assert_eq!(strs.len(), unique.len(), "duplicate as_str values: {strs:?}");
    }
}
