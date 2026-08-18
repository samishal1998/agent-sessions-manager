//! Mutating operations on the OpenCode store.
//!
//! Policy: `opencode import` is the sanctioned bulk-write path (used by the
//! import engine in M2). The operations here are the documented exceptions —
//! narrow, column-level UPDATEs and a guarded DELETE — and every one of
//! them (a) refuses while an OpenCode instance holds the lock dir, and
//! (b) writes a JSON backup of the affected rows first. Rationale: 1.17.x
//! ships no CLI verbs for rename/archive/delete.

use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::adapter::{ArchiveOutcome, DeleteReport, RelocateOutcome};
use crate::model::{Session, SessionStatus};
use crate::{CoreError, paths};

use super::OpenCodeAdapter;

const AGENT: &str = "opencode";

fn guard_not_busy(adapter: &OpenCodeAdapter) -> Result<(), CoreError> {
    let held: Vec<String> = adapter
        .locks()
        .into_iter()
        .filter(|lock| lock.held)
        .map(|lock| lock.reason)
        .collect();
    if !held.is_empty() {
        return Err(CoreError::StoreBusy { agent: AGENT, detail: held.join("; ") });
    }
    Ok(())
}

fn open_rw(adapter: &OpenCodeAdapter) -> Result<Connection, CoreError> {
    Connection::open(adapter.db())
        .map_err(|e| CoreError::Sqlite { db: adapter.db().to_path_buf(), source: Box::new(e) })
}

fn sql_err(adapter: &OpenCodeAdapter) -> impl Fn(rusqlite::Error) -> CoreError + '_ {
    |e| CoreError::Sqlite { db: adapter.db().to_path_buf(), source: Box::new(e) }
}

fn table_exists(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |_| Ok(()),
    )
    .is_ok()
}

/// Dump all rows of `table` matching `key_column = id` as JSON objects.
fn dump_rows(conn: &Connection, table: &str, key_column: &str, id: &str) -> Vec<Value> {
    let Ok(mut stmt) = conn.prepare(&format!("SELECT * FROM {table} WHERE {key_column} = ?1"))
    else {
        return Vec::new();
    };
    let column_names: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
    let rows = stmt.query_map([id], |row| {
        let mut object = serde_json::Map::new();
        for (i, name) in column_names.iter().enumerate() {
            let value = match row.get_ref(i) {
                Ok(rusqlite::types::ValueRef::Null) => Value::Null,
                Ok(rusqlite::types::ValueRef::Integer(n)) => json!(n),
                Ok(rusqlite::types::ValueRef::Real(f)) => json!(f),
                Ok(rusqlite::types::ValueRef::Text(t)) => {
                    Value::String(String::from_utf8_lossy(t).into_owned())
                }
                Ok(rusqlite::types::ValueRef::Blob(b)) => json!({ "blob_len": b.len() }),
                Err(_) => Value::Null,
            };
            object.insert(name.clone(), value);
        }
        Ok(Value::Object(object))
    });
    match rows {
        Ok(rows) => rows.filter_map(Result::ok).collect(),
        Err(_) => Vec::new(),
    }
}

/// Row-level JSON backup of everything a session-scoped operation touches.
/// `ids` is the session plus, for a delete, its descendants.
fn backup_session_rows(
    adapter: &OpenCodeAdapter,
    conn: &Connection,
    id: &str,
    ids: &[String],
) -> Result<PathBuf, CoreError> {
    let backup = paths::backup_dir(AGENT, id)
        .ok_or_else(|| CoreError::Invalid { msg: "cannot determine backup directory".into() })?;
    fs::create_dir_all(&backup).map_err(|e| CoreError::io(&backup, e))?;

    let mut all = serde_json::Map::new();
    for target in ids {
        let mut dump = serde_json::Map::new();
        dump.insert("session".into(), Value::Array(dump_rows(conn, "session", "id", target)));
        for table in ["message", "part", "todo", "session_share"] {
            if table_exists(conn, table) {
                dump.insert(
                    table.into(),
                    Value::Array(dump_rows(conn, table, "session_id", target)),
                );
            }
        }
        all.insert(target.clone(), Value::Object(dump));

        // The one per-session file sidecar.
        if let Some(diff) = session_diff_path(adapter, target)
            && diff.is_file()
        {
            crate::fsutil::copy_recursive(&diff, &backup.join(format!("{target}.diff.json")))?;
        }
    }

    let path = backup.join("rows.json");
    fs::write(&path, serde_json::to_string_pretty(&Value::Object(all)).unwrap())
        .map_err(|e| CoreError::io(&path, e))?;
    Ok(backup)
}

