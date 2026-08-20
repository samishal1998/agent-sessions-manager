//! Antigravity read-adapter tests.
//!
//! The fixture mirrors a real `agy` install: one SQLite database per
//! conversation under `conversations/`, the two JSON caches that describe
//! them, and antigravity's own JSONL rendering of the steps under
//! `brain/<id>/.system_generated/logs/`. Record shapes are copied from a
//! captured conversation — `Title` empty with the real name in `Preview`,
//! `WorkspaceURIs` null, and user text wrapped in a `<USER_REQUEST>`
//! envelope alongside metadata meant for the model.

use std::path::Path;

use asm_core::adapter::antigravity::AntigravityAdapter;
use asm_core::adapter::{AgentRead, SessionFilter};
use asm_core::ir::{IrPart, IrRole};
use asm_core::model::AgentKind;

const MAPPED: &str = "3f6bfd66-67b6-4c35-9065-5316a755e9af";
const ORPHAN: &str = "8c1de4aa-1111-4222-8333-944455556666";

fn transcript() -> String {
    [
        r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-08-20T19:33:38Z","content":"<USER_REQUEST>\nReply with exactly the word: first\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: 2026-08-20T19:33:38Z.\n</ADDITIONAL_METADATA>"}"#,
        r#"{"step_index":1,"source":"SYSTEM","type":"CHECKPOINT","status":"DONE","created_at":"2026-08-20T19:33:38Z","content":"{{ CHECKPOINT 0 }} truncated context summary"}"#,
        r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-08-20T19:33:40Z","content":"first"}"#,
        // A step type asm has never seen must not be dropped.
        r#"{"step_index":3,"source":"SYSTEM","type":"SOME_FUTURE_STEP","status":"DONE","created_at":"2026-08-20T19:33:41Z","content":"kept as context"}"#,
        // A torn final line: antigravity was still writing.
        r#"{"step_index":4,"source":"MODEL","type":"PLANNER_RESP"#,
    ]
    .join("\n")
}

