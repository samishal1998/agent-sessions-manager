//! What a program driving asm relies on, below the CLI: the capability
//! report, list queries, the message view and the change watch, over fixture
//! stores. OpenCode appears as 1.x here and as 2.x beside its own fixture
//! (`adapter/opencode/v2.rs`), because a process has one OpenCode store.

use std::fs;
use std::path::Path;

use asm_core::adapter::opencode::OpenCodeAdapter;
use asm_core::adapter::{Adapter, AgentRead, SessionFilter};
use asm_core::model::AgentKind;
use asm_core::ops::{self, ChangeKind, ListQuery, MessageQuery, SessionWatch};

const CLAUDE_A: &str = "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43";
const CLAUDE_B: &str = "7f3a1c99-2d4e-4b91-9a05-6c7e8f201b44";

fn claude_line(id: &str, ts: &str, text: &str) -> String {
    format!(
        r#"{{"type":"user","uuid":"u1","parentUuid":null,"isSidechain":false,"timestamp":"{ts}","cwd":"/w/app","sessionId":"{id}","message":{{"role":"user","content":"{text}"}}}}"#
    )
}

fn opencode_v1(db: &Path) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT, vcs TEXT, time_created INTEGER, time_updated INTEGER);
         CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT, slug TEXT, directory TEXT,
             title TEXT, version TEXT, agent TEXT, model TEXT, cost REAL, tokens_input INTEGER,
             tokens_output INTEGER, tokens_cache_read INTEGER, tokens_cache_write INTEGER,
             time_created INTEGER, time_updated INTEGER, time_archived INTEGER);
         CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
         CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
         INSERT INTO project VALUES ('p1', '/w/app', 'git', 1, 1);
         INSERT INTO session VALUES ('ses_one', 'p1', NULL, 'slug-one', '/w/app', 'Plan', '1.17.18', 'build', NULL,
             NULL, NULL, NULL, NULL, NULL, 1800000000000, 1800000010000, NULL);
         INSERT INTO message VALUES ('m1', 'ses_one', 1, 1, '{\"role\":\"user\"}');
         INSERT INTO part VALUES ('p1', 'm1', 'ses_one', 1, 1, '{\"type\":\"text\",\"text\":\"what is in here\"}');
         INSERT INTO message VALUES ('m2', 'ses_one', 2, 2, '{\"role\":\"assistant\"}');
         INSERT INTO part VALUES ('p2', 'm2', 'ses_one', 2, 2, '{\"type\":\"text\",\"text\":\"let me look\"}');
         INSERT INTO part VALUES ('p3', 'm2', 'ses_one', 3, 3, '{\"type\":\"tool\",\"callID\":\"c1\",\"tool\":\"bash\",\"state\":{\"status\":\"completed\",\"input\":{\"command\":\"ls\"},\"output\":\"a b\"}}');",
    )
    .unwrap();
}

/// OpenCode 1.x through the IR: the same message view every agent has.
#[test]
fn opencode_1x_messages_and_schema() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("opencode.db");
    opencode_v1(&db);
    let adapter = OpenCodeAdapter::with_db(&db);
    let session = adapter.sessions(&SessionFilter::default()).unwrap().remove(0);
    let ir = adapter.export_ir(&session).unwrap();

    let all = ops::messages_from_ir(&ir, &MessageQuery::default());
    assert_eq!(all.iter().map(|r| r.index).collect::<Vec<_>>(), [0, 1]);
    let tool = ops::messages_from_ir(&ir, &MessageQuery { role: Some("tool".into()), ..Default::default() });
    assert_eq!(tool.len(), 1, "only the message with the tool call");
    let text = ops::messages_from_ir(&ir, &MessageQuery { text_only: true, tail: Some(1), ..Default::default() });
    assert_eq!(text.len(), 1);
    assert_eq!(text[0].index, 1);
    assert_eq!(text[0].parts.len(), 1, "the tool parts are dropped");

    assert_eq!(ops::store_schema(&Adapter::OpenCode(adapter)), (Some("v1"), None));
}

