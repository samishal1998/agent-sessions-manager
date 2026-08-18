//! Mutating operations on the Claude Code store.
//!
//! Ground rules (from the /cd relocation internals extracted from the CLI
//! binary, and from how resume resolves ids):
//! - Never touch a session whose PID record shows a live process.
//! - Never rewrite existing transcript bytes — background jobs hold raw
//!   byte offsets (`linkScanOffset`) into these files. Appends and
//!   whole-file renames only.
//! - Move, never copy: a duplicated transcript id makes Claude Code's
//!   cross-project `--resume <id>` return "not found" (2+ matches → null).
//! - A session's state spans FIVE locations in two roots. Project-scoped
//!   (move on relocate): the transcript and the in-project `<uuid>/` dir.
//!   UUID-keyed under the root (leave alone on relocate, remove on
//!   delete): `file-history/<uuid>/`, `session-env/<uuid>/`, `tasks/<uuid>/`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::adapter::{ArchiveOutcome, DeleteReport, RelocateOutcome};
use crate::fsutil;
use crate::model::{Session, SessionLocation};
use crate::{CoreError, paths};

use super::{ClaudeAdapter, liveness};

const AGENT: &str = "claude-code";

fn guard_not_live(adapter: &ClaudeAdapter, session_id: &str) -> Result<(), CoreError> {
    if let Some(pid) = liveness::live_sessions(adapter.root()).get(session_id) {
        return Err(CoreError::SessionLive { id: session_id.to_string(), pid: Some(*pid) });
    }
    Ok(())
}

fn transcript_path(session: &Session) -> Result<PathBuf, CoreError> {
    match &session.handle.location {
        SessionLocation::JsonlFile { path } => Ok(path.clone()),
        _ => Err(CoreError::Invalid { msg: "session has no transcript file".into() }),
    }
}

/// UUID-keyed sidecar dirs under the store root (NOT project-scoped).
fn uuid_sidecars(root: &Path, id: &str) -> Vec<PathBuf> {
    ["file-history", "session-env", "tasks"]
        .iter()
        .map(|d| root.join(d).join(id))
        .filter(|p| p.exists())
        .collect()
}

pub(super) fn rename(
    adapter: &ClaudeAdapter,
    session: &Session,
    title: &str,
) -> Result<(), CoreError> {
    guard_not_live(adapter, &session.handle.native_id)?;
    let transcript = transcript_path(session)?;
    // `custom-title` is the record Claude Code's own rename writes, and it
    // outranks `ai-title` (which is its auto-summarizer's slot) when the
    // picker resolves a title. Writing `ai-title` here would leave the
    // rename invisible in Claude Code's own UI whenever a custom title
    // already existed.
    let record = json!({
        "type": "custom-title",
        "customTitle": title,
        "sessionId": session.handle.native_id,
    });
    fsutil::append_jsonl_line(&transcript, &record.to_string())
}

pub(super) fn relocate(
    adapter: &ClaudeAdapter,
    session: &Session,
    new_dir: &Path,
) -> Result<RelocateOutcome, CoreError> {
    let id = &session.handle.native_id;
    guard_not_live(adapter, id)?;

    let new_dir = new_dir
        .canonicalize()
        .map_err(|e| CoreError::io(new_dir, e))?;
    if !new_dir.is_dir() {
        return Err(CoreError::Invalid {
            msg: format!("{} is not a directory", new_dir.display()),
        });
    }
    let new_dir_str = new_dir
        .to_str()
        .ok_or_else(|| CoreError::Invalid { msg: "destination path is not UTF-8".into() })?
        .to_string();

    let old_transcript = transcript_path(session)?;
    let old_project_dir = old_transcript
        .parent()
        .ok_or_else(|| CoreError::Invalid { msg: "transcript has no parent dir".into() })?
        .to_path_buf();

    // Step 2: compute the destination project dir with the ported encoder.
    let encoded = super::path_encode::encode_project_dir(&new_dir_str);
    let dest_project_dir = adapter.root().join("projects").join(&encoded);
    let dest_transcript = dest_project_dir.join(format!("{id}.jsonl"));
    if dest_transcript.exists() {
        return Err(CoreError::DestinationExists { path: dest_transcript });
    }

    // Step 1b: duplicate pre-scan — an existing duplicate elsewhere means
    // cross-project resume for this id is already poisoned; surface it.
    let mut warnings = Vec::new();
    for (dup_id, transcript_paths) in super::store::duplicate_session_ids(adapter)? {
        if dup_id == *id {
            warnings.push(format!(
                "session {id} already exists in {} project dirs; cross-project --resume \
                 stays broken until the duplicates are removed",
                transcript_paths.len()
            ));
        }
    }

    // Step 3: create the destination project dir, private like the CLI does.
    fs::create_dir_all(&dest_project_dir).map_err(|e| CoreError::io(&dest_project_dir, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dest_project_dir, fs::Permissions::from_mode(0o700));
    }

    // Step 4: move the transcript (never copy-and-keep).
    fsutil::move_path(&old_transcript, &dest_transcript)?;

    // Step 5: move the in-project sidecar dir (subagents/workflows/
    // tool-results), tolerating its absence.
    let old_sidecar = old_project_dir.join(id.as_str());
    let dest_sidecar = dest_project_dir.join(id.as_str());
    let sidecar_moved = old_sidecar.is_dir();
    if sidecar_moved {
        fsutil::move_path(&old_sidecar, &dest_sidecar)?;
    }

    // Step 6: append the relocation marker — this is what keeps the session
    // out of the old directory's picker and records the authoritative cwd.
    let marker = json!({
        "type": "relocated",
        "sessionId": id,
        "relocatedCwd": new_dir_str,
    });
    fsutil::append_jsonl_line(&dest_transcript, &marker.to_string())?;

    // Step 7: repoint symlinks inside the moved sidecar dir whose targets
    // referenced the old sidecar location (task-output symlinks).
    if sidecar_moved {
        repoint_symlinks(&dest_sidecar, &old_sidecar, &dest_sidecar)?;
    }

    // Step 8: rehome background-job state that references this transcript.
    // cwd/originCwd and linkScanPath are updated; linkScanOffset is a raw
    // byte offset that stays valid because the move preserved bytes.
    rehome_jobs(adapter.root(), &old_transcript, &dest_transcript, &new_dir_str, &mut warnings);

    // Steps 9-10: the trust latch in ~/.claude.json is a security decision
    // left to the user (Claude will show its trust prompt); everything else
    // (file-history/, session-env/, tasks/, sessions/, plans/, memory/) is
    // deliberately untouched.

    Ok(RelocateOutcome { new_transcript: Some(dest_transcript), warnings })
}

