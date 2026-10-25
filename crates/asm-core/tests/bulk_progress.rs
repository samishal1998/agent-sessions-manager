//! `bulk::run` says how far it has got after every item. A process of its
//! own, with every store in a temp dir: a batch reads the agents' stores.

use std::fs;

use asm_core::adapter::SessionFilter;
use asm_core::bulk::{self, BulkAction};

#[test]
fn progress_is_reported_once_per_item() {
    let root = tempfile::tempdir().unwrap();
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
        fs::create_dir_all(root.path().join(sub)).unwrap();
        // Safety: the only test in this process, set before anything reads them.
        unsafe { std::env::set_var(var, root.path().join(sub)) };
    }
    let project = root.path().join("claude/projects/-w-app");
    fs::create_dir_all(&project).unwrap();
    for id in ["7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43", "7f3a1c99-2d4e-4b91-9a05-6c7e8f201b44"] {
        let line = serde_json::json!({"type": "user", "uuid": "u1", "parentUuid": null, "isSidechain": false,
            "timestamp": "2026-10-09T10:00:00.000Z", "cwd": "/w/app", "sessionId": id, "message": {"role": "user", "content": "hi"}});
        fs::write(project.join(format!("{id}.jsonl")), line.to_string()).unwrap();
    }
    let sessions = asm_core::ops::list_sessions(&SessionFilter::default()).unwrap();
    assert_eq!(sessions.len(), 2);

    let mut seen = Vec::new();
    let report = bulk::run(&sessions, &BulkAction::Export { dir: root.path().join("out") }, |done, total| seen.push((done, total)));
    assert_eq!(report.ok(), 2, "{:?}", report.problems());
    assert_eq!(seen, [(1, 2), (2, 2)]);
}
