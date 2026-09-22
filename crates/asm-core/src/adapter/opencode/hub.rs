//! OpenCode over the hub: backed up, not yet restorable.
//!
//! Built by reading the rows directly, not by `opencode export`: measured
//! against 1.18.31, export writes to the store on every call, and it also
//! drops a session's todos, shares, inputs and subagent sessions. What is
//! uploaded is the session and every descendant, from every table that
//! holds them, plus the per-session diff file.

use rusqlite::Connection;
use serde_json::{Value, json};

use super::{OpenCodeAdapter, write};
use crate::hub::bundle::{Bundle, Staged};
use crate::model::Session;
use crate::{CoreError, fsutil};

const TABLES: [&str; 5] = ["message", "part", "todo", "session_share", "session_input"];

/// Each table with the column that moves when one of its rows changes.
const WATCHED: [(&str, &str); 5] = [
    ("message", "time_updated"),
    ("part", "time_updated"),
    ("todo", "time_updated"),
    ("session_share", "time_updated"),
    ("session_input", "time_created"),
];

/// What moves when anything the bundle holds does: every descendant's
/// session row, and each table's row count and newest time for it. A
/// subagent continued on its own, or a todo ticked off, changes it though
/// the root's messages do not. Push compares it to skip what has not
/// changed, and it is the bundle's identity.
fn watermark(conn: &Connection, root: &str) -> String {
    let mut parts = Vec::new();
    for id in write::with_descendants(conn, root) {
        let ask = |sql: &str| {
            conn.query_row(sql, [&id], |r| r.get::<_, String>(0)).unwrap_or_else(|_| "?".into())
        };
        parts.push(format!("{id}:{}", ask("SELECT COALESCE(time_updated, 0) || '' FROM session WHERE id = ?1")));
        for (table, col) in WATCHED {
            parts.push(ask(&format!(
                "SELECT COUNT(*) || ':' || COALESCE(MAX({col}), 0) FROM {table} WHERE session_id = ?1"
            )));
        }
    }
    format!("tree:{}", fsutil::sha256_hex(parts.join("\n").as_bytes()))
}

pub(crate) fn fingerprint(adapter: &OpenCodeAdapter, session: &Session) -> Option<String> {
    let conn = super::super::open_ro(adapter.db()).ok()?;
    Some(watermark(&conn, &session.handle.native_id))
}

pub(crate) fn collect(adapter: &OpenCodeAdapter, session: &Session) -> Result<Bundle, CoreError> {
    let conn = super::super::open_ro(adapter.db())?;
    let root = &session.handle.native_id;
    // One read snapshot for the watermark and every row, so they agree with
    // each other: a message written meanwhile is in neither, and the next
    // push sees the watermark move.
    let _ = conn.execute_batch("BEGIN");
    let canonical = watermark(&conn, root);
    let mut rows = serde_json::Map::new();
    let mut files = Vec::new();
    for id in write::with_descendants(&conn, root) {
        let mut dump = serde_json::Map::new();
        dump.insert("session".into(), Value::Array(super::super::dump_rows(&conn, "session", "id", &id)));
        for table in TABLES {
            if write::table_exists(&conn, table) {
                dump.insert(
                    table.into(),
                    Value::Array(super::super::dump_rows(&conn, table, "session_id", &id)),
                );
            }
        }
        rows.insert(id.clone(), Value::Object(dump));
        if let Some(diff) = write::session_diff_path(adapter, &id)
            && let Some(staged) = Staged::file(&format!("session_diff/{id}.json"), &diff)?
        {
            files.push(staged);
        }
    }
    let _ = conn.execute_batch("COMMIT");
    let body = serde_json::to_vec(&json!({ "root": root, "sessions": rows })).unwrap();
    files.insert(0, Staged::bytes("rows.json", body));
    Ok(Bundle { files, canonical, extra: Value::Null })
}

