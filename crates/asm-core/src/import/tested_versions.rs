//! The tested-versions matrix. Full-mode import writes native records, so
//! it is only as safe as the agent versions it was verified against; drift
//! produces a warning, never a silent assumption.

use crate::model::AgentKind;

/// (agent, exact version asm's writes were last verified against)
///
/// "Verified" never means the code compiled. For Claude Code and OpenCode
/// it means a real session was imported and then resumed in that agent's
/// own CLI with its conversation intact. jcode is not an import target, so
/// there it means the write verbs were exercised against a real store and
/// the agent's own view was checked afterwards: rename read back through
/// `jcode session rename --json`, and archive/delete confirmed by jcode no
/// longer being able to resolve the session.
pub const TESTED: &[(AgentKind, &str)] = &[
    (AgentKind::ClaudeCode, "2.1.234"),
    (AgentKind::OpenCode, "1.17.18"),
    (AgentKind::JCode, "0.78.0"),
    (AgentKind::Codex, "0.148.0"),
];

pub fn tested_version(agent: AgentKind) -> Option<&'static str> {
    TESTED.iter().find(|(a, _)| *a == agent).map(|(_, v)| *v)
}

fn binary_name(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::ClaudeCode => "claude",
        AgentKind::OpenCode => "opencode",
        AgentKind::JCode => "jcode",
        AgentKind::Codex => "codex",
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
    let tested = tested_version(agent)?;
    classify_drift(agent, installed_version(agent).as_deref(), tested)
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
    let major_minor = |v: &str| v.split('.').take(2).collect::<Vec<_>>().join(".");
    if major_minor(installed) == major_minor(tested) {
        Some(format!(
            "{agent} {installed} installed; import path tested against {tested} (patch drift — \
             likely fine)"
        ))
    } else {
        Some(format!(
            "{agent} {installed} installed but the import path was tested against {tested} — \
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

    #[test]
    fn missing_binary_is_reported_not_assumed_fine() {
        let warning = classify_drift(CC, None, "2.1.233").unwrap();
        assert!(warning.contains("not found on PATH"), "{warning}");
    }
}