fn repoint_symlinks(dir: &Path, old_prefix: &Path, new_prefix: &Path) -> Result<(), CoreError> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if meta.is_dir() {
            repoint_symlinks(&path, old_prefix, new_prefix)?;
        } else if meta.is_symlink()
            && let Ok(target) = fs::read_link(&path)
            && let Ok(rest) = target.strip_prefix(old_prefix)
        {
            let new_target = new_prefix.join(rest);
            fs::remove_file(&path).map_err(|e| CoreError::io(&path, e))?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&new_target, &path)
                .map_err(|e| CoreError::io(&path, e))?;
        }
    }
    Ok(())
}

fn rehome_jobs(
    root: &Path,
    old_transcript: &Path,
    new_transcript: &Path,
    new_cwd: &str,
    warnings: &mut Vec<String>,
) {
    let jobs_dir = root.join("jobs");
    let Ok(entries) = fs::read_dir(&jobs_dir) else { return };
    for entry in entries.filter_map(Result::ok) {
        let state_path = entry.path().join("state.json");
        let Ok(bytes) = fs::read(&state_path) else { continue };
        let Ok(mut state) = serde_json::from_slice::<Value>(&bytes) else { continue };
        let matches = state
            .get("linkScanPath")
            .and_then(Value::as_str)
            .is_some_and(|p| Path::new(p) == old_transcript);
        if !matches {
            continue;
        }
        state["linkScanPath"] = Value::String(new_transcript.display().to_string());
        state["cwd"] = Value::String(new_cwd.to_string());
        // originCwd follows cwd unless a worktreePath pins it elsewhere.
        if state.get("worktreePath").and_then(Value::as_str).is_none()
            && state.get("originCwd").is_some()
        {
            state["originCwd"] = Value::String(new_cwd.to_string());
        }
        if let Err(e) = fs::write(&state_path, state.to_string()) {
            warnings.push(format!(
                "failed to rehome background job state {}: {e}",
                state_path.display()
            ));
        }
    }
}

pub(super) fn archive(
    adapter: &ClaudeAdapter,
    session: &Session,
) -> Result<ArchiveOutcome, CoreError> {
    let id = &session.handle.native_id;
    guard_not_live(adapter, id)?;

    let transcript = transcript_path(session)?;
    let project_dir = transcript.parent().unwrap().to_path_buf();

    // Everything that belongs to this session, moved as-is so unarchive is
    // an exact restore.
    let mut entries: Vec<(String, PathBuf)> = vec![("transcript.jsonl".into(), transcript)];
    let sidecar = project_dir.join(id.as_str());
    if sidecar.is_dir() {
        entries.push(("session-dir".into(), sidecar));
    }
    for dir in uuid_sidecars(adapter.root(), id) {
        let name = dir.parent().unwrap().file_name().unwrap().to_string_lossy().into_owned();
        entries.push((name, dir));
    }

    crate::archive::archive_entries(session, &entries)
}

pub(super) fn delete(
    adapter: &ClaudeAdapter,
    session: &Session,
) -> Result<DeleteReport, CoreError> {
    let id = &session.handle.native_id;
    guard_not_live(adapter, id)?;

    let transcript = transcript_path(session)?;
    let project_dir = transcript.parent().unwrap().to_path_buf();

    let mut targets: Vec<PathBuf> = vec![transcript.clone()];
    let sidecar = project_dir.join(id.as_str());
    if sidecar.exists() {
        targets.push(sidecar);
    }
    targets.extend(uuid_sidecars(adapter.root(), id));

    // Full copy into the backup dir before anything is removed.
    let backup = paths::backup_dir(AGENT, id)
        .ok_or_else(|| CoreError::Invalid { msg: "cannot determine backup directory".into() })?;
    fs::create_dir_all(&backup).map_err(|e| CoreError::io(&backup, e))?;
    for target in &targets {
        let name = backup_entry_name(adapter.root(), &project_dir, target, id);
        fsutil::copy_recursive(target, &backup.join(name))?;
    }

    for target in &targets {
        fsutil::remove_recursive(target)?;
    }
    Ok(DeleteReport { backup_dir: Some(backup), removed: targets })
}

/// Stable, collision-free names for backed-up entries.
fn backup_entry_name(root: &Path, project_dir: &Path, target: &Path, id: &str) -> String {
    if target.extension().and_then(|e| e.to_str()) == Some("jsonl") {
        "transcript.jsonl".to_string()
    } else if target.parent() == Some(project_dir) {
        "session-dir".to_string()
    } else {
        // root-level uuid sidecar: file-history/<id> -> "file-history"
        target
            .strip_prefix(root)
            .ok()
            .and_then(|p| p.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_else(|| id.to_string())
    }
}
