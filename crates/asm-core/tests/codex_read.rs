//! Codex read-adapter tests.
//!
//! The fixture store mirrors a real 0.148.0 install: a `state_5.sqlite`
//! whose `threads` row is what `codex resume` reads, and rollout JSONL
//! under `sessions/<Y>/<M>/<D>/`. Record shapes are copied from a captured
//! rollout — `custom_tool_call` with a plain-string `input`, a
//! `custom_tool_call_output` whose `output` is an *array* of content parts,
//! and a `reasoning` item carrying only `encrypted_content`.

use std::path::Path;

use asm_core::adapter::codex::CodexAdapter;
use asm_core::adapter::{AgentRead, SessionFilter};
use asm_core::ir::IrPart;
use asm_core::model::{AgentKind, SessionStatus};

const TRACKED: &str = "01a020ab-9a2c-71a1-8384-1e1058de5553";
const ORPHAN: &str = "01a020ab-1f35-7763-8138-231f59ac2e99";

fn rollout(id: &str, cwd: &str) -> String {
    let lines = [
        format!(
            r#"{{"timestamp":"2026-08-20T19:35:12.500Z","ordinal":0,"type":"session_meta","payload":{{"session_id":"{id}","id":"{id}","timestamp":"2026-08-20T19:35:12.500Z","cwd":"{cwd}","originator":"codex_exec","cli_version":"0.148.0","source":"exec"}}}}"#
        ),
        r#"{"timestamp":"2026-08-20T19:35:13.000Z","ordinal":1,"type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#.to_string(),
        r#"{"timestamp":"2026-08-20T19:35:13.100Z","ordinal":2,"type":"response_item","payload":{"type":"message","id":"msg_dev","role":"developer","content":[{"type":"input_text","text":"<skills_instructions>ignore me</skills_instructions>"}]}}"#.to_string(),
        r#"{"timestamp":"2026-08-20T19:35:13.200Z","ordinal":3,"type":"response_item","payload":{"type":"message","id":"msg_u","role":"user","content":[{"type":"input_text","text":"read NOTES.md"}]}}"#.to_string(),
        r#"{"timestamp":"2026-08-20T19:35:14.000Z","ordinal":4,"type":"response_item","payload":{"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"gAAAAABopaque"}}"#.to_string(),
        r#"{"timestamp":"2026-08-20T19:35:15.000Z","ordinal":5,"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","call_id":"call_1","name":"exec","input":"sed -n '1,5p' NOTES.md"}}"#.to_string(),
        r#"{"timestamp":"2026-08-20T19:35:16.000Z","ordinal":6,"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"demo"},{"type":"input_text","text":" file"}]}}"#.to_string(),
        r#"{"timestamp":"2026-08-20T19:35:17.000Z","ordinal":7,"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15216,"cached_input_tokens":14080,"output_tokens":303}}}}"#.to_string(),
        r#"{"timestamp":"2026-08-20T19:35:17.500Z","ordinal":8,"type":"response_item","payload":{"type":"message","id":"msg_a","role":"assistant","content":[{"type":"output_text","text":"first"}]}}"#.to_string(),
        // A torn tail: codex was still writing when we read.
        r#"{"timestamp":"2026-08-20T19:35:18.000Z","ordinal":9,"type":"resp"#.to_string(),
    ];
    lines.join("\n")
}

/// A `threads` row for `TRACKED` only, so `ORPHAN` exercises the
/// filesystem sweep for rollouts codex's own picker cannot see.
fn write_store(root: &Path, cwd: &str) {
    let day = root.join("sessions/2026/08/20");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::write(
        day.join(format!("rollout-2026-08-20T19-35-12-{TRACKED}.jsonl")),
        rollout(TRACKED, cwd),
    )
    .unwrap();
    std::fs::write(
        day.join(format!("rollout-2026-08-20T19-34-40-{ORPHAN}.jsonl")),
        rollout(ORPHAN, cwd),
    )
    .unwrap();
    // A stray file in the same tree must not be read as a session.
    std::fs::write(day.join("notes.txt"), "not a rollout").unwrap();

    let conn = rusqlite::Connection::open(root.join("state_5.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE threads (
            id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL,
            created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            source TEXT NOT NULL, model_provider TEXT NOT NULL, cwd TEXT NOT NULL,
            title TEXT NOT NULL, sandbox_policy TEXT NOT NULL, approval_mode TEXT NOT NULL,
            tokens_used INTEGER NOT NULL DEFAULT 0, has_user_event INTEGER NOT NULL DEFAULT 0,
            archived INTEGER NOT NULL DEFAULT 0, archived_at INTEGER,
            git_sha TEXT, git_branch TEXT, git_origin_url TEXT,
            cli_version TEXT NOT NULL DEFAULT '', model TEXT, preview TEXT NOT NULL DEFAULT '');
         CREATE TABLE thread_spawn_edges (
            parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL PRIMARY KEY,
            status TEXT NOT NULL);",
    )
    .unwrap();
    let path = root
        .join(format!("sessions/2026/08/20/rollout-2026-08-20T19-35-12-{TRACKED}.jsonl"));
    conn.execute(
        "INSERT INTO threads (id, rollout_path, created_at, updated_at, source, model_provider,
             cwd, title, sandbox_policy, approval_mode, tokens_used, archived, git_branch,
             cli_version, model)
         VALUES (?1, ?2, 1787253312, 1787253341, 'exec', 'openai', ?3, 'Read NOTES.md',
             '{}', 'never', 15216, 0, 'main', '0.148.0', 'gpt-5.6-sol')",
        rusqlite::params![TRACKED, path.display().to_string(), cwd],
    )
    .unwrap();
}

