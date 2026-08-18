//! Search index: content extraction, ranking, filters, and above all the
//! incremental behavior — an index that silently goes stale, or that
//! re-extracts everything every time, is the failure mode that matters.

use std::cell::RefCell;
use std::path::PathBuf;

use asm_core::CoreError;
use asm_core::index::{Index, SearchQuery};
use asm_core::ir::{ExtBag, IrMessage, IrPart, IrProvenance, IrRole, IrSession, PortablePath};
use asm_core::model::{
    AgentKind, Session, SessionLocation, SessionRef, SessionStatus, Usage,
};

fn session(id: &str, transcript: &std::path::Path, project: &str) -> Session {
    Session {
        handle: SessionRef {
            agent: AgentKind::ClaudeCode,
            native_id: id.to_string(),
            location: SessionLocation::JsonlFile { path: transcript.to_path_buf() },
        },
        title: Some(format!("Session {id}")),
        slug: None,
        project_root: PathBuf::from(project),
        git_branch: None,
        created: None,
        updated: "2026-08-01T10:00:00Z".parse().ok(),
        model: None,
        usage: Usage::default(),
        status: SessionStatus::Idle,
        parent: None,
        agent_version: None,
    }
}

fn ir(id: &str, messages: Vec<IrMessage>) -> IrSession {
    IrSession {
        ir_version: 1,
        source: IrProvenance {
            agent: AgentKind::ClaudeCode,
            native_id: id.to_string(),
            agent_version: None,
            exported_at: jiff::Timestamp::UNIX_EPOCH,
            exporter_version: "test".into(),
        },
        title: Some(format!("Session {id}")),
        slug: None,
        project_path: PortablePath("/p".into()),
        created: None,
        updated: None,
        model: None,
        usage: Usage::default(),
        messages,
        extensions: ExtBag::new(),
    }
}

fn text_message(role: IrRole, text: &str) -> IrMessage {
    IrMessage {
        role,
        timestamp: "2026-08-01T10:00:00Z".parse().ok(),
        parts: vec![IrPart::Text { text: text.into() }],
        source_id: None,
        extensions: ExtBag::new(),
    }
}

/// An exporter that counts calls, so "did it re-extract?" is observable.
struct Exporter {
    bodies: RefCell<std::collections::HashMap<String, IrSession>>,
    calls: RefCell<Vec<String>>,
}

impl Exporter {
    fn new() -> Self {
        Exporter { bodies: RefCell::new(Default::default()), calls: RefCell::new(Vec::new()) }
    }
    fn set(&self, id: &str, ir: IrSession) {
        self.bodies.borrow_mut().insert(id.to_string(), ir);
    }
    fn export(&self, s: &Session) -> Result<IrSession, CoreError> {
        self.calls.borrow_mut().push(s.handle.native_id.clone());
        self.bodies
            .borrow()
            .get(&s.handle.native_id)
            .cloned()
            .ok_or_else(|| CoreError::Invalid { msg: "no fixture".into() })
    }
    fn take_calls(&self) -> Vec<String> {
        self.calls.borrow_mut().drain(..).collect()
    }
}

fn open_index(dir: &std::path::Path) -> Index {
    Index::open_at(&dir.join("index/sessions.db")).unwrap()
}

#[test]
fn indexes_and_finds_conversation_text() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("a.jsonl");
    std::fs::write(&transcript, "x").unwrap();

    let exporter = Exporter::new();
    exporter.set(
        "a",
        ir(
            "a",
            vec![
                text_message(IrRole::User, "how do I encode the project directory"),
                text_message(IrRole::Assistant, "the encoder replaces non-alphanumerics"),
            ],
        ),
    );

    let sessions = vec![session("a", &transcript, "/proj/one")];
    let mut index = open_index(dir.path());
    let report = index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!(report.reindexed, 1);
    assert_eq!(report.messages_indexed, 2);

    let hits = index
        .search(&SearchQuery { text: "encoder".into(), ..SearchQuery::default() })
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].native_id, "a");
    assert_eq!(hits[0].role, "assistant");
    // Matches are delimited with control characters, never printable ones:
    // transcript content is full of brackets and quotes.
    let marked = format!(
        "{}encoder{}",
        asm_core::index::MATCH_START,
        asm_core::index::MATCH_END
    );
    assert!(hits[0].snippet.contains(&marked), "{:?}", hits[0].snippet);

    // Multiple words are AND-ed, not OR-ed.
    assert_eq!(
        index
            .search(&SearchQuery { text: "encode directory".into(), ..SearchQuery::default() })
            .unwrap()
            .len(),
        1
    );
    assert!(
        index
            .search(&SearchQuery { text: "encode absent".into(), ..SearchQuery::default() })
            .unwrap()
            .is_empty()
    );
}

