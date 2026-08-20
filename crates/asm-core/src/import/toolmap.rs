//! Tool-name mapping between agents' tool schemas.
//!
//! A tool_use naming a tool the receiving agent has never heard of is inert
//! history the model can still read — so unmapped names are KEPT verbatim
//! and reported, not dropped. Mapping the overlapping core just helps the
//! receiving model recognize what happened.

use crate::model::AgentKind;

/// One row per concept, with each agent's name for it. `None` means that
/// agent has no equivalent, so a call from elsewhere keeps its own name.
///
/// Only genuinely equivalent tools are listed. Claude Code's `Grep` and
/// jcode's `agentgrep` are both "search the code", for instance, but the
/// latter delegates to an agent rather than running a regex, so mapping
/// them would misrepresent what happened.
struct Row {
    claude: Option<&'static str>,
    opencode: Option<&'static str>,
    jcode: Option<&'static str>,
}

const TABLE: &[Row] = &[
    row(Some("Bash"), Some("bash"), Some("bash")),
    row(Some("Read"), Some("read"), Some("read")),
    row(Some("Edit"), Some("edit"), Some("edit")),
    row(Some("MultiEdit"), Some("edit"), Some("multiedit")),
    row(Some("Write"), Some("write"), Some("write")),
    row(Some("Glob"), Some("glob"), None),
    row(Some("Grep"), Some("grep"), None),
    row(Some("WebFetch"), Some("webfetch"), Some("webfetch")),
    row(Some("WebSearch"), Some("websearch"), Some("websearch")),
    row(Some("TodoWrite"), Some("todowrite"), Some("todo")),
    row(Some("Task"), Some("task"), None),
    row(Some("Agent"), Some("task"), None),
];

const fn row(
    claude: Option<&'static str>,
    opencode: Option<&'static str>,
    jcode: Option<&'static str>,
) -> Row {
    Row { claude, opencode, jcode }
}

fn name_for(row: &Row, agent: AgentKind) -> Option<&'static str> {
    match agent {
        AgentKind::ClaudeCode => row.claude,
        AgentKind::OpenCode => row.opencode,
        AgentKind::JCode => row.jcode,
        // Codex is read-only, so nothing is ever mapped *into* its
        // vocabulary; its tool names still resolve on the way out because
        // `map_tool` matches the name against every column.
        AgentKind::Codex => None,
        AgentKind::Antigravity => None,
    }
}

/// Map `name` into `target`'s vocabulary. `None` = no mapping known (keep
/// the original name and report it).
pub fn map_tool(name: &str, target: AgentKind) -> Option<&'static str> {
    // The source agent is not known here, so match the name against every
    // column: tool names are distinctive enough that a collision across
    // agents would mean the same concept anyway.
    TABLE
        .iter()
        .find(|row| {
            [row.claude, row.opencode, row.jcode].contains(&Some(name))
        })
        .and_then(|row| name_for(row, target))
        .filter(|mapped| *mapped != name)
}

/// The mapping table as seen from `target`, for `--show-toolmap`.
pub fn table(target: AgentKind) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for row in TABLE {
        let Some(to) = name_for(row, target) else { continue };
        for from in [row.claude, row.opencode, row.jcode].into_iter().flatten() {
            if from != to {
                pairs.push((from.to_string(), to.to_string()));
            }
        }
    }
    pairs.sort();
    pairs.dedup();
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_between_every_pair() {
        assert_eq!(map_tool("Bash", AgentKind::OpenCode), Some("bash"));
        assert_eq!(map_tool("Bash", AgentKind::JCode), Some("bash"));
        assert_eq!(map_tool("multiedit", AgentKind::ClaudeCode), Some("MultiEdit"));
        assert_eq!(map_tool("todo", AgentKind::ClaudeCode), Some("TodoWrite"));
        assert_eq!(map_tool("TodoWrite", AgentKind::JCode), Some("todo"));
    }

    #[test]
    fn a_name_the_target_already_uses_is_not_remapped() {
        assert_eq!(map_tool("bash", AgentKind::OpenCode), None);
        assert_eq!(map_tool("read", AgentKind::JCode), None);
    }

    #[test]
    fn tools_without_an_equivalent_keep_their_name() {
        // jcode has no Task/Glob/Grep equivalent worth claiming.
        assert_eq!(map_tool("Task", AgentKind::JCode), None);
        assert_eq!(map_tool("Grep", AgentKind::JCode), None);
        assert_eq!(map_tool("mcp__custom__thing", AgentKind::OpenCode), None);
    }
}
