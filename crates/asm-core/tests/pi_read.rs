//! pi and omp read-adapter tests.
//!
//! `fixtures/omp-18.11.3-branched.jsonl` was written by omp 18.11.3's own
//! `SessionManager` (driven from a script in a scratch home, no model
//! calls): a 256-byte title slot, the header, a renamed session, and a
//! branch taken back at the first question, so the answer on the abandoned
//! branch must not appear. Only the project path was replaced. The pi files
//! follow pi 1.5.0's `docs/session-format.md`: no slot, `session_info` names,
//! string user content, and a version 1 file with no ids at all.

use std::path::{Path, PathBuf};

use asm_core::adapter::pi::PiAdapter;
use asm_core::adapter::{AgentRead, SessionFilter};
use asm_core::ir::{IrPart, IrRole};
use asm_core::model::{AgentKind, SessionLocation};

const OMP: &str = include_str!("fixtures/omp-18.11.3-branched.jsonl");
const OMP_ID: &str = "01a19e5a-44b5-755e-9374-a9b367ad5f65";

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn omp_store(root: &Path) -> PathBuf {
    let file = root.join(format!("sessions/-proj/2026-11-02T22-21-18-389Z_{OMP_ID}.jsonl"));
    write(&file, OMP);
    file
}

fn all() -> SessionFilter {
    SessionFilter { include_children: true, ..SessionFilter::default() }
}

