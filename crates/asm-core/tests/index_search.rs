//! Search index: content extraction, ranking, filters, and above all the
//! incremental behavior — an index that silently goes stale, or that
//! re-extracts everything every time, is the failure mode that matters.

use std::sync::Mutex;
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
        size_bytes: None,
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
    bodies: Mutex<std::collections::HashMap<String, IrSession>>,
    calls: Mutex<Vec<String>>,
}

impl Exporter {
    fn new() -> Self {
        Exporter { bodies: Mutex::new(Default::default()), calls: Mutex::new(Vec::new()) }
    }
    fn set(&self, id: &str, ir: IrSession) {
        self.bodies.lock().unwrap().insert(id.to_string(), ir);
    }
    fn export(&self, s: &Session) -> Result<IrSession, CoreError> {
        self.calls.lock().unwrap().push(s.handle.native_id.clone());
        self.bodies
            .lock().unwrap()
            .get(&s.handle.native_id)
            .cloned()
            .ok_or_else(|| CoreError::Invalid { msg: "no fixture".into() })
    }
    fn take_calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().drain(..).collect()
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

/// `asm index --agent codex` reads codex: what it says nothing about is
/// kept, not taken for gone.
#[test]
fn a_subset_refresh_leaves_the_rest_of_the_index_alone() {
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

    std::fs::write(&ta, "a and more").unwrap();
    exporter.set("a", ir("a", vec![text_message(IrRole::User, "apricot and cherry")]));
    let report = index.refresh_subset(&both[..1], |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!((report.reindexed, report.removed), (1, 0));
    assert_eq!(index.stats().unwrap().sessions, 2);
    assert_eq!(hits(&index, "cherry"), 1);
    assert_eq!(hits(&index, "blueberry"), 1, "b was outside the subset and is still there");
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

/// A refresh says how far it has got, and a second one has nothing to do:
/// on a busy machine both matter more than anything else about indexing.
#[test]
fn refresh_reports_progress_and_the_second_one_reads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let exporter = Exporter::new();
    let sessions: Vec<Session> = (0..5)
        .map(|i| {
            let id = format!("s{i}");
            let transcript = dir.path().join(format!("{id}.jsonl"));
            std::fs::write(&transcript, format!("body of {id}")).unwrap();
            exporter.set(&id, ir(&id, vec![text_message(IrRole::User, &format!("about {id}"))]));
            session(&id, &transcript, "/proj/one")
        })
        .collect();

    let mut index = open_index(dir.path());
    let mut seen: Vec<(usize, usize)> = Vec::new();
    let report = index
        .refresh_with(&sessions, |s| exporter.export(s), |p| seen.push((p.done, p.total)))
        .unwrap();
    assert_eq!(report.reindexed, 5);
    assert_eq!(seen.first(), Some(&(0, 5)), "it says how much there is before it starts");
    assert_eq!(seen.last(), Some(&(5, 5)), "and counts every session");
    assert!(seen.windows(2).all(|w| w[0].0 <= w[1].0), "progress never goes backwards");
    assert_eq!(exporter.take_calls().len(), 5);

    let mut seen = Vec::new();
    let report = index
        .refresh_with(&sessions, |s| exporter.export(s), |p| seen.push((p.done, p.total)))
        .unwrap();
    assert_eq!((report.reindexed, report.unchanged), (0, 5));
    assert!(exporter.take_calls().is_empty(), "nothing is read twice");
    assert_eq!(seen, vec![(0, 0)], "nothing to do, and it says so at once");
}

/// A session is stamped with what it was when asm started reading it, not
/// with what it became. An agent writing a turn while the transcript is
/// read would otherwise leave the index believing it had that turn.
#[test]
fn a_turn_written_while_the_session_is_read_is_not_marked_as_indexed() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("a.jsonl");
    std::fs::write(&transcript, "first").unwrap();

    let exporter = Exporter::new();
    exporter.set("a", ir("a", vec![text_message(IrRole::User, "first content")]));
    let sessions = vec![session("a", &transcript, "/proj/one")];
    let mut index = open_index(dir.path());

    // The agent appends while asm is reading the file.
    let growing = |s: &Session| {
        let out = exporter.export(s);
        std::fs::write(&transcript, "first and then a new turn").unwrap();
        out
    };
    let report = index.refresh_with(&sessions, growing, |_| {}).unwrap();
    assert_eq!(report.reindexed, 1);

    exporter.set(
        "a",
        ir("a", vec![text_message(IrRole::User, "first content and the new turn")]),
    );
    let report = index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!(report.reindexed, 1, "the appended turn is read on the next refresh");
    assert_eq!(report.unchanged, 0);
    let hits = index
        .search(&SearchQuery { text: "new turn".into(), limit: 5, ..Default::default() })
        .unwrap();
    assert_eq!(hits.len(), 1, "and it is findable");
}

/// What the index recorded for each session's rows: `(native_id, first
/// rowid, last rowid, message count)`, read behind the index's back.
fn recorded(db: &std::path::Path) -> Vec<(String, Option<i64>, Option<i64>, i64)> {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare("SELECT native_id, first_rowid, last_rowid, message_count FROM indexed_session ORDER BY native_id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn hits(index: &Index, text: &str) -> usize {
    index.search(&SearchQuery { text: text.into(), limit: 50, ..Default::default() }).unwrap().len()
}

/// Change a session's transcript, so that its fingerprint moves: it is
/// longer each round, because the file's mtime alone is not trusted to.
fn touch(session: &Session, round: usize) {
    let SessionLocation::JsonlFile { path } = &session.handle.location else { unreachable!() };
    std::fs::write(path, "x".repeat(round + 2)).unwrap();
}

/// Sessions of 2, 3 and 1 messages, `b` the middle one, not yet indexed.
fn three_sessions(dir: &std::path::Path, exporter: &Exporter) -> Vec<Session> {
    let words = [
        ("a", vec!["apricot one", "apricot two"]),
        ("b", vec!["bilberry one", "bilberry two", "bilberry three"]),
        ("c", vec!["cherry one"]),
    ];
    words
        .into_iter()
        .map(|(id, texts)| {
            let transcript = dir.join(format!("{id}.jsonl"));
            std::fs::write(&transcript, id).unwrap();
            let messages = texts.into_iter().map(|t| text_message(IrRole::User, t)).collect();
            exporter.set(id, ir(id, messages));
            session(id, &transcript, "/proj/one")
        })
        .collect()
}

/// The session's rows are deleted by the range recorded for them, which is
/// only right if the range really is the session's rows and nobody else's.
#[test]
fn a_reindexed_session_leaves_its_neighbours_rows_alone() {
    let dir = tempfile::tempdir().unwrap();
    let exporter = Exporter::new();
    let sessions = three_sessions(dir.path(), &exporter);
    let mut index = open_index(dir.path());
    index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!(exporter.take_calls().len(), 3);

    // One unbroken run of rowids per session, as many as it has messages,
    // and no two sessions share one.
    let db = dir.path().join("index/sessions.db");
    let ranges = recorded(&db);
    let mut seen = Vec::new();
    for (id, first, last, count) in &ranges {
        let (first, last) = (first.expect(id), last.expect(id));
        assert_eq!(last - first + 1, *count, "{id}: a run as long as its messages");
        assert!(seen.iter().all(|r: &(i64, i64)| last < r.0 || first > r.1), "{id}: ranges overlap");
        seen.push((first, last));
    }

    // b grows and shrinks and grows: its old rows go each time, its new ones
    // arrive, and a and c are never touched.
    let rounds = [
        vec!["bilberry"],
        vec!["gooseberry one", "gooseberry two", "gooseberry three", "gooseberry four"],
        vec!["loganberry"],
    ];
    for (round, texts) in rounds.into_iter().enumerate() {
        touch(&sessions[1], round);
        exporter.set("b", ir("b", texts.iter().map(|t| text_message(IrRole::User, t)).collect()));
        let report = index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();
        assert_eq!(report.reindexed, 1, "round {round}");
        assert_eq!(exporter.take_calls(), vec!["b"]);
        assert_eq!(hits(&index, "apricot"), 2, "round {round}: a is intact");
        assert_eq!(hits(&index, "cherry"), 1, "round {round}: c is intact");
        assert_eq!(hits(&index, texts[0].split(' ').next().unwrap()), texts.len(), "round {round}: new rows");
        assert_eq!(index.stats().unwrap().messages, 2 + texts.len() + 1, "round {round}: no stale rows");
    }
    assert_eq!(hits(&index, "bilberry"), 0);
    assert_eq!(hits(&index, "gooseberry"), 0);
}

/// A range that does not account for the session's rows is not believed:
/// the delete still names the session, and anything it missed is found by
/// the scan. Wrong bookkeeping costs time, never a stale or a foreign row.
#[test]
fn a_range_that_does_not_fit_the_session_falls_back_to_scanning() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index/sessions.db");
    let exporter = Exporter::new();
    let sessions = three_sessions(dir.path(), &exporter);
    let mut index = open_index(dir.path());
    index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();

    let tamper = |first: Option<i64>, last: Option<i64>| {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE indexed_session SET first_rowid = ?1, last_rowid = ?2 WHERE native_id = 'b'",
            rusqlite::params![first, last],
        )
        .unwrap();
    };
    let rows = recorded(&db);
    let (a, b) = (&rows[0], &rows[1]);
    let cases = [
        ("unknown", None, None),
        ("a neighbour's rows", a.1, a.2),
        ("too narrow", b.1, b.1),
        ("too wide", Some(1), Some(1_000_000)),
        ("nowhere", Some(1_000_000), Some(1_000_001)),
    ];
    for (round, (why, first, last)) in cases.into_iter().enumerate() {
        tamper(first, last);
        touch(&sessions[1], round);
        let word = format!("fruit{round}");
        exporter.set("b", ir("b", vec![text_message(IrRole::User, &word), text_message(IrRole::User, &word)]));
        let report = index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();
        assert_eq!(report.reindexed, 1, "{why}");
        assert_eq!(hits(&index, &word), 2, "{why}: the new rows");
        assert_eq!(hits(&index, "bilberry"), 0, "{why}: the original rows are gone");
        if round > 0 {
            assert_eq!(hits(&index, &format!("fruit{}", round - 1)), 0, "{why}: so are the last round's");
        }
        assert_eq!(hits(&index, "apricot"), 2, "{why}: a is intact");
        assert_eq!(hits(&index, "cherry"), 1, "{why}: c is intact");
        assert_eq!(index.stats().unwrap().messages, 5, "{why}");
    }