#[test]
fn unchanged_sessions_are_not_re_extracted() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("a.jsonl");
    std::fs::write(&transcript, "hello").unwrap();

    let exporter = Exporter::new();
    exporter.set("a", ir("a", vec![text_message(IrRole::User, "first content")]));
    let sessions = vec![session("a", &transcript, "/proj/one")];

    let mut index = open_index(dir.path());
    index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!(exporter.take_calls(), vec!["a"], "first pass extracts");

    let report = index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!(report.unchanged, 1);
    assert_eq!(report.reindexed, 0);
    assert!(exporter.take_calls().is_empty(), "unchanged session must not be re-read");
}

#[test]
fn a_changed_transcript_is_re_extracted_and_old_text_disappears() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("a.jsonl");
    std::fs::write(&transcript, "hello").unwrap();

    let exporter = Exporter::new();
    exporter.set("a", ir("a", vec![text_message(IrRole::User, "watermelon")]));
    let sessions = vec![session("a", &transcript, "/proj/one")];

    let mut index = open_index(dir.path());
    index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();
    exporter.take_calls();
    assert_eq!(
        index.search(&SearchQuery { text: "watermelon".into(), ..Default::default() }).unwrap().len(),
        1
    );

    // Growing the transcript changes size and mtime — the fingerprint moves.
    std::fs::write(&transcript, "hello, plus a great deal more content").unwrap();
    exporter.set("a", ir("a", vec![text_message(IrRole::User, "cantaloupe")]));

    let report = index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!(report.reindexed, 1);
    assert_eq!(exporter.take_calls(), vec!["a"]);

    // The replaced content must be gone, not merely shadowed.
    assert!(
        index.search(&SearchQuery { text: "watermelon".into(), ..Default::default() }).unwrap().is_empty(),
        "stale rows must be deleted when a session is re-indexed"
    );
    assert_eq!(
        index.search(&SearchQuery { text: "cantaloupe".into(), ..Default::default() }).unwrap().len(),
        1
    );
}

