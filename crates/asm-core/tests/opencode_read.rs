//! OpenCode read-adapter tests against a fixture SQLite database that
//! mirrors the 1.17.x schema, including a legacy 1.2.x-generation row
//! (NULL directory/agent/model) and a subagent child session.

use std::path::Path;

use rusqlite::Connection;

use asm_core::adapter::opencode::OpenCodeAdapter;
use asm_core::adapter::{AgentRead, SessionFilter};
use asm_core::model::SessionStatus;

fn write_store(db: &Path) {
    let conn = Connection::open(db).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT, vcs TEXT,
            time_created INTEGER, time_updated INTEGER);
        CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT,
            slug TEXT, directory TEXT, title TEXT, version TEXT, agent TEXT,
            model TEXT, cost REAL, tokens_input INTEGER, tokens_output INTEGER,
            tokens_cache_read INTEGER, tokens_cache_write INTEGER,
            time_created INTEGER, time_updated INTEGER, time_archived INTEGER);

        INSERT INTO project VALUES
            ('global', '/', NULL, 0, 0),
            ('9a2186bccac610043bfe58e9bdedcbaf59dbf7f1',
             '/home/user/projects/openrpc', 'git', 1700000000000, 1700000001000);

        -- modern root session
        INSERT INTO session VALUES
            ('ses_modern000000000000000000', '9a2186bccac610043bfe58e9bdedcbaf59dbf7f1',
             NULL, 'misty-squid', '/home/user/projects/openrpc',
             'OpenRPC tooling plan', '1.17.18', 'plan',
             '{"id":"gpt-5.6-sol-pro","providerID":"openai","variant":"xhigh"}',
             0.42, 1000, 2000, 300, 400, 1755000000000, 1755000100000, NULL);

        -- legacy 1.2.x row: empty directory, NULL agent/model
        INSERT INTO session VALUES
            ('ses_legacy000000000000000000', '9a2186bccac610043bfe58e9bdedcbaf59dbf7f1',
             NULL, 'crisp-tiger', '', 'Old generation session', '1.2.22', NULL,
             NULL, NULL, NULL, NULL, NULL, NULL, 1754000000000, 1754000100000, NULL);

        -- archived session
        INSERT INTO session VALUES
            ('ses_archived00000000000000000', '9a2186bccac610043bfe58e9bdedcbaf59dbf7f1',
             NULL, 'dull-owl', '/home/user/projects/openrpc', 'Done work', '1.17.18',
             'build', NULL, NULL, NULL, NULL, NULL, NULL,
             1753000000000, 1753000100000, 1753500000000);

        -- subagent child: hidden unless include_children
        INSERT INTO session VALUES
            ('ses_child0000000000000000000', '9a2186bccac610043bfe58e9bdedcbaf59dbf7f1',
             'ses_modern000000000000000000', 'tiny-fish',
             '/home/user/projects/openrpc', 'Review docs (@general subagent)',
             '1.17.18', 'general', NULL, NULL, NULL, NULL, NULL, NULL,
             1755000050000, 1755000060000, NULL);
        "#,
    )
    .unwrap();
}

fn adapter(dir: &Path) -> OpenCodeAdapter {
    let db = dir.join("opencode.db");
    write_store(&db);
    OpenCodeAdapter::with_db(db)
}

#[test]
fn lists_root_sessions_with_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = adapter(dir.path()).sessions(&SessionFilter::default()).unwrap();

    assert_eq!(sessions.len(), 3, "children hidden by default");

    let modern = sessions
        .iter()
        .find(|s| s.handle.native_id == "ses_modern000000000000000000")
        .unwrap();
    assert_eq!(modern.title.as_deref(), Some("OpenRPC tooling plan"));
    assert_eq!(modern.slug.as_deref(), Some("misty-squid"));
    assert_eq!(modern.project_root, Path::new("/home/user/projects/openrpc"));
    assert_eq!(modern.model.as_deref(), Some("gpt-5.6-sol-pro"));
    assert_eq!(modern.agent_version.as_deref(), Some("1.17.18"));
    assert_eq!(modern.usage.cost_usd, Some(0.42));
    assert_eq!(modern.usage.input_tokens, Some(1000));
    assert_eq!(modern.status, SessionStatus::Idle);
    // epoch-milliseconds conversion
    assert_eq!(modern.created.unwrap().as_millisecond(), 1755000000000);
}

#[test]
fn legacy_row_falls_back_to_project_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = adapter(dir.path()).sessions(&SessionFilter::default()).unwrap();

    let legacy = sessions
        .iter()
        .find(|s| s.handle.native_id == "ses_legacy000000000000000000")
        .unwrap();
    assert_eq!(
        legacy.project_root,
        Path::new("/home/user/projects/openrpc"),
        "empty directory falls back to project.worktree"
    );
    assert!(legacy.model.is_none());
}

#[test]
fn archived_and_children_flags() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = adapter(dir.path());

    let default = adapter.sessions(&SessionFilter::default()).unwrap();
    let archived = default
        .iter()
        .find(|s| s.handle.native_id == "ses_archived00000000000000000")
        .unwrap();
    assert_eq!(archived.status, SessionStatus::Archived);

    let all = adapter
        .sessions(&SessionFilter { include_children: true, ..SessionFilter::default() })
        .unwrap();
    assert_eq!(all.len(), 4);
    let child = all
        .iter()
        .find(|s| s.handle.native_id == "ses_child0000000000000000000")
        .unwrap();
    assert_eq!(child.parent.as_deref(), Some("ses_modern000000000000000000"));
}

#[test]
fn projects_report_root_session_counts() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = adapter(dir.path()).sessions(&SessionFilter::default()).unwrap();
    let projects = asm_core::ops::group_projects(&sessions, |_| None, |_| Vec::new());

    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].root, Path::new("/home/user/projects/openrpc"));
    assert_eq!(projects[0].session_count, 3, "root sessions only");
}
