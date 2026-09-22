//! jcode over the hub.
//!
//! jcode rewrites its whole snapshot every turn, so no file is ever a prefix
//! of another. The identity is the conversation — the snapshot's id,
//! messages, title and names — and not the rest, which is one machine's
//! own: the pid that last opened it, the directory it is filed under, the
//! times it was last looked at, the environment of each process that did.
//!
//! A pull replaces an older copy here when its messages are a prefix of the
//! hub's, after a backup, and files it under this machine's directory.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{JCodeAdapter, store, write};
use crate::hub::bundle::{self, Base, Bundle, InstallOutcome, Installed, Staged};
use crate::hub::manifest::Manifest;
use crate::model::Session;
use crate::{CoreError, fsutil, paths};

/// The fields that make up the conversation.
const CONVERSATION: [&str; 7] =
    ["id", "created_at", "parent_id", "short_name", "title", "custom_title", "messages"];

/// Turns jcode has taken but not yet folded into the snapshot (it does so
/// when the session next loads, then deletes the file).
pub(crate) fn journal_of(snapshot: &Path) -> PathBuf {
    snapshot.with_extension("journal.jsonl")
}

fn has_journal(snapshot: &Path) -> bool {
    std::fs::metadata(journal_of(snapshot)).is_ok_and(|m| m.len() > 0)
}

fn canonical_of(snapshot: &Value) -> String {
    let conversation: serde_json::Map<String, Value> = CONVERSATION
        .iter()
        .filter_map(|k| snapshot.get(*k).map(|v| (k.to_string(), v.clone())))
        .collect();
    fsutil::sha256_hex(&serde_json::to_vec(&conversation).unwrap())
}

pub(crate) fn collect(_adapter: &JCodeAdapter, session: &Session) -> Result<Bundle, CoreError> {
    let mut files = Vec::new();
    let mut canonical = None;
    for (name, path) in write::session_files(session)? {
        // Read once and upload exactly what was read: a snapshot rewritten
        // between hashing and uploading would otherwise fail its checksum.
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && name != "snapshot.json" => continue,
            Err(e) => return Err(CoreError::io(&path, e)),
        };
        if name == "snapshot.json" {
            let value: Value = serde_json::from_slice(&bytes).map_err(|e| CoreError::Invalid {
                msg: format!("{} is not readable JSON yet: {e}", path.display()),
            })?;
            canonical = Some(canonical_of(&value));
        }
        files.push(Staged::bytes(&name, bytes));
    }
    let canonical = canonical
        .ok_or_else(|| CoreError::Invalid { msg: "jcode session has no snapshot".into() })?;
    Ok(Bundle { files, canonical, extra: Value::Null })
}

fn read_json(path: &Path) -> Result<Value, CoreError> {
    let bytes = std::fs::read(path).map_err(|e| CoreError::io(path, e))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| CoreError::Invalid { msg: format!("{} is not readable JSON: {e}", path.display()) })
}

fn messages(snapshot: &Value) -> &[Value] {
    snapshot.get("messages").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default()
}

/// The copy here against the hub's, by their messages: jcode only ever
/// adds to the list, so an older copy's is a prefix of a newer one's.
fn compare(local: &Value, hub: &Value) -> InstallOutcome {
    let (l, h) = (messages(local), messages(hub));
    if canonical_of(local) == canonical_of(hub) {
        InstallOutcome::InSync
    } else if h.len() > l.len() && h.starts_with(l) {
        InstallOutcome::Replaced
    } else if l.len() > h.len() && l.starts_with(h) {
        InstallOutcome::Ahead
    } else {
        InstallOutcome::Diverged
    }
}

/// Filed under `dir` here: jcode resumes a session in its `working_dir`.
fn filed_at(mut snapshot: Value, dir: &Path) -> Vec<u8> {
    if let Some(object) = snapshot.as_object_mut() {
        object.insert("working_dir".into(), json!(dir.display().to_string()));
        object.remove("last_pid");
    }
    serde_json::to_vec_pretty(&snapshot).unwrap()
}