#[test]
fn vanished_sessions_are_dropped_from_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let (ta, tb) = (dir.path().join("a.jsonl"), dir.path().join("b.jsonl"));
    std::fs::write(&ta, "a").unwrap();
    std::fs::write(&tb, "b").unwrap();

    let exporter = Exporter::new();
    exporter.set("a", ir("a", vec![text_message(IrRole::User, "apricot")]));
    exporter.set("b", ir("b", vec![text_message(IrRole::User, "blueberry")]));

    let mut index = open_index(dir.path());
    let both = vec![session("a", &ta, "/proj/one"), session("b", &tb, "/proj/two")];
    index.refresh_with(&both, |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!(index.stats().unwrap().sessions, 2);

    // b has been deleted or archived away.
    let only_a = vec![session("a", &ta, "/proj/one")];
    let report = index.refresh_with(&only_a, |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(index.stats().unwrap().sessions, 1);
    assert!(
        index.search(&SearchQuery { text: "blueberry".into(), ..Default::default() }).unwrap().is_empty()
    );
}

#[test]
fn filters_narrow_results() {
    let dir = tempfile::tempdir().unwrap();
    let (ta, tb) = (dir.path().join("a.jsonl"), dir.path().join("b.jsonl"));
    std::fs::write(&ta, "a").unwrap();
    std::fs::write(&tb, "b").unwrap();

    let exporter = Exporter::new();
    exporter.set("a", ir("a", vec![text_message(IrRole::User, "shared term here")]));
    exporter.set("b", ir("b", vec![text_message(IrRole::User, "shared term there")]));

    let mut index = open_index(dir.path());
    let sessions = vec![session("a", &ta, "/proj/one"), session("b", &tb, "/proj/two")];
    index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();

    assert_eq!(
        index.search(&SearchQuery { text: "shared".into(), ..Default::default() }).unwrap().len(),
        2
    );
    let scoped = index
        .search(&SearchQuery {
            text: "shared".into(),
            project: Some(PathBuf::from("/proj/two")),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].native_id, "b");

    // No OpenCode sessions were indexed.
    assert!(
        index
            .search(&SearchQuery {
                text: "shared".into(),
                agent: Some(AgentKind::OpenCode),
                ..Default::default()
            })
            .unwrap()
            .is_empty()
    );
}

#[test]
fn tool_calls_are_searchable_and_a_failed_export_does_not_abort_the_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let (ta, tb) = (dir.path().join("a.jsonl"), dir.path().join("b.jsonl"));
    std::fs::write(&ta, "a").unwrap();
    std::fs::write(&tb, "b").unwrap();

    let exporter = Exporter::new();
    exporter.set(
        "a",
        ir(
            "a",
            vec![IrMessage {
                role: IrRole::Assistant,
                timestamp: None,
                parts: vec![
                    IrPart::ToolCall {
                        call_id: "c1".into(),
                        name: "Bash".into(),
                        input: serde_json::json!({"command": "cargo nextest run"}),
                    },
                    IrPart::ToolResult {
                        call_id: "c1".into(),
                        output: "distinctiveoutput".into(),
                        is_error: false,
                        truncated: false,
                    },
                ],
                source_id: None,
                extensions: ExtBag::new(),
            }],
        ),
    );
    // No fixture for "b": its export fails.

    let mut index = open_index(dir.path());
    let sessions = vec![session("a", &ta, "/proj/one"), session("b", &tb, "/proj/two")];
    let report = index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();

    assert_eq!(report.reindexed, 1);
    assert_eq!(report.failed.len(), 1, "the unreadable session is reported, not fatal");
    assert!(report.failed[0].contains("b"));

    for term in ["nextest", "Bash", "distinctiveoutput"] {
        assert_eq!(
            index.search(&SearchQuery { text: term.into(), ..Default::default() }).unwrap().len(),
            1,
            "expected to find {term}"
        );
    }
}

#[test]
fn subagent_transcripts_are_searchable() {
    // A delegating session keeps most of its text inside subagent runs;
    // indexing only the agent's name would hide the majority of the corpus.
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("a.jsonl");
    std::fs::write(&transcript, "a").unwrap();

    let nested = IrMessage {
        role: IrRole::Assistant,
        timestamp: None,
        parts: vec![IrPart::Agent {
            name: "explorer".into(),
            description: Some("search the repo".into()),
            transcript: vec![
                text_message(IrRole::User, "find the sasquatch handler"),
                IrMessage {
                    role: IrRole::Assistant,
                    timestamp: None,
                    parts: vec![IrPart::ToolCall {
                        call_id: "c1".into(),
                        name: "Grep".into(),
                        input: serde_json::json!({"pattern": "chupacabra"}),
                    }],
                    source_id: None,
                    extensions: ExtBag::new(),
                },
            ],
        }],
        source_id: None,
        extensions: ExtBag::new(),
    };

    let exporter = Exporter::new();
    exporter.set("a", ir("a", vec![nested]));

    let mut index = open_index(dir.path());
    index
        .refresh_with(&[session("a", &transcript, "/p")], |s| exporter.export(s), |_| {})
        .unwrap();

    for term in ["sasquatch", "chupacabra", "explorer"] {
        assert_eq!(
            index.search(&SearchQuery { text: term.into(), ..Default::default() }).unwrap().len(),
            1,
            "expected subagent content to be searchable: {term}"
        );
    }
}

