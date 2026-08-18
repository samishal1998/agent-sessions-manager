//! OpenCode write-verb tests: rename/archive/unarchive (column updates),
//! delete with explicit child-row removal + session_diff cleanup, row-level
//! backups, and the busy-store guard.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use rusqlite::Connection;

use asm_core::CoreError;
use asm_core::adapter::opencode::OpenCodeAdapter;
use asm_core::adapter::{AgentRead, AgentWrite, SessionFilter};
use asm_core::model::SessionStatus;

const SESSION: &str = "ses_writetest000000000000000";

fn asm_data_dir() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        // Safety: set once per test process, before concurrent readers.
        unsafe { std::env::set_var("ASM_DATA_DIR", dir.path()) };
        dir
    });
    dir.path()
}

fn write_store(dir: &Path) -> OpenCodeAdapter {
    asm_data_dir();
    let db = dir.join("opencode.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(&format!(
        r#"
        CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT);
        CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT,
            slug TEXT, directory TEXT, title TEXT, version TEXT, agent TEXT,
            model TEXT, cost REAL, tokens_input INTEGER, tokens_output INTEGER,
            tokens_cache_read INTEGER, tokens_cache_write INTEGER,
            time_created INTEGER, time_updated INTEGER, time_archived INTEGER);
        CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER,
            time_updated INTEGER, data TEXT);
        CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, data TEXT);
        CREATE TABLE todo (session_id TEXT, content TEXT, status TEXT, position INTEGER);

        INSERT INTO project VALUES ('p1', '/home/user/projects/demo');
        INSERT INTO session VALUES ('{SESSION}', 'p1', NULL, 'test-slug',
            '/home/user/projects/demo', 'Write test', '1.17.18', 'build', NULL,
            NULL, NULL, NULL, NULL, NULL, 1755000000000, 1755000100000, NULL);
        INSERT INTO message VALUES ('msg_1', '{SESSION}', 1, 1, '{{"role":"user"}}');
        INSERT INTO part VALUES ('prt_1', 'msg_1', '{SESSION}', '{{"type":"text"}}');
        INSERT INTO todo VALUES ('{SESSION}', 'thing', 'completed', 0);
        "#
    ))
    .unwrap();

    // The per-session file sidecar.
    let diff_dir = dir.join("storage/session_diff");
    fs::create_dir_all(&diff_dir).unwrap();
    fs::write(diff_dir.join(format!("{SESSION}.json")), "[]").unwrap();

    OpenCodeAdapter::with_db(db)
}

fn session(adapter: &OpenCodeAdapter) -> asm_core::model::Session {
    adapter
        .sessions(&SessionFilter::default())
        .unwrap()
        .into_iter()
        .find(|s| s.handle.native_id == SESSION)
        .unwrap()
}

#[test]
fn rename_updates_title() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = write_store(dir.path());
    let s = session(&adapter);
    adapter.rename(&s, "New name").unwrap();
    assert_eq!(session(&adapter).title.as_deref(), Some("New name"));
}

/// Write an OpenCode-shaped lock directory owned by `pid`.
fn write_lock(lock_dir: &Path, name: &str, pid: u32, hostname: &str) {
    let lock = lock_dir.join(name);
    fs::create_dir_all(&lock).unwrap();
    fs::write(
        lock.join("meta.json"),
        format!(
            r#"{{"token":"t","pid":{pid},"hostname":"{hostname}","createdAt":"2026-08-18T03:44:29.534Z"}}"#
        ),
    )
    .unwrap();
    fs::write(lock.join("heartbeat"), "").unwrap();
}

fn this_host() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_string())
        .unwrap_or_default()
}

/// A pid that is guaranteed not to be running: spawn a process and reap it.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

#[test]
fn live_lock_refuses_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let lock_dir = dir.path().join("locks");
    write_lock(&lock_dir, "live.lock", std::process::id(), &this_host());
    let adapter = write_store(dir.path());
    let s = session(&adapter);
    let adapter = OpenCodeAdapter::with_db(dir.path().join("opencode.db")).with_lock_dir(&lock_dir);

    assert!(adapter.store_busy());
    assert!(matches!(adapter.rename(&s, "x"), Err(CoreError::StoreBusy { .. })));
    assert!(matches!(adapter.delete(&s), Err(CoreError::StoreBusy { .. })));
    assert!(adapter.stale_locks().is_empty());
}

#[test]
fn stale_lock_does_not_block_mutations() {
    // OpenCode leaves its lock directory behind when it exits uncleanly.
    // Treating that as "busy" would block every mutation forever.
    let dir = tempfile::tempdir().unwrap();
    let lock_dir = dir.path().join("locks");
    let pid = dead_pid();
    write_lock(&lock_dir, "stale.lock", pid, &this_host());
    let adapter = write_store(dir.path());
    let s = session(&adapter);
    let adapter = OpenCodeAdapter::with_db(dir.path().join("opencode.db")).with_lock_dir(&lock_dir);

    assert!(!adapter.store_busy(), "a dead pid's lock must not block");
    let stale = adapter.stale_locks();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].pid, Some(pid));
    assert!(stale[0].reason.contains("no longer running"), "{}", stale[0].reason);

    adapter.rename(&s, "renamed past a stale lock").unwrap();
    assert_eq!(session(&adapter).title.as_deref(), Some("renamed past a stale lock"));
}