/// jcode 0.83's picker lists `recent_sessions`, a row it writes when a
/// session is used and never for a file that merely appears (measured: a
/// pulled session stays out of it through later runs until resumed by id).
/// So write the row jcode would, derived from the snapshot — into a
/// database jcode already made, never a new one. It is a cache: if this
/// fails the session is still resumable by id, so failure is ignored.
fn remember_in_picker(adapter: &JCodeAdapter, snapshot: &Value, dir: &Path) {
    let db = adapter.root().join("session-metadata-v1.sqlite3");
    if !db.is_file() {
        return;
    }
    let Ok(conn) = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
    else {
        return;
    };
    let _ = conn.busy_timeout(std::time::Duration::from_secs(2));
    let ms = |k: &str| {
        snapshot.get(k).and_then(Value::as_str).and_then(|t| t.parse::<jiff::Timestamp>().ok()).map(|t| t.as_millisecond())
    };
    // jcode's own upsert, less todo_title, which the snapshot does not hold.
    let _ = conn.execute(
        "INSERT INTO recent_sessions \
         (session_id, working_dir, generated_title, custom_title, updated_at_ms, last_active_at_ms, saved) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(session_id) DO UPDATE SET working_dir = excluded.working_dir, \
         generated_title = excluded.generated_title, custom_title = excluded.custom_title, \
         updated_at_ms = excluded.updated_at_ms, last_active_at_ms = excluded.last_active_at_ms, \
         saved = excluded.saved",
        rusqlite::params![
            snapshot.get("id").and_then(Value::as_str),
            dir.display().to_string(),
            snapshot.get("title").and_then(Value::as_str),
            snapshot.get("custom_title").and_then(Value::as_str),
            ms("updated_at").unwrap_or(0),
            ms("last_active_at"),
            snapshot.get("saved").and_then(Value::as_bool).unwrap_or(false) as i64,
        ],
    );
}