fn texts(parts: &[IrPart]) -> Vec<String> {
    parts
        .iter()
        .filter_map(|p| match p {
            IrPart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn an_omp_session_lists_with_its_title_model_usage_and_version() {
    let dir = tempfile::tempdir().unwrap();
    let file = omp_store(dir.path());
    let adapter = PiAdapter::with_root(AgentKind::Omp, dir.path());
    let sessions = adapter.sessions(&SessionFilter::default()).unwrap();
    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!(s.handle.agent, AgentKind::Omp);
    assert_eq!(s.handle.native_id, OMP_ID);
    assert_eq!(s.handle.location, SessionLocation::JsonlFile { path: file });
    assert_eq!(s.title.as_deref(), Some("Listing src"));
    assert_eq!(s.project_root, Path::new("/w/proj"), "from the header, not the folder name");
    assert_eq!(s.agent_version.as_deref(), Some("18.11.3"));
    // Every assistant turn in the file counts, on either branch: it was paid for.
    assert_eq!((s.usage.input_tokens, s.usage.output_tokens), (Some(45), Some(37)));
    assert!(s.usage.cost_usd.is_some());
    assert!(s.created.is_some() && s.updated.is_some());
    assert_eq!(s.short_id(), &OMP_ID[..13], "UUIDv7: eight characters is a timestamp bucket");
}

#[test]
fn the_transcript_is_the_current_branch_only() {
    let dir = tempfile::tempdir().unwrap();
    omp_store(dir.path());
    let adapter = PiAdapter::with_root(AgentKind::Omp, dir.path());
    let session = adapter.sessions(&SessionFilter::default()).unwrap().remove(0);
    let ir = adapter.export_ir(&session).unwrap();
    let shown: Vec<(IrRole, Vec<String>)> = ir.messages.iter().map(|m| (m.role, texts(&m.parts))).collect();
    assert_eq!(
        shown,
        vec![
            (IrRole::User, vec!["List the files in src".to_string()]),
            (IrRole::Assistant, vec!["BRANCH B ANSWER".to_string()]),
        ],
        "the branch taken back at the first question replaces the tool turn"
    );
    assert_eq!(ir.messages[0].source_id.as_deref(), Some("f590485a"));
}

/// The abandoned branch, read by making its last entry the leaf, carries
/// every kind of assistant block.
#[test]
fn tool_calls_results_and_signed_thinking_convert() {
    let dir = tempfile::tempdir().unwrap();
    let cut: String = OMP.lines().take(7).map(|l| format!("{l}\n")).collect();
    write(&dir.path().join("sessions/-proj/a_x.jsonl"), &cut);
    let adapter = PiAdapter::with_root(AgentKind::Omp, dir.path());
    let session = adapter.sessions(&SessionFilter::default()).unwrap().remove(0);
    let parts: Vec<IrPart> = adapter.export_ir(&session).unwrap().messages.into_iter().flat_map(|m| m.parts).collect();
    assert!(parts.iter().any(|p| matches!(p, IrPart::Reasoning { summary, opaque: true } if summary == "Use ls.")));
    assert!(parts.iter().any(|p| matches!(p, IrPart::ToolCall { call_id, name, input }
        if call_id == "toolu_1" && name == "bash" && input["command"] == "ls src")));
    assert!(parts.iter().any(|p| matches!(p, IrPart::ToolResult { call_id, output, is_error: false, .. }
        if call_id == "toolu_1" && output == "lib.rs\nmain.rs")));
    assert!(parts.iter().any(|p| matches!(p, IrPart::Text { text } if text == "src has lib.rs and main.rs.")));
}

/// omp rewrites the slot in place on a rename; the slot wins over the
/// `title_change` history.
#[test]
fn omp_s_title_slot_wins_over_older_title_changes() {
    let dir = tempfile::tempdir().unwrap();
    let renamed = OMP.replacen(r#""title":"Listing src","source""#, r#""title":"Renamed!!!!","source""#, 1);
    assert_ne!(renamed, OMP);
    write(&dir.path().join("sessions/-proj/a_x.jsonl"), &renamed);
    let adapter = PiAdapter::with_root(AgentKind::Omp, dir.path());
    assert_eq!(adapter.sessions(&SessionFilter::default()).unwrap()[0].title.as_deref(), Some("Renamed!!!!"));
}

#[test]
fn omp_subagents_are_children_listed_only_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let parent = omp_store(dir.path());
    let stem = parent.with_extension("");
    let child_header = |id: &str| {
        format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-11-02T22:22:00.000Z\",\"cwd\":\"/w/proj\",\"parentSession\":\"{}\"}}\n\
             {{\"type\":\"message\",\"id\":\"aaaa0001\",\"parentId\":null,\"timestamp\":\"2026-11-02T22:22:01.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"find the tests\",\"timestamp\":1}}}}\n",
            parent.display()
        )
    };
    write(&stem.join("Explorer.jsonl"), &child_header("01a19e5a-0000-7000-8000-000000000001"));
    write(&stem.join("Explorer/Nested.jsonl"), &child_header("01a19e5a-0000-7000-8000-000000000002"));
    // omp's own side files beside them.
    write(&stem.join("__advisor-1.jsonl"), &child_header("01a19e5a-0000-7000-8000-00000000000a"));
    write(&stem.join("Killed.jsonl.tombstone"), "");
    write(&stem.join("1.bash.log"), "output");
    write(&parent.parent().unwrap().join(format!(".{}.lock", parent.file_name().unwrap().to_str().unwrap())), "");

    let adapter = PiAdapter::with_root(AgentKind::Omp, dir.path());
    assert_eq!(adapter.sessions(&SessionFilter::default()).unwrap().len(), 1, "children hidden by default");
    let sessions = adapter.sessions(&all()).unwrap();
    let parent_of = |id: &str| sessions.iter().find(|s| s.handle.native_id == id).map(|s| s.parent.clone());
    assert_eq!(sessions.len(), 3);
    assert_eq!(parent_of(OMP_ID), Some(None));
    assert_eq!(parent_of("01a19e5a-0000-7000-8000-000000000001"), Some(Some(OMP_ID.to_string())));
    assert_eq!(
        parent_of("01a19e5a-0000-7000-8000-000000000002"),
        Some(Some("01a19e5a-0000-7000-8000-000000000001".to_string()))
    );
}

fn pi_line(id: &str, parent: Option<&str>, at: &str, rest: &str) -> String {
    let parent = parent.map_or("null".to_string(), |p| format!("\"{p}\""));
    format!("{{\"id\":\"{id}\",\"parentId\":{parent},\"timestamp\":\"2026-10-01T10:{at}.000Z\",{rest}}}\n")
}

/// A pi 1.5.0 session: string user content, a name set and then replaced,
/// a system-prompt message, a torn last line.
fn pi_session(id: &str, cwd: &str) -> String {
    let mut text = format!("{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-10-01T10:00:00.000Z\",\"cwd\":\"{cwd}\"}}\n");
    text += &pi_line("s0000001", None, "00:01", r#""type":"message","message":{"role":"system","content":"","sections":{"preamble":"You are..."},"timestamp":1}"#);
    text += &pi_line("u0000001", Some("s0000001"), "00:02", r#""type":"message","message":{"role":"user","content":"Fix the login bug\nin auth.rs","timestamp":2}"#);
    text += &pi_line("a0000001", Some("u0000001"), "00:03", r#""type":"message","message":{"role":"assistant","content":[{"type":"text","text":"Done."}],"provider":"openai","model":"gpt-5","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":0,"totalTokens":160,"cost":{"total":0.5}},"stopReason":"stop","timestamp":3}"#);
    text += &pi_line("n0000001", Some("a0000001"), "00:04", r#""type":"session_info","name":"First name""#);
    text += &pi_line("n0000002", Some("n0000001"), "00:05", r#""type":"session_info","name":"Auth fix""#);
    text += &pi_line("c0000001", Some("n0000002"), "00:06", r#""type":"usage","kind":"cache_warm","usage":{"input":0,"output":0,"cacheRead":5000,"cacheWrite":0,"cost":{"total":0.25}}"#);
    text += "{\"id\":\"torn\",\"parentId\":\"c0000001\",\"type\":\"mess";
    text
}

#[test]
fn a_pi_session_lists_with_its_latest_name_and_reads() {
    let dir = tempfile::tempdir().unwrap();
    let id = "0b6e9a52-3f1c-4d7e-9a11-2c5d8e7f6a01";
    write(&dir.path().join(format!("sessions/--w-app--/2026-10-01T10-00-00-000Z_{id}.jsonl")), &pi_session(id, "/w/app"));
    // Not sessions: a stray file, and a JSONL with no header.
    write(&dir.path().join("sessions/--w-app--/notes.txt"), "hi");
    write(&dir.path().join("sessions/--w-app--/other.jsonl"), "{\"type\":\"message\"}\n");

    let adapter = PiAdapter::with_root(AgentKind::Pi, dir.path());
    let sessions = adapter.sessions(&SessionFilter::default()).unwrap();
    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!((s.handle.agent, s.title.as_deref(), s.model.as_deref()), (AgentKind::Pi, Some("Auth fix"), Some("gpt-5")));
    assert_eq!(s.project_root, Path::new("/w/app"));
    assert_eq!((s.usage.input_tokens, s.usage.cache_read_tokens), (Some(100), Some(5010)), "the turn and the cache warm-up");
    assert_eq!(s.usage.cost_usd, Some(0.75));
    assert_eq!(s.short_id(), "0b6e9a52");

    let ir = adapter.export_ir(s).unwrap();
    let shown: Vec<(IrRole, Vec<String>)> = ir.messages.iter().map(|m| (m.role, texts(&m.parts))).collect();
    assert_eq!(
        shown,
        vec![
            (IrRole::User, vec!["Fix the login bug\nin auth.rs".to_string()]),
            (IrRole::Assistant, vec!["Done.".to_string()]),
        ],
        "the empty system prompt is not part of the conversation"
    );

    // A name cleared by an empty one falls back to the first user line.
    let cleared = pi_session(id, "/w/app").replace(r#""name":"Auth fix""#, r#""name":"""#);
    write(&dir.path().join(format!("sessions/--w-app--/2026-10-01T10-00-00-000Z_{id}.jsonl")), &cleared);
    assert_eq!(adapter.sessions(&SessionFilter::default()).unwrap()[0].title.as_deref(), Some("Fix the login bug"));
}

/// Version 1 had no `version`, no ids and no tree: read in file order.
#[test]
fn a_version_1_pi_file_reads_in_file_order() {
    let dir = tempfile::tempdir().unwrap();
    let text = concat!(
        "{\"type\":\"session\",\"id\":\"11111111-2222-4333-8444-555555555555\",\"timestamp\":\"2025-01-01T00:00:00.000Z\",\"cwd\":\"/w/old\"}\n",
        "{\"type\":\"message\",\"timestamp\":\"2025-01-01T00:00:01.000Z\",\"message\":{\"role\":\"user\",\"content\":\"one\",\"timestamp\":1}}\n",
        "{\"type\":\"message\",\"timestamp\":\"2025-01-01T00:00:02.000Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"two\"}],\"timestamp\":2}}\n",
    );
    write(&dir.path().join("sessions/--w-old--/x_11111111-2222-4333-8444-555555555555.jsonl"), text);
    let adapter = PiAdapter::with_root(AgentKind::Pi, dir.path());
    let session = adapter.sessions(&SessionFilter::default()).unwrap().remove(0);
    let ir = adapter.export_ir(&session).unwrap();
    assert_eq!(ir.messages.iter().flat_map(|m| texts(&m.parts)).collect::<Vec<_>>(), ["one", "two"]);
}

#[test]
fn resume_runs_the_file_in_its_own_directory_and_each_agent_keeps_to_its_store() {
    let dir = tempfile::tempdir().unwrap();
    let file = omp_store(dir.path());
    let omp = PiAdapter::with_root(AgentKind::Omp, dir.path());
    let session = omp.sessions(&SessionFilter::default()).unwrap().remove(0);
    let cmd = omp.resume_command(&session).unwrap();
    assert_eq!(cmd.get_program(), "omp");
    assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["--resume".as_ref(), file.as_os_str()]);
    assert_eq!(cmd.get_current_dir(), Some(Path::new("/w/proj")));

    let pi = PiAdapter::with_root(AgentKind::Pi, dir.path());
    let as_pi = pi.sessions(&SessionFilter::default()).unwrap().remove(0);
    let cmd = pi.resume_command(&as_pi).unwrap();
    assert_eq!((cmd.get_program(), cmd.get_args().next()), ("pi".as_ref(), Some("--session".as_ref())));

    let only_pi = SessionFilter { agent: Some(AgentKind::Pi), ..SessionFilter::default() };
    assert!(omp.sessions(&only_pi).unwrap().is_empty());
}

/// omp's side calls append a `model_usage` entry after the turn they belong
/// to; when it names the active leaf, that leaf is the conversation, not
/// the usage entry's own chain.
#[test]
fn a_trailing_model_usage_names_the_leaf() {
    let dir = tempfile::tempdir().unwrap();
    let mut text = String::from("{\"type\":\"session\",\"version\":3,\"id\":\"01a19e5a-0000-7000-8000-0000000000aa\",\"timestamp\":\"2026-11-02T00:00:00.000Z\",\"cwd\":\"/w/proj\"}\n");
    text += &pi_line("u0000001", None, "00:01", r#""type":"message","message":{"role":"user","content":"which?","timestamp":1}"#);
    text += &pi_line("a0000001", Some("u0000001"), "00:02", r#""type":"message","message":{"role":"assistant","content":[{"type":"text","text":"kept"}],"timestamp":2}"#);
    text += &pi_line("a0000002", Some("u0000001"), "00:03", r#""type":"message","message":{"role":"assistant","content":[{"type":"text","text":"abandoned"}],"timestamp":3}"#);
    text += &pi_line("m0000001", Some("a0000002"), "00:04", r#""type":"model_usage","activeLeafId":"a0000001","usage":{"input":1,"output":1}"#);
    write(&dir.path().join("sessions/-proj/x_y.jsonl"), &text);
    let adapter = PiAdapter::with_root(AgentKind::Omp, dir.path());
    let session = adapter.sessions(&SessionFilter::default()).unwrap().remove(0);
    let ir = adapter.export_ir(&session).unwrap();
    assert_eq!(ir.messages.iter().flat_map(|m| texts(&m.parts)).collect::<Vec<_>>(), ["which?", "kept"]);
}
