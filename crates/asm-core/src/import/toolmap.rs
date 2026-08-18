//! Tool-name mapping between agents' tool schemas.
//!
//! A tool_use naming a tool the receiving agent has never heard of is inert
//! history the model can still read — so unmapped names are KEPT verbatim
//! and reported, not dropped. Mapping the overlapping core just helps the
//! receiving model recognize what happened.

use crate::model::AgentKind;

/// (claude-code name, opencode name)
const CLAUDE_OPENCODE: &[(&str, &str)] = &[
    ("Bash", "bash"),
    ("Read", "read"),
    ("Edit", "edit"),
    ("MultiEdit", "edit"),
    ("Write", "write"),
    ("Glob", "glob"),
    ("Grep", "grep"),
    ("WebFetch", "webfetch"),
    ("WebSearch", "websearch"),
    ("TodoWrite", "todowrite"),
    ("Task", "task"),
    ("Agent", "task"),
];

/// Map `name` into `target`'s vocabulary. `None` = no mapping known (keep
/// the original name and report it).
pub fn map_tool(name: &str, target: AgentKind) -> Option<&'static str> {
    match target {
        AgentKind::OpenCode => {
            CLAUDE_OPENCODE.iter().find(|(claude, _)| *claude == name).map(|(_, oc)| *oc)
        }
        AgentKind::ClaudeCode => {
            CLAUDE_OPENCODE.iter().find(|(_, oc)| *oc == name).map(|(claude, _)| *claude)
        }
    }
}

pub fn table(target: AgentKind) -> Vec<(String, String)> {
    match target {
        AgentKind::OpenCode => {
            CLAUDE_OPENCODE.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
        }
        AgentKind::ClaudeCode => {
            CLAUDE_OPENCODE.iter().map(|(a, b)| (b.to_string(), a.to_string())).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_both_directions() {
        assert_eq!(map_tool("Bash", AgentKind::OpenCode), Some("bash"));
        assert_eq!(map_tool("MultiEdit", AgentKind::OpenCode), Some("edit"));
        // Reverse picks the first (canonical) name.
        assert_eq!(map_tool("edit", AgentKind::ClaudeCode), Some("Edit"));
        assert_eq!(map_tool("mcp__custom__thing", AgentKind::OpenCode), None);
    }
}
