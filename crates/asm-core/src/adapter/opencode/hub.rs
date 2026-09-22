//! OpenCode over the hub: backed up, not yet restorable.
//!
//! Built by reading the rows directly, not by `opencode export`: measured
//! against 1.18.31, export writes to the store on every call, and it also
//! drops a session's todos, shares, inputs and subagent sessions. What is
//! uploaded is the session and every descendant, from every table that
//! holds them, plus the per-session diff file.

use serde_json::{Value, json};

use super::{OpenCodeAdapter, write};
use crate::hub::bundle::{Bundle, Staged};
use crate::model::Session;
use crate::CoreError;

const TABLES: [&str; 5] = ["message", "part", "todo", "session_share", "session_input"];

pub(crate) fn collect(adapter: &OpenCodeAdapter, session: &Session) -> Result<Bundle, CoreError> {
    let conn = super::super::open_ro(adapter.db())?;
    let root = &session.handle.native_id;
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
    let body = serde_json::to_vec(&json!({ "root": root, "sessions": rows })).unwrap();
    files.insert(0, Staged::bytes("rows.json", body));
    // Rows are rewritten in place while a turn streams, so a byte hash of
    // them would churn; the same watermark the search index trusts is the
    // identity instead.
    Ok(Bundle { files, canonical: crate::index::fingerprint(session), extra: Value::Null })
}