    // The same when the session goes away rather than changing.
    tamper(Some(1_000_000), Some(1_000_001));
    let others = [sessions[0].clone(), sessions[2].clone()];
    let report = index.refresh_with(&others, |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(hits(&index, "fruit4"), 0, "a vanished session's rows go, wherever the range says they are");
    assert_eq!((hits(&index, "apricot"), hits(&index, "cherry")), (2, 1));
    assert_eq!(index.stats().unwrap().messages, 3);
}

/// An index written before rowid ranges were recorded has a table the new
/// code cannot write to; it is discarded like any other old schema.
#[test]
fn an_index_from_before_rowid_ranges_is_rebuilt_and_usable() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index/sessions.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let agent = AgentKind::ClaudeCode.to_string();
    let conn = rusqlite::Connection::open(&db).unwrap();
    // Schema version 3, as shipped.
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE indexed_session (
             agent TEXT NOT NULL, native_id TEXT NOT NULL, title TEXT, project_root TEXT,
             updated_ms INTEGER, status TEXT, fingerprint TEXT,
             message_count INTEGER NOT NULL DEFAULT 0, indexed_at_ms INTEGER NOT NULL,
             PRIMARY KEY (agent, native_id));
         CREATE INDEX indexed_session_project ON indexed_session (project_root);
         CREATE VIRTUAL TABLE message_fts USING fts5(
             text, agent UNINDEXED, native_id UNINDEXED, role UNINDEXED, seq UNINDEXED, ts UNINDEXED,
             tokenize = 'unicode61 remove_diacritics 2');
         INSERT INTO meta VALUES ('schema_version', '3');",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO indexed_session (agent, native_id, title, project_root, status, fingerprint, message_count, indexed_at_ms)
         VALUES (?1, 'a', 't', '/p', 'idle', 'file:1:1', 1, 0)",
        [&agent],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO message_fts (text, agent, native_id, role, seq, ts) VALUES ('persimmon', ?1, 'a', 'user', 0, NULL)",
        [&agent],
    )
    .unwrap();
    drop(conn);

    let mut index = Index::open_at(&db).unwrap();
    assert_eq!(index.stats().unwrap().sessions, 0, "the old rows are not read");
    assert_eq!(hits(&index, "persimmon"), 0);

    let transcript = dir.path().join("a.jsonl");
    std::fs::write(&transcript, "a").unwrap();
    let exporter = Exporter::new();
    exporter.set("a", ir("a", vec![text_message(IrRole::User, "kumquat")]));
    let sessions = vec![session("a", &transcript, "/p")];
    let report = index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!((report.reindexed, report.failed.len()), (1, 0));
    assert_eq!(hits(&index, "kumquat"), 1);
    assert_eq!(recorded(&db)[0].3, 1);

    // And it carries on: the next change to the session replaces its row.
    std::fs::write(&transcript, "aa").unwrap();
    exporter.set("a", ir("a", vec![text_message(IrRole::User, "mangosteen")]));
    index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!((hits(&index, "kumquat"), hits(&index, "mangosteen")), (0, 1));
}

