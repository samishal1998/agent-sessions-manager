//! Mutating operations on the jcode store.
//!
//! Two rules shape this module.
//!
//! First, **rename goes through `jcode session rename`**, jcode's own
//! command, rather than through asm rewriting the snapshot. The snapshot is
//! a single JSON document holding the entire conversation; rewriting it to
//! change one field would rewrite every byte of someone's history, and
//! jcode ships a command for exactly this. It also writes the field jcode
//! itself reads (`custom_title`, leaving the generated `title` alone).
//!
//! Second, **archive and delete only move or remove whole files** — the
//! snapshot, its `.bak` sibling and its journal — and never edit their
//! contents. Anything that would require rewriting the document (moving a
//! session to another directory, or materializing an imported one) has no
//! sanctioned path and stays unsupported.

use std::fs;
use std::path::PathBuf;

use crate::adapter::{ArchiveOutcome, DeleteReport};
use crate::model::{Session, SessionLocation};
use crate::{CoreError, fsutil, paths};

use super::{JCodeAdapter, store};

const AGENT: &str = "jcode";

fn guard_not_live(adapter: &JCodeAdapter, session: &Session) -> Result<(), CoreError> {
    if let Some(pid) = store::live_pid(adapter.root(), &session.handle.native_id) {
        return Err(CoreError::SessionLive {
            id: session.handle.native_id.clone(),
            pid: Some(pid),
        });
    }
    Ok(())
}

fn snapshot_path(session: &Session) -> Result<PathBuf, CoreError> {
    match &session.handle.location {
        SessionLocation::JsonlFile { path } => Ok(path.clone()),
        _ => Err(CoreError::Invalid { msg: "session has no snapshot file".into() }),
    }
}

/// Every file that belongs to this session: the snapshot, the backup jcode
/// writes beside it, and the append-only journal.
fn session_files(session: &Session) -> Result<Vec<(String, PathBuf)>, CoreError> {
    let snapshot = snapshot_path(session)?;
    let mut files = vec![("snapshot.json".to_string(), snapshot.clone())];
    for (name, path) in [
        ("snapshot.bak", snapshot.with_extension("bak")),
        ("journal.jsonl", snapshot.with_extension("journal.jsonl")),
    ] {
        if path.is_file() {
            files.push((name.to_string(), path));
        }
    }
    Ok(files)
}

/// jcode caches the session picker's list. It is derived from the sessions
/// directory and regenerates on the next scan, so dropping it after a
/// session appears or disappears just avoids a stale picker.
fn invalidate_picker_cache(adapter: &JCodeAdapter) {
    let _ = fs::remove_file(adapter.root().join("cache/session-picker-list-v2.json"));
}

pub(super) fn rename(
    adapter: &JCodeAdapter,
    session: &Session,
    title: &str,
) -> Result<(), CoreError> {
    guard_not_live(adapter, session)?;

    let output = std::process::Command::new("jcode")
        .args(["session", "rename"])
        .arg(&session.handle.native_id)
        .arg(title)
        .arg("--json")
        .output()
        .map_err(|e| CoreError::Invalid { msg: format!("failed to run jcode: {e}") })?;
    if !output.status.success() {
        return Err(CoreError::Invalid {
            msg: format!(
                "jcode session rename failed ({}): {}{}",
                output.status,
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim(),
            ),
        });
    }
    invalidate_picker_cache(adapter);
    Ok(())
}

pub(super) fn archive(
    adapter: &JCodeAdapter,
    session: &Session,
) -> Result<ArchiveOutcome, CoreError> {
    guard_not_live(adapter, session)?;
    let outcome = crate::archive::archive_entries(session, &session_files(session)?)?;
    invalidate_picker_cache(adapter);
    Ok(outcome)
}

pub(super) fn delete(
    adapter: &JCodeAdapter,
    session: &Session,
) -> Result<DeleteReport, CoreError> {
    guard_not_live(adapter, session)?;
    let files = session_files(session)?;

    let backup = paths::backup_dir(AGENT, &session.handle.native_id)
        .ok_or_else(|| CoreError::Invalid { msg: "cannot determine backup directory".into() })?;
    fs::create_dir_all(&backup).map_err(|e| CoreError::io(&backup, e))?;
    for (name, path) in &files {
        fsutil::copy_recursive(path, &backup.join(name))?;
    }

    let mut removed = Vec::new();
    for (_, path) in &files {
        fsutil::remove_recursive(path)?;
        removed.push(path.clone());
    }
    invalidate_picker_cache(adapter);
    Ok(DeleteReport { backup_dir: Some(backup), removed })
}
