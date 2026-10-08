//! The tested-versions matrix. Full-mode import writes native records, so
//! it is only as safe as the agent versions it was verified against; drift
//! produces a warning, never a silent assumption.

use crate::model::AgentKind;

/// (agent, exact version asm's writes were last verified against). An agent
/// may have several entries, one per major version it is supported at: a
/// 2.x install is judged against the 2.x entry, not the 1.x one.
///
/// "Verified" never means the code compiled, and it is not the same thing for
/// every entry (the comment on each says which):
/// - Claude Code and OpenCode 1.x: a real session was imported and then
///   resumed in that agent's own CLI with its conversation intact.
/// - OpenCode 2.x: the import was accepted by the real 2.0.25 binary and the
///   session read back from its store the way `session export` prints it.
///   Resuming an imported session was NOT exercised (it would send a model a
///   prompt).
/// - jcode is not an import target, so there it means the write verbs were
///   exercised against a real store and the agent's own view was checked
///   afterwards: rename read back through `jcode session rename --json`, and
///   archive/delete confirmed by jcode no longer being able to resolve the
///   session.
pub const TESTED: &[(AgentKind, &str)] = &[
    (AgentKind::ClaudeCode, "2.1.234"),
    (AgentKind::OpenCode, "1.17.18"),
    // Import accepted by 2.0.25 and read back; resuming it was not exercised.
    (AgentKind::OpenCode, "2.0.25"),
    (AgentKind::JCode, "0.78.0"),
    // Read-only, so "verified" means listed and exported: a 0.151.0 rollout
    // and threads row read correctly. 0.151 also records `cwd` in
    // turn_context, world_state and the environment message, not only on the
    // first line — relevant to anything that would ever write one.
    (AgentKind::Codex, "0.151.0"),
    // `agy version` reports nothing parseable yet; pinned by observation.
    (AgentKind::Antigravity, "1.1.16"),
];

fn major(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

/// The version `agent` was tested at: the entry whose major version is the
/// installed one's, else the first (oldest listed) entry.
pub fn tested_version_for(agent: AgentKind, installed: Option<&str>) -> Option<&'static str> {
    let mut entries = TESTED.iter().filter(|(a, _)| *a == agent).map(|(_, v)| *v);
    let first = entries.clone().next();
    installed.and_then(|i| entries.find(|v| major(v) == major(i))).or(first)
}

pub fn tested_version(agent: AgentKind) -> Option<&'static str> {
    tested_version_for(agent, None)
}

fn binary_name(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::ClaudeCode => "claude",
        AgentKind::OpenCode => "opencode",
        AgentKind::JCode => "jcode",
        AgentKind::Codex => "codex",
        AgentKind::Antigravity => "agy",
    }
}

/// Best-effort installed-version probe (`<binary> --version`, first
/// semver-looking token).
pub fn installed_version(agent: AgentKind) -> Option<String> {
    let output = std::process::Command::new(binary_name(agent)).arg("--version").output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_whitespace()
        .find(|token| {
            let mut parts = token.trim_start_matches('v').split('.');
            matches!(
                (parts.next(), parts.next()),
                (Some(a), Some(b)) if a.chars().all(|c| c.is_ascii_digit())
                    && b.chars().all(|c| c.is_ascii_digit())
            )
        })
        .map(|t| t.trim_start_matches('v').to_string())
}

/// None = fine; Some(warning) = drift the user should know about.
pub fn drift_warning(agent: AgentKind) -> Option<String> {
    let installed = installed_version(agent);
    let tested = tested_version_for(agent, installed.as_deref())?;
    classify_drift(agent, installed.as_deref(), tested)
}

/// What the tested version does and does not cover, where it is less than
/// "imported and resumed".
fn caveat(agent: AgentKind, tested: &str) -> String {
    if agent == AgentKind::OpenCode && major(tested) == "2" {
        format!(" (import accepted by opencode {tested}; resuming an imported session was not exercised)")
    } else {
        String::new()
    }
}

/// The pure half of drift detection, split out so it can be tested without
/// depending on which agent versions happen to be installed here.
fn classify_drift(agent: AgentKind, installed: Option<&str>, tested: &str) -> Option<String> {
    let Some(installed) = installed else {
        return Some(format!(
            "{agent} binary not found on PATH — cannot verify version (tested against {tested})"
        ));
    };
    if installed == tested {
        return None;
    }
    let caveat = caveat(agent, tested);
    let major_minor = |v: &str| v.split('.').take(2).collect::<Vec<_>>().join(".");
    if major_minor(installed) == major_minor(tested) {
        Some(format!(
            "{agent} {installed} installed; import path tested against {tested}{caveat} (patch drift — \
             likely fine)"
        ))
    } else {
        Some(format!(
            "{agent} {installed} installed but the import path was tested against {tested}{caveat} — \
             verify the imported session resumes, or use --mode seed"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CC: AgentKind = AgentKind::ClaudeCode;

    #[test]
    fn exact_match_is_silent() {
        assert_eq!(classify_drift(CC, Some("2.1.233"), "2.1.233"), None);
    }

    #[test]
    fn patch_drift_is_reassuring_but_reported() {
        let warning = classify_drift(CC, Some("2.1.234"), "2.1.233").unwrap();
        assert!(warning.contains("patch drift"), "{warning}");
    }

    #[test]
    fn minor_drift_tells_the_user_what_to_do() {
        let warning = classify_drift(CC, Some("2.2.0"), "2.1.233").unwrap();
        assert!(warning.contains("--mode seed"), "{warning}");
        let major = classify_drift(CC, Some("3.0.1"), "2.1.233").unwrap();
        assert!(major.contains("--mode seed"), "{major}");
    }

    /// OpenCode 1.x and 2.x are different programs with a different import:
    /// each install is compared with the entry of its own major version.
    #[test]
    fn an_agent_may_be_tested_at_more_than_one_major_version() {
        const OC: AgentKind = AgentKind::OpenCode;
        assert_eq!(tested_version_for(OC, Some("1.18.31")), Some("1.17.18"));
        assert_eq!(tested_version_for(OC, Some("2.0.25")), Some("2.0.25"));
        assert_eq!(tested_version_for(OC, Some("2.4.0")), Some("2.0.25"));
        assert_eq!(tested_version_for(OC, Some("3.0.0")), Some("1.17.18"), "an unknown major falls back to the first entry");
        assert_eq!(tested_version_for(OC, None), Some("1.17.18"));
        assert_eq!(tested_version(AgentKind::ClaudeCode), Some("2.1.234"));
        // 2.0.25 installed: silent against its own entry, not a "major drift" from 1.17.18.
        let tested = tested_version_for(OC, Some("2.0.25")).unwrap();
        assert_eq!(classify_drift(OC, Some("2.0.25"), tested), None);
        let drift = classify_drift(OC, Some("2.0.30"), tested).unwrap();
        assert!(drift.contains("patch drift") && drift.contains("resuming an imported session was not exercised"), "{drift}");
        assert!(!classify_drift(OC, Some("1.18.1"), "1.17.18").unwrap().contains("not exercised"));
    }

    #[test]
    fn missing_binary_is_reported_not_assumed_fine() {
        let warning = classify_drift(CC, None, "2.1.233").unwrap();
        assert!(warning.contains("not found on PATH"), "{warning}");
    }
}