/// Searching has to work while the indexer is mid-transaction. Opening the
/// index used to write (a tuning statement run on every open), and a write
/// waits for the indexer's — five seconds, then goes ahead without it.
#[test]
fn opening_and_searching_do_not_wait_for_a_writer() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index/sessions.db");
    let transcript = dir.path().join("a.jsonl");
    std::fs::write(&transcript, "a").unwrap();
    let exporter = Exporter::new();
    exporter.set("a", ir("a", vec![text_message(IrRole::User, "persimmon")]));
    let mut index = Index::open_at(&db).unwrap();
    index.refresh_with(&[session("a", &transcript, "/p")], |s| exporter.export(s), |_| {}).unwrap();
    drop(index);

    // The indexer, part-way through a batch.
    let writer = rusqlite::Connection::open(&db).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();

    let started = std::time::Instant::now();
    let index = Index::open_at(&db).unwrap();
    assert_eq!(hits(&index, "persimmon"), 1);
    assert!(index.stats().unwrap().sessions == 1);
    let waited = started.elapsed();
    writer.execute_batch("ROLLBACK").unwrap();
    assert!(waited < std::time::Duration::from_secs(2), "opened and searched in {waited:?}, behind a writer");
}

fn child_of(parent: &str, id: &str, transcript: &std::path::Path, project: &str) -> Session {
    Session { parent: Some(parent.to_string()), ..session(id, transcript, project) }
}

