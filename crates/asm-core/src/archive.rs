//! The archive store, shared by every file-backed agent.
//!
//! An archived session is a directory holding a manifest plus the session's
//! own files, moved rather than transformed:
//!
//! ```text
//! <data>/archive/<agent>/<id>/
//!     manifest.json   what it was, and where each file came from
//!     native/         the files themselves, unchanged
//! ```
//!
//! Because the manifest records each entry's original absolute path,
//! restoring is exact and does not need to know which agent wrote it — one
//! `unarchive_by_id` serves them all.

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::adapter::ArchiveOutcome;
use crate::model::Session;
use crate::{CoreError, fsutil, paths};

/// Move a session's files into the archive. `entries` pairs the name the
/// file takes inside `native/` with where it currently lives.
pub fn archive_entries(
    session: &Session,
    entries: &[(String, PathBuf)],
) -> Result<ArchiveOutcome, CoreError> {
    let agent = session.handle.agent.as_str();
    let id = &session.handle.native_id;

    let archive_dir = paths::archive_dir(agent, id)
        .ok_or_else(|| CoreError::Invalid { msg: "cannot determine archive directory".into() })?;
    if archive_dir.exists() {
        return Err(CoreError::DestinationExists { path: archive_dir });
    }
    let native = archive_dir.join("native");
    fs::create_dir_all(&native).map_err(|e| CoreError::io(&native, e))?;

    let manifest = json!({
        "schema": 1,
        "agent": agent,
        "id": id,
        // The memorable name an agent uses for the session, so it can be
        // restored by the handle its user knows it by.
        "slug": session.slug,
        "title": session.title,
        "project_root": session.project_root,
        "archived_at": jiff::Timestamp::now().to_string(),
        "entries": entries
            .iter()
            .map(|(name, original)| json!({ "name": name, "original_path": original }))
            .collect::<Vec<_>>(),
    });
    let manifest_path = archive_dir.join("manifest.json");
    fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).unwrap())
        .map_err(|e| CoreError::io(&manifest_path, e))?;

    for (name, original) in entries {
        fsutil::move_path(original, &native.join(name))?;
    }
    Ok(ArchiveOutcome { archived_to: Some(archive_dir) })
}

/// Restore an archived session, whichever agent wrote it. Archived sessions
/// no longer resolve as normal sessions, so this takes the raw id — or the
/// memorable name recorded in the manifest, since that is the handle the
/// user actually types (`asm unarchive iwazaru`).
///
/// Returns the restored path that looks most like the session itself.
pub fn unarchive_by_id(id: &str) -> Result<PathBuf, CoreError> {
    let root = paths::data_dir()
        .map(|d| d.join("archive"))
        .ok_or_else(|| CoreError::Invalid { msg: "cannot determine archive directory".into() })?;

    // The agent is part of the path, but the caller only has an id — so
    // look through each agent's archive for it.
    let archive_dir = find_archived(&root, id)
        .ok_or_else(|| CoreError::NotArchived { id: id.to_string() })?;

    let bytes = fs::read(archive_dir.join("manifest.json"))
        .map_err(|_| CoreError::NotArchived { id: id.to_string() })?;
    let manifest: Value = serde_json::from_slice(&bytes)
        .map_err(|_| CoreError::NotArchived { id: id.to_string() })?;
    let entries = manifest
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| CoreError::Invalid { msg: "malformed archive manifest".into() })?;

    // Refuse if anything would be overwritten, before moving anything.
    let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();
    for entry in entries {
        let (Some(name), Some(original)) = (
            entry.get("name").and_then(Value::as_str),
            entry.get("original_path").and_then(Value::as_str),
        ) else {
            return Err(CoreError::Invalid { msg: "malformed archive manifest entry".into() });
        };
        let to = PathBuf::from(original);
        if to.exists() {
            return Err(CoreError::DestinationExists { path: to });
        }
        moves.push((archive_dir.join("native").join(name), to));
    }

    let mut restored = None;
    for (from, to) in moves {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|e| CoreError::io(parent, e))?;
        }
        fsutil::move_path(&from, &to)?;
        // The session's own file, as opposed to its sidecars.
        if matches!(to.extension().and_then(|e| e.to_str()), Some("jsonl" | "json"))
            && restored.is_none()
        {
            restored = Some(to);
        }
    }
    fsutil::remove_recursive(&archive_dir)?;
    restored.ok_or(CoreError::NotArchived { id: id.to_string() })
}

/// Locate an archived session by id or by the memorable name in its manifest.
fn find_archived(root: &std::path::Path, query: &str) -> Option<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else { return None };
    // The archive root also holds the scaffolding `asm sync init` writes
    // (.gitignore, README.md), so anything that is not a directory is
    // skipped rather than aborting the search.
    let agent_dirs = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir());

    let mut by_slug = None;
    for agent_dir in agent_dirs {
        let exact = agent_dir.join(query);
        if exact.join("manifest.json").is_file() {
            return Some(exact);
        }
        let Ok(sessions) = fs::read_dir(&agent_dir) else { continue };
        for entry in sessions.filter_map(Result::ok) {
            let Ok(bytes) = fs::read(entry.path().join("manifest.json")) else { continue };
            let Ok(value) = serde_json::from_slice::<Value>(&bytes) else { continue };
            if value.get("slug").and_then(Value::as_str) == Some(query) {
                by_slug = Some(entry.path());
            }
        }
    }
    by_slug
}