fn write_store(root: &Path, workspace: &str) {
    let conversations = root.join("conversations");
    std::fs::create_dir_all(&conversations).unwrap();
    for id in [MAPPED, ORPHAN] {
        // The real store is SQLite; the adapter only ever stats these, so
        // the bytes need not be a database — but the siblings beside them
        // must not be mistaken for conversations.
        std::fs::write(conversations.join(format!("{id}.db")), b"sqlitefixture").unwrap();
        std::fs::write(conversations.join(format!("{id}.db-wal")), vec![0u8; 1024]).unwrap();
        std::fs::write(conversations.join(format!("{id}.db-shm")), b"shm").unwrap();

        let logs = root.join("brain").join(id).join(".system_generated/logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("transcript.jsonl"), transcript()).unwrap();
    }

    let cache = root.join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    // `Title` empty and the real name in `Preview` is the normal case.
    std::fs::write(
        cache.join("conversation_metadata.json"),
        format!(
            r#"{{"conversations":{{"{MAPPED}":{{"summary":{{"ID":"{MAPPED}","Title":"","Preview":"Exact Word Reply Test","NumSteps":3,"UpdatedAt":"2026-08-20T19:33:40.709257577Z","WorkspaceURIs":null,"ProjectID":"default-cli-project"}},"is_internal":false}}}}}}"#
        ),
    )
    .unwrap();
    // Keyed workspace -> conversation, and only the latest per directory.
    std::fs::write(
        cache.join("last_conversations.json"),
        format!(r#"{{"{workspace}":"{MAPPED}"}}"#),
    )
    .unwrap();
}

fn store() -> (tempfile::TempDir, AntigravityAdapter, String) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("proj").display().to_string();
    write_store(dir.path(), &workspace);
    let adapter = AntigravityAdapter::with_root(dir.path().to_path_buf());
    (dir, adapter, workspace)
}

#[test]
fn lists_conversations_with_their_cached_titles() {
    let (_dir, adapter, workspace) = store();
    let sessions = adapter.sessions(&SessionFilter::default()).unwrap();
    assert_eq!(sessions.len(), 2, "-wal and -shm must not become sessions: {sessions:?}");

    let mapped = sessions.iter().find(|s| s.handle.native_id == MAPPED).unwrap();
    assert_eq!(mapped.handle.agent, AgentKind::Antigravity);
    // `Title` is empty in the real store; `Preview` is the name shown.
    assert_eq!(mapped.title.as_deref(), Some("Exact Word Reply Test"));
    assert_eq!(mapped.project_root.display().to_string(), workspace);
    assert_eq!(mapped.created.map(|t| t.to_string()), Some("2026-08-20T19:33:38Z".to_string()));
}

/// `last_conversations.json` holds one conversation per directory, so an
/// older conversation in a reused directory has no workspace anywhere. An
/// empty project is the honest answer; inventing one would put the session
/// in a project it may have nothing to do with.
#[test]
fn a_conversation_with_no_recorded_workspace_gets_an_empty_project() {
    let (_dir, adapter, _workspace) = store();
    let sessions = adapter.sessions(&SessionFilter::default()).unwrap();
    let orphan = sessions.iter().find(|s| s.handle.native_id == ORPHAN).unwrap();
    assert_eq!(orphan.project_root.as_os_str(), "");
    assert!(orphan.title.is_none(), "no cached summary means no title to show");
}

/// Most of a fresh conversation lives in the write-ahead log, so a size
/// that counted only the `.db` would understate it several times over.
#[test]
fn size_counts_the_write_ahead_log() {
    let (_dir, adapter, _workspace) = store();
    let sessions = adapter.sessions(&SessionFilter::default()).unwrap();
    let size = sessions[0].size_bytes.unwrap();
    assert!(size > 1024, "expected db + wal, got {size}");
}

#[test]
fn export_strips_the_envelope_and_keeps_the_roles_straight() {
    let (_dir, adapter, _workspace) = store();
    let sessions = adapter.sessions(&SessionFilter::default()).unwrap();
    let mapped = sessions.iter().find(|s| s.handle.native_id == MAPPED).unwrap();
    let ir = adapter.export_ir(mapped).unwrap();

    let text = |m: &asm_core::ir::IrMessage| match &m.parts[0] {
        IrPart::Text { text } => text.clone(),
        other => panic!("expected text, got {other:?}"),
    };

    // The user's words come wrapped with the local time and any settings
    // that changed; only the request is what they said.
    let user = ir.messages.iter().find(|m| m.role == IrRole::User).unwrap();
    assert_eq!(text(user), "Reply with exactly the word: first");

    let assistant = ir.messages.iter().find(|m| m.role == IrRole::Assistant).unwrap();
    assert_eq!(text(assistant), "first");

    // A checkpoint is antigravity's own compaction summary and a step type
    // asm has never seen is still context — both are system, neither is
    // dropped, and neither is attributed to the user.
    let system: Vec<String> =
        ir.messages.iter().filter(|m| m.role == IrRole::System).map(text).collect();
    assert!(system.iter().any(|t| t.contains("CHECKPOINT")), "{system:?}");
    assert!(system.iter().any(|t| t == "kept as context"), "{system:?}");

    // The torn final line is skipped, not fatal.
    assert_eq!(ir.messages.len(), 4);
}

/// A conversation whose JSONL log is gone must say why rather than reporting
/// an empty conversation, because the steps do still exist — as protobuf
/// blobs asm deliberately does not decode.
#[test]
fn a_missing_transcript_log_explains_itself() {
    let (dir, adapter, _workspace) = store();
    std::fs::remove_file(dir.path().join(format!(
        "brain/{MAPPED}/.system_generated/logs/transcript.jsonl"
    )))
    .unwrap();
    let sessions = adapter.sessions(&SessionFilter::default()).unwrap();
    let mapped = sessions.iter().find(|s| s.handle.native_id == MAPPED).unwrap();
    let error = adapter.export_ir(mapped).unwrap_err().to_string();
    assert!(error.contains("protobuf"), "{error}");
}

#[test]
fn capabilities_are_read_only_but_can_send() {
    let (_dir, adapter, _workspace) = store();
    let caps = adapter.capabilities();
    assert!(caps.list && caps.read_transcript && caps.export_ir && caps.send_message);
    assert!(!caps.rename && !caps.archive && !caps.delete && !caps.import_ir);
}