/// A hit inside a subagent session is still a hit, and says whose it is.
#[test]
fn a_hit_in_a_child_session_names_its_parent() {
    let dir = tempfile::tempdir().unwrap();
    let exporter = Exporter::new();
    let mut sessions = three_sessions(dir.path(), &exporter);
    // b becomes a child of a.
    sessions[1] = child_of("a", "b", &dir.path().join("b.jsonl"), "/proj/one");
    let mut index = open_index(dir.path());
    index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();

    let find = |word: &str| {
        index.search(&SearchQuery { text: word.into(), limit: 5, ..Default::default() }).unwrap()
    };
    let child = find("bilberry");
    assert_eq!(child.len(), 3, "children are searched like any other session");
    assert!(child.iter().all(|h| h.native_id == "b" && h.parent.as_deref() == Some("a")), "{child:?}");
    assert_eq!(find("apricot")[0].parent, None, "a root has no parent");
    assert_eq!(find("cherry")[0].parent, None);
}

/// The rowid ranges recorded for each session keep working beside children:
/// re-extracting one, and dropping one that went away, touch only its rows,
/// and the parent survives a re-extraction.
#[test]
fn children_are_reindexed_and_forgotten_by_their_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index/sessions.db");
    let exporter = Exporter::new();
    let mut sessions = three_sessions(dir.path(), &exporter);
    sessions[1] = child_of("a", "b", &dir.path().join("b.jsonl"), "/proj/one");
    sessions[2] = child_of("b", "c", &dir.path().join("c.jsonl"), "/proj/one");
    let mut index = open_index(dir.path());
    index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!(exporter.take_calls().len(), 3);
    for (id, first, last, count) in recorded(&db) {
        assert_eq!(last.unwrap() - first.unwrap() + 1, count, "{id}: its own unbroken run");
    }

    touch(&sessions[1], 0);
    exporter.set("b", ir("b", vec![text_message(IrRole::User, "gooseberry one"), text_message(IrRole::User, "gooseberry two")]));
    let report = index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!(report.reindexed, 1);
    assert_eq!(exporter.take_calls(), vec!["b"]);
    assert_eq!(hits(&index, "bilberry"), 0);
    assert_eq!(hits(&index, "gooseberry"), 2);
    assert_eq!((hits(&index, "apricot"), hits(&index, "cherry")), (2, 1), "neighbours are intact");
    let hit = &index.search(&SearchQuery { text: "gooseberry".into(), limit: 1, ..Default::default() }).unwrap()[0];
    assert_eq!(hit.parent.as_deref(), Some("a"), "re-extracted, still a child");

    // The grandchild goes away with nothing else changed.
    let report = index.refresh_with(&sessions[..2], |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(hits(&index, "cherry"), 0);
    assert_eq!((hits(&index, "apricot"), hits(&index, "gooseberry")), (2, 2));
}

