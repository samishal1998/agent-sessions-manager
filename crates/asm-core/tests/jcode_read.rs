//! jcode read-adapter tests against a fixture store built to match the
//! schema in the project's own source: whole-snapshot `sessions/<id>.json`
//! (plus a `.journal.jsonl` sibling that must never be mistaken for a
//! session), `custom_title` outranking `title`, `working_dir` serialized
//! *after* the message array, and PID-file liveness under `active_pids/`.

use std::fs;
use std::path::{Path, PathBuf};

use asm_core::adapter::jcode::JCodeAdapter;
use asm_core::adapter::{AgentRead, AgentWrite, SessionFilter};
use asm_core::ir::IrPart;
use asm_core::model::{AgentKind, SessionStatus};

const MAIN: &str = "01HQ8ZK7MAIN0000000000";
const CHILD: &str = "01HQ8ZK7CHILD000000000";
const DEBUG: &str = "01HQ8ZK7DEBUG000000000";

/// Field order mirrors jcode's struct: `messages` sits before the metadata
/// a listing needs, which is why the reader streams rather than head-scans.
fn snapshot(id: &str, extra_head: &str, messages: &str, extra_tail: &str) -> String {
    format!(
        r#"{{
  "id": "{id}",
  {extra_head}
  "created_at": "2026-08-01T10:00:00Z",
  "updated_at": "2026-08-01T11:30:00Z",
  "messages": [{messages}],
  "model": "gpt-5.6-sol",
  "working_dir": "/home/user/projects/demo",
  "short_name": "fox",
  "status": "Active",
  "last_pid": 4242
  {extra_tail}
}}"#
    )
}

