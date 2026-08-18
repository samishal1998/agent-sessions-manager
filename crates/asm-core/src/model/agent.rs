use serde::{Deserialize, Serialize};

/// The coding agents this tool knows about. `#[non_exhaustive]` because the
/// set will grow (Codex, Gemini CLI, Cursor, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum AgentKind {
    ClaudeCode,
    // serde must agree with Display/parse: "opencode", not "open-code".
    #[serde(rename = "opencode", alias = "open-code")]
    OpenCode,
    #[serde(rename = "jcode")]
    JCode,
}

impl AgentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::OpenCode => "opencode",
            AgentKind::JCode => "jcode",
        }
    }

    /// Parse a user-supplied agent name, accepting a few natural aliases.
    pub fn parse(s: &str) -> Option<AgentKind> {
        match s.to_ascii_lowercase().as_str() {
            "claude-code" | "claude" | "claudecode" | "cc" => Some(AgentKind::ClaudeCode),
            "opencode" | "open-code" | "oc" => Some(AgentKind::OpenCode),
            "jcode" | "jc" => Some(AgentKind::JCode),
            _ => None,
        }
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
