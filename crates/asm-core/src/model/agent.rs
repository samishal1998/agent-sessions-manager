use serde::{Deserialize, Serialize};

/// The coding agents this tool knows about. `#[non_exhaustive]` because the
/// set will grow (Cursor, Copilot, ...).
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
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "antigravity")]
    Antigravity,
    /// Pi (earendil-works/pi), the `pi` command.
    #[serde(rename = "pi")]
    Pi,
    /// oh-my-pi, a fork of Pi with its own store, the `omp` command.
    #[serde(rename = "omp")]
    Omp,
}

impl AgentKind {
    /// Every agent, in the order listings and reports show them.
    pub const ALL: [AgentKind; 7] = [
        AgentKind::ClaudeCode,
        AgentKind::OpenCode,
        AgentKind::JCode,
        AgentKind::Codex,
        AgentKind::Antigravity,
        AgentKind::Pi,
        AgentKind::Omp,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::OpenCode => "opencode",
            AgentKind::JCode => "jcode",
            AgentKind::Codex => "codex",
            AgentKind::Antigravity => "antigravity",
            AgentKind::Pi => "pi",
            AgentKind::Omp => "omp",
        }
    }

    /// Parse a user-supplied agent name, accepting a few natural aliases.
    pub fn parse(s: &str) -> Option<AgentKind> {
        match s.to_ascii_lowercase().as_str() {
            "claude-code" | "claude" | "claudecode" | "cc" => Some(AgentKind::ClaudeCode),
            "opencode" | "open-code" | "oc" => Some(AgentKind::OpenCode),
            "jcode" | "jc" => Some(AgentKind::JCode),
            "codex" | "codex-cli" | "cx" => Some(AgentKind::Codex),
            // `agy` is the command the CLI installs, so accept it as a name.
            "antigravity" | "agy" | "ag" => Some(AgentKind::Antigravity),
            "pi" | "pi-coding-agent" => Some(AgentKind::Pi),
            "omp" | "oh-my-pi" | "ohmypi" => Some(AgentKind::Omp),
            _ => None,
        }
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