fn store() -> (tempfile::TempDir, CodexAdapter, String) {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("proj").display().to_string();
    write_store(dir.path(), &cwd);
    let adapter = CodexAdapter::with_root(dir.path());
    (dir, adapter, cwd)
}

#[test]
fn lists_the_tracked_thread_with_its_database_metadata() {
    let (_dir, adapter, cwd) = store();
    let sessions = adapter.sessions(&SessionFilter::default()).unwrap();
    let tracked = sessions.iter().find(|s| s.handle.native_id == TRACKED).unwrap();

    assert_eq!(tracked.handle.agent, AgentKind::Codex);
    assert_eq!(tracked.title.as_deref(), Some("Read NOTES.md"));
    assert_eq!(tracked.project_root.display().to_string(), cwd);
    assert_eq!(tracked.git_branch.as_deref(), Some("main"));
    assert_eq!(tracked.model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(tracked.agent_version.as_deref(), Some("0.148.0"));
    assert_eq!(tracked.status, SessionStatus::Idle);
    assert!(tracked.size_bytes.is_some_and(|b| b > 0));
}

/// A rollout with no `threads` row is a real session codex will adopt on
/// resume. Hiding it would hide exactly what asm exists to surface.
#[test]
fn sweeps_up_rollouts_the_database_has_not_adopted() {
    let (_dir, adapter, _cwd) = store();
    let sessions = adapter.sessions(&SessionFilter::default()).unwrap();
    let ids: Vec<&str> = sessions.iter().map(|s| s.handle.native_id.as_str()).collect();
    assert!(ids.contains(&TRACKED), "{ids:?}");
    assert!(ids.contains(&ORPHAN), "{ids:?}");
    assert_eq!(sessions.len(), 2, "the stray notes.txt must not become a session");
}

#[test]
fn a_short_id_separates_sessions_started_in_the_same_minute() {
    let (_dir, adapter, _cwd) = store();
    let sessions = adapter.sessions(&SessionFilter::default()).unwrap();
    let shorts: Vec<&str> = sessions.iter().map(|s| s.short_id()).collect();
    assert_eq!(shorts.len(), 2);
    assert_ne!(shorts[0], shorts[1], "UUIDv7 ids share a timestamp prefix: {shorts:?}");
    // Still a prefix of the real id, so it resolves back.
    for session in &sessions {
        assert!(session.handle.native_id.starts_with(session.short_id()));
    }
}

#[test]
fn export_maps_every_record_shape_the_rollout_uses() {
    let (_dir, adapter, _cwd) = store();
    let sessions = adapter.sessions(&SessionFilter::default()).unwrap();
    let tracked = sessions.iter().find(|s| s.handle.native_id == TRACKED).unwrap();
    let ir = adapter.export_ir(tracked).unwrap();

    let parts: Vec<&IrPart> = ir.messages.iter().flat_map(|m| m.parts.iter()).collect();

    // `developer` turns are codex's own instructions, not the user's.
    let developer = ir
        .messages
        .iter()
        .find(|m| m.source_id.as_deref() == Some("msg_dev"))
        .expect("developer message");
    assert_eq!(developer.role, asm_core::ir::IrRole::System);

    assert!(parts.iter().any(|p| matches!(p, IrPart::Text { text } if text == "read NOTES.md")));
    assert!(parts.iter().any(|p| matches!(p, IrPart::Reasoning { opaque: true, .. })));
    assert!(
        parts
            .iter()
            .any(|p| matches!(p, IrPart::ToolCall { name, .. } if name == "exec")),
        "custom_tool_call must map to a tool call"
    );
    // The output is an array of content parts, not a string.
    assert!(
        parts.iter().any(|p| matches!(p, IrPart::ToolResult { output, .. } if output == "demo file")),
        "custom_tool_call_output content parts must be joined"
    );

    // `token_count` events carry the real totals; the threads row rounds.
    assert_eq!(ir.usage.input_tokens, Some(15216));
    assert_eq!(ir.usage.cache_read_tokens, Some(14080));
}

/// The last line of a rollout being half-written is normal while codex is
/// running, and must not fail the read.
#[test]
fn a_torn_final_line_is_tolerated() {
    let (_dir, adapter, _cwd) = store();
    let sessions = adapter.sessions(&SessionFilter::default()).unwrap();
    let tracked = sessions.iter().find(|s| s.handle.native_id == TRACKED).unwrap();
    let ir = adapter.export_ir(tracked).unwrap();
    assert!(!ir.messages.is_empty());
}

/// Codex is read-only, and every UI decides what to offer from these flags.
#[test]
fn capabilities_admit_that_nothing_is_writable() {
    let (_dir, adapter, _cwd) = store();
    let caps = adapter.capabilities();
    assert!(caps.list && caps.read_transcript && caps.export_ir && caps.resume_native);
    assert!(!caps.rename && !caps.archive && !caps.delete && !caps.relocate && !caps.import_ir);
}
