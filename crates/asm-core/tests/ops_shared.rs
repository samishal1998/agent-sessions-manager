//! `ops` entry points that take work the caller has already done: the
//! session counts a frontend has just listed. The stores are process-wide,
//! so they are set up once, before anything reads them.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use asm_core::CoreError;
use asm_core::model::AgentKind;
use asm_core::ops;

const ID: &str = "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43";

fn isolate() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (var, sub) in [
            ("HOME", "home"),
            ("ASM_DATA_DIR", "asm"),
            ("XDG_DATA_HOME", "xdg-data"),
            ("XDG_CONFIG_HOME", "xdg-config"),
            ("XDG_STATE_HOME", "xdg-state"),
            ("XDG_CACHE_HOME", "xdg-cache"),
            ("CLAUDE_CONFIG_DIR", "claude"),
            ("CODEX_HOME", "codex"),
            ("JCODE_HOME", "jcode"),
            ("ASM_ANTIGRAVITY_ROOT", "antigravity"),
        ] {
            fs::create_dir_all(root.join(sub)).unwrap();
            // Safety: set once per test process, before concurrent readers.
            unsafe { std::env::set_var(var, root.join(sub)) };
        }
        let project = root.join("claude/projects/-tmp-app");
        fs::create_dir_all(&project).unwrap();
        let record = |kind: &str, uuid: &str, parent: &str, message: &str| {
            format!(
                r#"{{"type":"{kind}","uuid":"{uuid}","parentUuid":{parent},"isSidechain":false,"timestamp":"2026-10-09T10:00:00.000Z","cwd":"/tmp/app","sessionId":"{ID}","message":{message}}}"#
            )
        };
        let transcript = [
            record("user", "u1", "null", r#"{"role":"user","content":"hello there"}"#),
            record(
                "assistant",
                "a1",
                r#""u1""#,
                r#"{"role":"assistant","content":[{"type":"text","text":"hi"}]}"#,
            ),
        ]
        .join("\n");
        fs::write(project.join(format!("{ID}.jsonl")), transcript).unwrap();
        dir
    })
    .path()
}

/// A frontend that has just listed the stores hands `doctor` the counts
/// instead of having it list them again; `doctor()` still counts for itself.
#[test]
fn doctor_reports_the_counts_it_is_given() {
    isolate();
    let counted = ops::doctor().unwrap();
    let claude = counted.agents.iter().find(|a| a.agent == AgentKind::ClaudeCode).unwrap();
    assert_eq!(claude.session_count, 1);

    let given = ops::doctor_with(|_| Ok(7)).unwrap();
    let claude = given.agents.iter().find(|a| a.agent == AgentKind::ClaudeCode).unwrap();
    assert_eq!(claude.session_count, 7, "the caller's number, not a fresh listing's");

    let refused = ops::doctor_with(|_| Err(CoreError::Invalid { msg: "no".into() }));
    assert!(refused.is_err(), "a count that cannot be given is not papered over");
}