fn write_store(root: &Path) {
    let sessions = root.join("sessions");
    fs::create_dir_all(&sessions).unwrap();

    let conversation = r#"
    {"id":"m1","role":"user","timestamp":"2026-08-01T10:00:00Z",
     "content":[{"type":"text","text":"find the bug"}]},
    {"id":"m2","role":"assistant","timestamp":"2026-08-01T10:00:09Z",
     "content":[
       {"type":"reasoning","text":"look at the parser"},
       {"type":"anthropic_thinking","thinking":"signed reasoning","signature":"sig"},
       {"type":"open_ai_reasoning","id":"r1","summary":["encrypted summary"],
        "encrypted_content":"opaque"},
       {"type":"text","text":"Running the tests."},
       {"type":"tool_use","id":"call_1","name":"bash","input":{"command":"cargo test"}},
       {"type":"image","media_type":"image/png","data":"iVBORw0KGgo="},
       {"type":"open_ai_compaction","encrypted_content":"blob"}
     ]},
    {"id":"m3","role":"user","timestamp":"2026-08-01T10:01:00Z",
     "content":[{"type":"tool_result","tool_use_id":"call_1","content":"1 failed","is_error":true}]}
    "#;

    // custom_title must win over the generated title.
    fs::write(
        sessions.join(format!("{MAIN}.json")),
        snapshot(
            MAIN,
            r#""title": "Generated title", "custom_title": "Renamed by the user","#,
            conversation,
            "",
        ),
    )
    .unwrap();
    // The journal sibling is not a session.
    fs::write(
        sessions.join(format!("{MAIN}.journal.jsonl")),
        "{\"event\":\"append\"}\n",
    )
    .unwrap();

    // A spawned child and a debug session: both hidden by default.
    fs::write(
        sessions.join(format!("{CHILD}.json")),
        snapshot(CHILD, &format!(r#""title": "Child", "parent_id": "{MAIN}","#), "", ""),
    )
    .unwrap();
    fs::write(
        sessions.join(format!("{DEBUG}.json")),
        snapshot(DEBUG, r#""title": "Debug run","#, "", r#", "is_debug": true"#),
    )
    .unwrap();
}

fn adapter(root: &Path) -> JCodeAdapter {
    JCodeAdapter::with_root(root)
}

fn only(root: &Path) -> asm_core::model::Session {
    let sessions = adapter(root).sessions(&SessionFilter::default()).unwrap();
    assert_eq!(sessions.len(), 1, "child and debug sessions are hidden by default");
    sessions.into_iter().next().unwrap()
}

#[test]
fn lists_sessions_with_metadata_from_after_the_message_array() {
    let dir = tempfile::tempdir().unwrap();
    write_store(dir.path());
    let session = only(dir.path());

    assert_eq!(session.handle.agent, AgentKind::JCode);
    assert_eq!(session.handle.native_id, MAIN);
    assert_eq!(
        session.title.as_deref(),
        Some("Renamed by the user"),
        "custom_title outranks the generated title"
    );
    // These three are serialized after `messages`, so reaching them proves
    // the whole document was streamed.
    assert_eq!(session.project_root, Path::new("/home/user/projects/demo"));
    assert_eq!(session.model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(session.slug.as_deref(), Some("fox"), "the memorable name drives --resume");
    assert_eq!(session.created.unwrap().to_string(), "2026-08-01T10:00:00Z");
    assert_eq!(session.updated.unwrap().to_string(), "2026-08-01T11:30:00Z");
}

#[test]
fn journal_files_are_not_sessions() {
    let dir = tempfile::tempdir().unwrap();
    write_store(dir.path());
    let all = adapter(dir.path())
        .sessions(&SessionFilter { include_children: true, ..SessionFilter::default() })
        .unwrap();

    assert_eq!(all.len(), 3, "main + child + debug, and no journal");
    assert!(all.iter().all(|s| !s.handle.native_id.contains("journal")));
}

#[test]
fn children_and_debug_sessions_are_hidden_until_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    write_store(dir.path());
    let all = adapter(dir.path())
        .sessions(&SessionFilter { include_children: true, ..SessionFilter::default() })
        .unwrap();

    let child = all.iter().find(|s| s.handle.native_id == CHILD).unwrap();
    assert_eq!(child.parent.as_deref(), Some(MAIN));
    assert!(all.iter().any(|s| s.handle.native_id == DEBUG));
}

#[test]
fn a_live_session_is_one_whose_pid_still_exists() {
    let dir = tempfile::tempdir().unwrap();
    write_store(dir.path());
    let pids = dir.path().join("active_pids");
    fs::create_dir_all(&pids).unwrap();

    // A stale marker must not mark the session live: jcode leaves these
    // behind when a process dies, the same way OpenCode leaves lock dirs.
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead = child.id();
    child.wait().unwrap();
    fs::write(pids.join(MAIN), dead.to_string()).unwrap();
    assert_eq!(only(dir.path()).status, SessionStatus::Idle, "dead pid is not live");

    fs::write(pids.join(MAIN), std::process::id().to_string()).unwrap();
    assert_eq!(
        only(dir.path()).status,
        SessionStatus::Live { pid: Some(std::process::id()) }
    );
}

#[test]
fn resume_uses_the_memorable_name_in_the_session_directory() {
    let dir = tempfile::tempdir().unwrap();
    write_store(dir.path());
    let session = only(dir.path());
    let command = adapter(dir.path()).resume_command(&session).unwrap();

    assert_eq!(command.get_program(), "jcode");
    let args: Vec<_> = command.get_args().collect();
    assert_eq!(args, ["--resume", "fox"]);
    assert_eq!(command.get_current_dir(), Some(Path::new("/home/user/projects/demo")));
}

#[test]
fn exports_every_content_block_kind_to_the_ir() {
    let dir = tempfile::tempdir().unwrap();
    write_store(dir.path());
    let session = only(dir.path());
    let ir = adapter(dir.path()).export_ir(&session).unwrap();

    assert_eq!(ir.messages.len(), 3);
    let parts: Vec<&IrPart> = ir.messages.iter().flat_map(|m| m.parts.iter()).collect();

    let reasoning: Vec<(&str, bool)> = parts
        .iter()
        .filter_map(|p| match p {
            IrPart::Reasoning { summary, opaque } => Some((summary.as_str(), *opaque)),
            _ => None,
        })
        .collect();
    assert_eq!(
        reasoning,
        vec![
            ("look at the parser", false),      // plain reasoning replays fine
            ("signed reasoning", true),          // anthropic signature is provider-bound
            ("encrypted summary", true),         // so is an encrypted OpenAI item
        ]
    );

    assert!(parts.iter().any(|p| matches!(p, IrPart::ToolCall { name, .. } if name == "bash")));
    assert!(
        parts
            .iter()
            .any(|p| matches!(p, IrPart::ToolResult { is_error: true, output, .. } if output == "1 failed"))
    );
    assert!(
        parts.iter().any(
            |p| matches!(p, IrPart::File { mime, .. } if mime.as_deref() == Some("image/png"))
        )
    );

    // A block kind the IR does not model is counted, not silently dropped.
    let skipped = &ir.extensions["jcode"]["skipped_block_counts"];
    assert_eq!(skipped["open_ai_compaction"], 1);
    // jcode-only state rides along so a round-trip can restore it.
    assert_eq!(ir.extensions["jcode"]["short_name"], "fox");
}

#[test]
fn writing_is_refused_rather_than_guessed() {
    // The adapter is read-only until its writes can be checked against a
    // real jcode install; the refusal must be explicit, not a silent no-op.
    let dir = tempfile::tempdir().unwrap();
    write_store(dir.path());
    let session = only(dir.path());
    let adapter = adapter(dir.path());

    let err = adapter.rename(&session, "new").unwrap_err().to_string();
    assert!(err.contains("jcode"), "{err}");
    assert!(adapter.delete(&session).is_err());
    assert!(adapter.archive(&session).is_err());
    assert!(adapter.relocate(&session, &PathBuf::from("/tmp")).is_err());
}