/// The session and every session transitively parented to it. OpenCode's
/// own `Session.remove` is recursive; deleting only the target would strand
/// its subagent sessions and all their messages.
fn with_descendants(conn: &Connection, root: &str) -> Vec<String> {
    let mut all = vec![root.to_string()];
    let mut frontier = vec![root.to_string()];
    while let Some(parent) = frontier.pop() {
        let Ok(mut stmt) = conn.prepare("SELECT id FROM session WHERE parent_id = ?1") else {
            break;
        };
        let Ok(rows) = stmt.query_map([&parent], |row| row.get::<_, String>(0)) else {
            continue;
        };
        for child in rows.filter_map(Result::ok) {
            if !all.contains(&child) {
                all.push(child.clone());
                frontier.push(child);
            }
        }
    }
    all
}

fn session_diff_path(adapter: &OpenCodeAdapter, id: &str) -> Option<PathBuf> {
    adapter.db().parent().map(|d| d.join("storage/session_diff").join(format!("{id}.json")))
}

pub(super) fn rename(
    adapter: &OpenCodeAdapter,
    session: &Session,
    title: &str,
) -> Result<(), CoreError> {
    guard_not_busy(adapter)?;
    let conn = open_rw(adapter)?;
    backup_session_rows(
        adapter,
        &conn,
        &session.handle.native_id,
        std::slice::from_ref(&session.handle.native_id),
    )?;
    let changed = conn
        .execute(
            "UPDATE session SET title = ?1 WHERE id = ?2",
            rusqlite::params![title, session.handle.native_id],
        )
        .map_err(sql_err(adapter))?;
    if changed == 0 {
        return Err(CoreError::Invalid {
            msg: format!("session {} not found in store", session.handle.native_id),
        });
    }
    Ok(())
}

pub(super) fn archive(
    adapter: &OpenCodeAdapter,
    session: &Session,
) -> Result<ArchiveOutcome, CoreError> {
    guard_not_busy(adapter)?;
    let conn = open_rw(adapter)?;
    backup_session_rows(
        adapter,
        &conn,
        &session.handle.native_id,
        std::slice::from_ref(&session.handle.native_id),
    )?;
    let now = jiff::Timestamp::now().as_millisecond();
    conn.execute(
        "UPDATE session SET time_archived = ?1 WHERE id = ?2 AND time_archived IS NULL",
        rusqlite::params![now, session.handle.native_id],
    )
    .map_err(sql_err(adapter))?;
    // Native archived flag — nothing moves out of the store.
    Ok(ArchiveOutcome { archived_to: None })
}

pub(super) fn unarchive(adapter: &OpenCodeAdapter, session: &Session) -> Result<(), CoreError> {
    if session.status != SessionStatus::Archived {
        return Err(CoreError::NotArchived { id: session.handle.native_id.clone() });
    }
    guard_not_busy(adapter)?;
    let conn = open_rw(adapter)?;
    backup_session_rows(
        adapter,
        &conn,
        &session.handle.native_id,
        std::slice::from_ref(&session.handle.native_id),
    )?;
    conn.execute(
        "UPDATE session SET time_archived = NULL WHERE id = ?1",
        rusqlite::params![session.handle.native_id],
    )
    .map_err(sql_err(adapter))?;
    Ok(())
}

pub(super) fn relocate(
    _adapter: &OpenCodeAdapter,
    _session: &Session,
    _new_dir: &std::path::Path,
) -> Result<RelocateOutcome, CoreError> {
    // The sanctioned relocate is `opencode import` run from the new
    // directory (the session-row upsert rewrites project/directory/path).
    // That lands with the M2 import engine, which builds the export doc.
    Err(CoreError::NotSupported { agent: AGENT, op: "move (arrives with the import engine)" })
}

pub(super) fn delete(
    adapter: &OpenCodeAdapter,
    session: &Session,
) -> Result<DeleteReport, CoreError> {
    guard_not_busy(adapter)?;
    let id = &session.handle.native_id;
    let conn = open_rw(adapter)?;
    let targets = with_descendants(&conn, id);
    let backup = backup_session_rows(adapter, &conn, id, &targets)?;

    let mut removed = Vec::new();
    let mut root_deleted = false;
    // Deepest first, so a failure part-way never orphans a child behind a
    // deleted parent.
    for target in targets.iter().rev() {
        // Child rows go explicitly rather than by FK cascade: SQLite only
        // enforces those when the connection opts in.
        for table in ["part", "message", "todo", "session_share", "session_input"] {
            if table_exists(&conn, table) {
                conn.execute(
                    &format!("DELETE FROM {table} WHERE session_id = ?1"),
                    [target.as_str()],
                )
                .map_err(sql_err(adapter))?;
            }
        }
        let changed = conn
            .execute("DELETE FROM session WHERE id = ?1", [target.as_str()])
            .map_err(sql_err(adapter))?;
        if target == id {
            root_deleted = changed > 0;
        }

        if let Some(diff) = session_diff_path(adapter, target)
            && diff.is_file()
        {
            crate::fsutil::remove_recursive(&diff)?;
            removed.push(diff);
        }
    }
    if !root_deleted {
        return Err(CoreError::Invalid { msg: format!("session {id} not found in store") });
    }
    removed.push(adapter.db().to_path_buf());
    Ok(DeleteReport { backup_dir: Some(backup), removed })
}
