//! Codex over the hub: backed up, not yet restorable.
//!
//! The rollout is append-only JSONL, so its complete lines are the
//! conversation's identity. The `threads` row travels as JSON beside it,
//! because it is the row codex's own picker reads.

use std::path::Path;

use serde_json::json;

use super::CodexAdapter;
use crate::hub::bundle::{self, Bundle, Staged};
use crate::model::{Session, SessionLocation};
use crate::{CoreError, fsutil};

pub(crate) fn collect(adapter: &CodexAdapter, session: &Session) -> Result<Bundle, CoreError> {
    let SessionLocation::JsonlFile { path } = &session.handle.location else {
        return Err(CoreError::Invalid { msg: "codex session has no rollout file".into() });
    };
    let raw = std::fs::read(path).map_err(|e| CoreError::io(path, e))?;
    let rollout = bundle::complete_lines(&raw).to_vec();
    let canonical = fsutil::sha256_hex(&rollout);
    let mut files = vec![Staged::bytes("rollout.jsonl", rollout)];

    // Orphan rollouts have no row; that is a normal codex state.
    let db = adapter.state_db();
    if db.is_file() {
        let conn = super::super::open_ro(&db)?;
        if let Some(row) =
            super::super::dump_rows(&conn, "threads", "id", &session.handle.native_id).pop()
        {
            files.push(Staged::bytes("thread.json", serde_json::to_vec_pretty(&row).unwrap()));
        }
    }
    // Where it lived inside CODEX_HOME. Codex also records the absolute
    // cwd inside the rollout itself, more than once, so a restore can only
    // ever be to the same path.
    let rollout_rel = path.strip_prefix(adapter.root()).unwrap_or(Path::new(path));
    Ok(Bundle { files, canonical, extra: json!({ "rollout_rel": rollout_rel.display().to_string() }) })
}