#[test]
fn punctuation_heavy_queries_do_not_error() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("a.jsonl");
    std::fs::write(&transcript, "a").unwrap();
    let exporter = Exporter::new();
    exporter.set("a", ir("a", vec![text_message(IrRole::User, "run cargo build --release now")]));

    let mut index = open_index(dir.path());
    index
        .refresh_with(&[session("a", &transcript, "/p")], |s| exporter.export(s), |_| {})
        .unwrap();

    // Bare FTS5 operators in user input must not become syntax errors.
    for query in ["--release", "\"", "a* OR", "NEAR(", "^start", "-", "()"] {
        let result = index.search(&SearchQuery { text: query.into(), ..Default::default() });
        assert!(result.is_ok(), "query {query:?} errored: {:?}", result.err());
    }
    assert_eq!(
        index.search(&SearchQuery { text: "--release".into(), ..Default::default() }).unwrap().len(),
        1
    );
}

#[test]
fn archived_sessions_are_indexed_and_labelled() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("archived.jsonl");
    std::fs::write(&transcript, "a").unwrap();

    let exporter = Exporter::new();
    exporter.set("a", ir("a", vec![text_message(IrRole::User, "quincetoken")]));

    let mut archived = session("a", &transcript, "/proj/one");
    archived.status = SessionStatus::Archived;

    let mut index = open_index(dir.path());
    index.refresh_with(&[archived], |s| exporter.export(s), |_| {}).unwrap();

    let hits = index
        .search(&SearchQuery { text: "quincetoken".into(), ..Default::default() })
        .unwrap();
    assert_eq!(hits.len(), 1, "archived sessions must stay searchable");
    assert_eq!(
        hits[0].status, "archived",
        "results must say a session needs restoring before it can be resumed"
    );
}

#[test]
fn status_and_title_stay_current_without_re_extracting() {
    // A session can go live and idle again, or be archived, without its
    // transcript changing at all — the labels must still track it.
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("a.jsonl");
    std::fs::write(&transcript, "a").unwrap();
    let exporter = Exporter::new();
    exporter.set("a", ir("a", vec![text_message(IrRole::User, "lingonberry")]));

    let mut index = open_index(dir.path());
    index
        .refresh_with(&[session("a", &transcript, "/proj/one")], |s| exporter.export(s), |_| {})
        .unwrap();
    exporter.take_calls();
    assert_eq!(
        index.search(&SearchQuery { text: "lingonberry".into(), ..Default::default() }).unwrap()[0]
            .status,
        "idle"
    );

    // Same bytes, same mtime — only the status and title differ.
    let mut archived = session("a", &transcript, "/proj/one");
    archived.status = SessionStatus::Archived;
    archived.title = Some("Renamed while archived".into());
    let report = index.refresh_with(&[archived], |s| exporter.export(s), |_| {}).unwrap();

    assert_eq!(report.unchanged, 1, "the transcript is untouched");
    assert!(exporter.take_calls().is_empty(), "no re-extraction for a label change");

    let hits =
        index.search(&SearchQuery { text: "lingonberry".into(), ..Default::default() }).unwrap();
    assert_eq!(hits[0].status, "archived", "stale status would mislabel the result");
    assert_eq!(hits[0].title.as_deref(), Some("Renamed while archived"));
}

#[test]
fn a_schema_version_change_rebuilds_rather_than_serving_stale_rows() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index/sessions.db");
    let transcript = dir.path().join("a.jsonl");
    std::fs::write(&transcript, "a").unwrap();
    let exporter = Exporter::new();
    exporter.set("a", ir("a", vec![text_message(IrRole::User, "persimmon")]));

    let mut index = Index::open_at(&db).unwrap();
    index
        .refresh_with(&[session("a", &transcript, "/p")], |s| exporter.export(s), |_| {})
        .unwrap();
    drop(index);

    // Simulate an index written by a different asm version.
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE meta SET value = '0' WHERE key = 'schema_version'", []).unwrap();
    drop(conn);

    let index = Index::open_at(&db).unwrap();
    assert_eq!(index.stats().unwrap().sessions, 0, "an old schema is discarded, not read");
    assert!(
        index.search(&SearchQuery { text: "persimmon".into(), ..Default::default() }).unwrap().is_empty()
    );
}