#[test]
fn another_hosts_lock_is_respected_while_its_heartbeat_is_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let lock_dir = dir.path().join("locks");
    // A pid from another machine says nothing about this machine's process
    // table, so the heartbeat decides — and this one was just written.
    write_lock(&lock_dir, "remote.lock", 1, "some-other-host");
    let adapter = write_store(dir.path());
    let s = session(&adapter);
    let adapter = OpenCodeAdapter::with_db(dir.path().join("opencode.db")).with_lock_dir(&lock_dir);

    assert!(adapter.store_busy());
    let err = adapter.rename(&s, "x").unwrap_err();
    assert!(err.to_string().contains("some-other-host"), "{err}");
}

#[test]
fn archive_sets_and_unarchive_clears_native_flag() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = write_store(dir.path());
    let s = session(&adapter);

    let outcome = adapter.archive(&s).unwrap();
    assert!(outcome.archived_to.is_none(), "native flag, nothing moves");
    let archived = session(&adapter);
    assert_eq!(archived.status, SessionStatus::Archived);

    adapter.unarchive(&archived).unwrap();
    assert_eq!(session(&adapter).status, SessionStatus::Idle);
}

#[test]
fn unarchive_refuses_non_archived() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = write_store(dir.path());
    let s = session(&adapter);
    assert!(matches!(adapter.unarchive(&s), Err(CoreError::NotArchived { .. })));
}

#[test]
fn delete_removes_child_sessions_too() {
    // OpenCode's own removal is recursive. Deleting only the target would
    // strand its subagent sessions and every row belonging to them.
    let dir = tempfile::tempdir().unwrap();
    let adapter = write_store(dir.path());
    let conn = Connection::open(dir.path().join("opencode.db")).unwrap();
    conn.execute_batch(&format!(
        r#"
        INSERT INTO session VALUES ('ses_child00000000000000000001', 'p1', '{SESSION}',
            'child', '/home/user/projects/demo', 'Child', '1.17.18', 'general', NULL,
            NULL, NULL, NULL, NULL, NULL, 1755000000000, 1755000100000, NULL);
        INSERT INTO session VALUES ('ses_grandchild0000000000000001', 'p1',
            'ses_child00000000000000000001', 'gc', '/home/user/projects/demo', 'Grandchild',
            '1.17.18', 'general', NULL, NULL, NULL, NULL, NULL, NULL,
            1755000000000, 1755000100000, NULL);
        INSERT INTO message VALUES ('msg_c', 'ses_child00000000000000000001', 1, 1, '{{}}');
        INSERT INTO part VALUES ('prt_c', 'msg_c', 'ses_child00000000000000000001', '{{}}');
        INSERT INTO message VALUES ('msg_g', 'ses_grandchild0000000000000001', 1, 1, '{{}}');
        "#
    ))
    .unwrap();
    drop(conn);

    let s = session(&adapter);
    let report = adapter.delete(&s).unwrap();

    let conn = Connection::open(dir.path().join("opencode.db")).unwrap();
    let sessions: i64 = conn.query_row("SELECT COUNT(*) FROM session", [], |r| r.get(0)).unwrap();
    let messages: i64 = conn.query_row("SELECT COUNT(*) FROM message", [], |r| r.get(0)).unwrap();
    let parts: i64 = conn.query_row("SELECT COUNT(*) FROM part", [], |r| r.get(0)).unwrap();
    assert_eq!(sessions, 0, "descendants must go too, transitively");
    assert_eq!(messages, 0, "no orphan messages left behind");
    assert_eq!(parts, 0);

    // And every one of them is in the backup, not just the target.
    let rows: serde_json::Value =
        serde_json::from_slice(&fs::read(report.backup_dir.unwrap().join("rows.json")).unwrap())
            .unwrap();
    for id in [SESSION, "ses_child00000000000000000001", "ses_grandchild0000000000000001"] {
        assert!(rows.get(id).is_some(), "missing {id} from the backup");
    }
}

#[test]
fn delete_removes_rows_sidecar_and_writes_backup() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = write_store(dir.path());
    let s = session(&adapter);

    let report = adapter.delete(&s).unwrap();
    let backup = report.backup_dir.unwrap();

    // Backup captured the rows and the diff sidecar, keyed by session id
    // (a delete can span a session and its descendants).
    let rows: serde_json::Value =
        serde_json::from_slice(&fs::read(backup.join("rows.json")).unwrap()).unwrap();
    assert_eq!(rows[SESSION]["session"].as_array().unwrap().len(), 1);
    assert_eq!(rows[SESSION]["message"].as_array().unwrap().len(), 1);
    assert_eq!(rows[SESSION]["part"].as_array().unwrap().len(), 1);
    assert!(backup.join(format!("{SESSION}.diff.json")).is_file());

    // Store is clean: no session, no orphan children, no sidecar file.
    assert!(adapter.sessions(&SessionFilter::default()).unwrap().is_empty());
    let conn = Connection::open(dir.path().join("opencode.db")).unwrap();
    for table in ["message", "part", "todo"] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "{table} rows removed");
    }
    assert!(!dir.path().join(format!("storage/session_diff/{SESSION}.json")).exists());
}