/// Version 4 had no parent column; its index is dropped and rebuilt rather
/// than migrated, and the rebuilt one carries parents.
#[test]
fn an_index_from_before_parents_is_rebuilt_with_them() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index/sessions.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    // Schema version 4, as shipped.
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE indexed_session (
             agent TEXT NOT NULL, native_id TEXT NOT NULL, title TEXT, project_root TEXT,
             updated_ms INTEGER, status TEXT, fingerprint TEXT,
             message_count INTEGER NOT NULL DEFAULT 0,
             first_rowid INTEGER, last_rowid INTEGER, indexed_at_ms INTEGER NOT NULL,
             PRIMARY KEY (agent, native_id));
         CREATE INDEX indexed_session_project ON indexed_session (project_root);
         CREATE VIRTUAL TABLE message_fts USING fts5(
             text, agent UNINDEXED, native_id UNINDEXED, role UNINDEXED, seq UNINDEXED, ts UNINDEXED,
             tokenize = 'unicode61 remove_diacritics 2');
         INSERT INTO meta VALUES ('schema_version', '4');
         INSERT INTO indexed_session (agent, native_id, title, project_root, status, fingerprint, message_count, indexed_at_ms)
             VALUES ('claude-code', 'a', 't', '/p', 'idle', 'file:1:1', 1, 0);
         INSERT INTO message_fts (text, agent, native_id, role, seq, ts)
             VALUES ('persimmon', 'claude-code', 'a', 'user', 0, NULL);",
    )
    .unwrap();
    drop(conn);

    let mut index = Index::open_at(&db).unwrap();
    assert_eq!(asm_core::index::SCHEMA_VERSION, 5);
    assert_eq!(index.stats().unwrap().sessions, 0, "the old rows are not read");

    let transcript = dir.path().join("kid.jsonl");
    std::fs::write(&transcript, "k").unwrap();
    let exporter = Exporter::new();
    exporter.set("kid", ir("kid", vec![text_message(IrRole::User, "kumquat")]));
    let sessions = vec![child_of("a", "kid", &transcript, "/p")];
    let report = index.refresh_with(&sessions, |s| exporter.export(s), |_| {}).unwrap();
    assert_eq!((report.reindexed, report.failed.len()), (1, 0));
    let found = index.search(&SearchQuery { text: "kumquat".into(), limit: 5, ..Default::default() }).unwrap();
    assert_eq!(found[0].parent.as_deref(), Some("a"));
}