/// Install a pulled jcode session here, under its original id.
pub(crate) fn install(
    adapter: &JCodeAdapter,
    manifest: &Manifest,
    blob: &dyn Fn(&str) -> Result<PathBuf, CoreError>,
    project_dir: Option<&Path>,
    base: Option<&Base>,
) -> Result<Installed, CoreError> {
    manifest.validate().map_err(|msg| CoreError::Invalid { msg })?;
    let id = &manifest.id;
    for sha in manifest.blob_shas() {
        blob(sha)?;
    }
    let file = |name: &str| manifest.files.iter().find(|f| f.path == name).and_then(|f| f.sha256.as_deref());
    let hub = read_json(&blob(file("snapshot.json").ok_or_else(|| {
        CoreError::Invalid { msg: "bundle has no snapshot".into() }
    })?)?)?;
    if hub.get("id").and_then(Value::as_str) != Some(id.as_str()) {
        return Err(CoreError::Invalid { msg: format!("the snapshot is not session {id}") });
    }
    if let Some(pid) = store::live_pid(adapter.root(), id) {
        return Err(CoreError::SessionLive { id: id.clone(), pid: Some(pid) });
    }
    // The journal is only ever half the state; installing it, or leaving
    // one here for jcode to replay onto the hub's snapshot, would make a
    // conversation neither machine had.
    if file("journal.jsonl").is_some() {
        return Err(CoreError::Invalid {
            msg: format!(
                "the pushing machine had turns of {id} jcode had not written into the session yet; \
                 resume it there once and push again"
            ),
        });
    }

    let dest = adapter.sessions_dir().join(format!("{id}.json"));
    if has_journal(&dest) {
        return Err(CoreError::Invalid {
            msg: format!(
                "{} holds turns jcode has not written into the session yet; resume it once so \
                 jcode does, then pull again",
                journal_of(&dest).display()
            ),
        });
    }
    let (outcome, target) = if dest.is_file() {
        let local = read_json(&dest)?;
        let here = PathBuf::from(local.get("working_dir").and_then(Value::as_str).unwrap_or_default());
        if let Some(dir) = project_dir
            && dir.canonicalize().map_err(|e| CoreError::io(dir, e))? != here
        {
            return Err(CoreError::Invalid {
                msg: format!(
                    "session {id} is already here, in {}; pull without --project-dir to update \
                     it there",
                    here.display()
                ),
            });
        }
        let outcome = bundle::with_base(compare(&local, &hub), &canonical_of(&local), &canonical_of(&hub), base);
        (outcome, here)
    } else {
        // A short name taken here by another session is no reason to
        // refuse: jcode itself gives two sessions one name (measured on
        // 0.83), and asm resumes by id.
        (InstallOutcome::New, bundle::target_dir(manifest, project_dir)?)
    };

    if matches!(outcome, InstallOutcome::New | InstallOutcome::Replaced) {
        let bak = dest.with_extension("bak");
        if outcome == InstallOutcome::Replaced {
            let backup = paths::backup_dir("jcode", id)
                .ok_or_else(|| CoreError::Invalid { msg: "cannot determine backup directory".into() })?;
            std::fs::create_dir_all(&backup).map_err(|e| CoreError::io(&backup, e))?;
            for path in [&dest, &bak] {
                if path.is_file() {
                    fsutil::copy_atomic(path, &backup.join(path.file_name().unwrap()))?;
                }
            }
        }
        let sessions = adapter.sessions_dir();
        std::fs::create_dir_all(&sessions).map_err(|e| CoreError::io(&sessions, e))?;
        // jcode's own fallback copy first, so the snapshot is never newer
        // than a backup that disagrees with it about where it lives.
        if let Some(sha) = file("snapshot.bak")
            && let Ok(previous) = read_json(&blob(sha)?)
        {
            fsutil::write_atomic(&bak, &filed_at(previous, &target))?;
        }
        fsutil::write_atomic(&dest, &filed_at(hub.clone(), &target))?;
        remember_in_picker(adapter, &hub, &target);
    }
    Ok(Installed { outcome, project_root: target, path: dest })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(texts: &[&str], dir: &str, opened: &str) -> Value {
        json!({
            "id": "session_otter_1790000000000_0123456789abcdef",
            "short_name": "otter",
            "title": null,
            "created_at": "2026-09-22T10:00:00Z",
            "last_active_at": opened,
            "last_pid": 4242,
            "working_dir": dir,
            "messages": texts.iter().enumerate()
                .map(|(i, t)| json!({ "id": format!("m{i}"), "role": "user", "content": t }))
                .collect::<Vec<_>>(),
        })
    }

    #[test]
    fn messages_decide_and_machine_local_fields_do_not() {
        let a = snapshot(&["one", "two"], "/home/a/notes", "2026-09-22T11:00:00Z");
        // Opened on another machine, in another directory: still the same.
        let b = snapshot(&["one", "two"], "/Users/a/notes", "2026-09-23T09:00:00Z");
        assert_eq!(compare(&b, &a), InstallOutcome::InSync);
        assert_eq!(compare(&snapshot(&["one"], "/x", ""), &a), InstallOutcome::Replaced);
        assert_eq!(compare(&snapshot(&["one", "two", "three"], "/x", ""), &a), InstallOutcome::Ahead);
        assert_eq!(compare(&snapshot(&["one", "other"], "/x", ""), &a), InstallOutcome::Diverged);
    }

    #[test]
    fn a_new_install_is_filed_under_this_machines_directory() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("notes");
        std::fs::create_dir_all(&project).unwrap();
        let adapter = JCodeAdapter::with_root(dir.path().join("jcode"));
        let bytes = serde_json::to_vec(&snapshot(&["one"], "/home/a/notes", "")).unwrap();
        let sha = fsutil::sha256_hex(&bytes);
        let blob_path = dir.path().join(&sha);
        std::fs::write(&blob_path, &bytes).unwrap();
        let manifest: Manifest = serde_json::from_value(json!({
            "schema": crate::hub::manifest::SCHEMA, "agent": "jcode",
            "id": "session_otter_1790000000000_0123456789abcdef",
            "project_root": "/home/a/notes", "project_root_portable": "/home/a/notes",
            "canonical": "x", "files": [{ "path": "snapshot.json", "sha256": sha, "size": bytes.len() }],
        }))
        .unwrap();

        let installed = install(&adapter, &manifest, &|_| Ok(blob_path.clone()), Some(&project), None).unwrap();
        assert_eq!(installed.outcome, InstallOutcome::New);
        let written = read_json(&installed.path).unwrap();
        assert_eq!(written["working_dir"], json!(project.canonicalize().unwrap().display().to_string()));
        assert!(written.get("last_pid").is_none(), "another machine's pid means nothing here");
        assert_eq!(canonical_of(&written), canonical_of(&snapshot(&["one"], "/home/a/notes", "")));
    }
}
