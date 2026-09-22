//! Antigravity over the hub: backed up, not yet restorable.
//!
//! The conversation is a SQLite database with most of a fresh conversation
//! in its write-ahead log, so it is snapshotted with `VACUUM INTO` — one
//! consistent file, taken through a read-only connection — rather than
//! copied. Antigravity's JSONL rendering of the same steps is the identity
//! when it exists, because the database bytes change on every vacuum.

use serde_json::{Value, json};

use super::AntigravityAdapter;
use crate::hub::bundle::{self, Bundle, Staged};
use crate::model::Session;
use crate::{CoreError, fsutil, paths};

pub(crate) fn collect(adapter: &AntigravityAdapter, session: &Session) -> Result<Bundle, CoreError> {
    let id = &session.handle.native_id;
    let db = adapter.conversations_dir().join(format!("{id}.db"));

    let tmp_dir = paths::data_dir()
        .ok_or_else(|| CoreError::Invalid { msg: "cannot determine asm data dir".into() })?
        .join("tmp");
    std::fs::create_dir_all(&tmp_dir).map_err(|e| CoreError::io(&tmp_dir, e))?;
    let snapshot = tmp_dir.join(format!("antigravity-{id}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&snapshot);
    let conn = super::super::open_ro(&db)?;
    conn.execute("VACUUM INTO ?1", [snapshot.display().to_string()])
        .map_err(|e| CoreError::Sqlite { db: db.clone(), source: Box::new(e) })?;
    drop(conn);
    let bytes = std::fs::read(&snapshot).map_err(|e| CoreError::io(&snapshot, e));
    let _ = std::fs::remove_file(&snapshot);
    let mut files = vec![Staged::bytes("conversation.db", bytes?)];

    files.extend(bundle::walk(&adapter.root().join("brain").join(id), "brain")?);

    // Its cache entries, as data. `last_conversations.json` maps a workspace
    // to one conversation per directory, so writing it back on another
    // machine would clobber that machine's own binding; it travels only so
    // the pushing machine's workspace is on record.
    let read_json = |path: std::path::PathBuf| -> Value {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null)
    };
    let metadata = read_json(adapter.metadata_file())
        .get("conversations")
        .and_then(|c| c.get(id))
        .cloned()
        .unwrap_or(Value::Null);
    let workspace = read_json(adapter.workspaces_file())
        .as_object()
        .and_then(|m| m.iter().find(|(_, v)| v.as_str() == Some(id)).map(|(k, _)| k.clone()));
    files.push(Staged::bytes(
        "cache.json",
        serde_json::to_vec_pretty(&json!({ "metadata": metadata, "workspace": workspace })).unwrap(),
    ));

    let transcript = adapter.transcript_of(id);
    let canonical = match std::fs::read(&transcript) {
        Ok(raw) => fsutil::sha256_hex(bundle::complete_lines(&raw)),
        Err(_) => crate::index::fingerprint(session),
    };
    Ok(Bundle { files, canonical, extra: Value::Null })
}