/// The one test that owns this process's stores: capabilities, list queries
/// and the watch, in the order they need the stores to change.
#[test]
fn capabilities_list_queries_and_the_watch() {
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
        // Safety: set once, before this process's other tests read the stores
        // (this file has no other test that does).
        unsafe { std::env::set_var(var, root.join(sub)) };
    }
    let project = root.join("claude/projects/-w-app");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join(format!("{CLAUDE_A}.jsonl")), claude_line(CLAUDE_A, "2026-10-09T10:00:00.000Z", "first")).unwrap();
    fs::create_dir_all(root.join("xdg-data/opencode")).unwrap();
    opencode_v1(&root.join("xdg-data/opencode/opencode.db"));

    // --- capabilities: every agent, installed or not, nothing counted.
    let report = ops::capabilities_report();
    assert_eq!(report.json_contract_version, asm_core::contract::JSON_CONTRACT_VERSION);
    assert_eq!(report.agents.iter().map(|a| a.agent).collect::<Vec<_>>(), AgentKind::ALL);
    let by = |kind| report.agents.iter().find(|a| a.agent == kind).unwrap();
    assert!(by(AgentKind::ClaudeCode).installed && by(AgentKind::ClaudeCode).schema.is_none());
    assert_eq!(by(AgentKind::OpenCode).schema, Some("v1"));
    assert!(by(AgentKind::OpenCode).capabilities.archive, "1.x archives with a flag");
    let codex = by(AgentKind::Codex);
    assert!(!codex.installed && codex.store_root.is_none());
    assert!(codex.capabilities.export_ir, "what asm can do is reported for an agent that is not here");

    // --- doctor carries the schema too.
    let doctor = ops::doctor().unwrap();
    let oc = doctor.agents.iter().find(|a| a.agent == AgentKind::OpenCode).unwrap();
    assert_eq!((oc.schema, oc.unmigrated), (Some("v1"), None));
    assert!(doctor.warnings.is_empty());

    // --- list queries.
    let all = ops::list_sessions(&SessionFilter::default()).unwrap();
    assert_eq!(all.len(), 2);
    let q = |state: Option<&str>, text: Option<&str>| ListQuery { state: state.map(Into::into), text: text.map(Into::into) };
    assert_eq!(all.iter().filter(|s| q(Some("idle"), None).keep(s)).count(), 2);
    assert_eq!(all.iter().filter(|s| q(Some("live"), None).keep(s)).count(), 0);
    assert_eq!(all.iter().filter(|s| q(None, Some("PLAN")).keep(s)).count(), 1, "title, any case");
    assert_eq!(all.iter().filter(|s| q(None, Some("7f3a1c88")).keep(s)).count(), 1, "id");
    assert_eq!(all.iter().filter(|s| q(None, Some("/w/app")).keep(s)).count(), 2, "project path");

    // --- the watch: the first look learns, the next says what moved.
    let filter = SessionFilter::default();
    let nothing = ListQuery::default();
    let mut watch = SessionWatch::new();
    let (first, problems) = watch.poll(&filter, &nothing);
    assert!(first.is_empty() && problems.is_empty(), "the baseline is not news");
    assert!(watch.poll(&filter, &nothing).0.is_empty(), "nothing moved, nothing said");

    fs::write(project.join(format!("{CLAUDE_B}.jsonl")), claude_line(CLAUDE_B, "2026-10-09T11:00:00.000Z", "second")).unwrap();
    let (added, _) = watch.poll(&filter, &nothing);
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].kind, ChangeKind::Added);
    assert_eq!(added[0].session.handle.native_id, CLAUDE_B);

    let conn = rusqlite::Connection::open(root.join("xdg-data/opencode/opencode.db")).unwrap();
    conn.execute("UPDATE session SET title = 'Renamed', time_updated = time_updated + 5 WHERE id = 'ses_one'", []).unwrap();
    let (changed, _) = watch.poll(&filter, &nothing);
    assert_eq!(changed.len(), 1);
    assert_eq!((changed[0].kind, changed[0].session.title.as_deref()), (ChangeKind::Changed, Some("Renamed")));

    fs::remove_file(project.join(format!("{CLAUDE_A}.jsonl"))).unwrap();
    let (gone, _) = watch.poll(&filter, &nothing);
    assert_eq!(gone.len(), 1);
    assert_eq!((gone[0].kind, gone[0].session.handle.native_id.as_str()), (ChangeKind::Removed, CLAUDE_A));

    // A watch with a query only hears of what the query keeps.
    let mut live_only = SessionWatch::new();
    live_only.poll(&filter, &q(Some("live"), None));
    fs::write(project.join("cccccccc-1111-2222-3333-444444444444.jsonl"), claude_line("cccccccc-1111-2222-3333-444444444444", "2026-10-09T12:00:00.000Z", "third")).unwrap();
    assert!(live_only.poll(&filter, &q(Some("live"), None)).0.is_empty());
}
