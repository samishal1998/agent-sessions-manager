//! Codex over the hub.
//!
//! The rollout is append-only JSONL, so its complete lines are the
//! conversation's identity, and a newer copy extends an older one: a pull
//! appends, as for Claude Code. The `threads` row travels as JSON beside
//! it, because it is the row codex's own picker reads.
//!
//! Measured on 0.151: `codex exec resume <id>` finds a rollout placed under
//! `sessions/` by its id and writes the `threads` row itself; one that has
//! not been resumed stays out of the picker, so a pull writes the row codex
//! would. `migrate-rollouts --apply` is no way in — it refuses a thread with
//! no row. Codex writes the absolute working directory into the rollout,
//! more than once, so a session continues only in the directory it started.

use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

use super::CodexAdapter;
use crate::hub::bundle::{self, Base, Bundle, InstallOutcome, Installed, Staged};
use crate::hub::manifest::Manifest;
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

/// Codex holds an flock on `thread-writer-locks/<id>.lock` while a process
/// writes the thread (measured on 0.151). Taking it proves the session idle
/// and keeps codex from starting to write while asm does; dropping the file
/// releases it.
struct WriterLock(#[allow(dead_code)] std::fs::File);

fn take_writer_lock(adapter: &CodexAdapter, id: &str) -> Result<WriterLock, CoreError> {
    let dir = adapter.root().join("thread-writer-locks");
    std::fs::create_dir_all(&dir).map_err(|e| CoreError::io(&dir, e))?;
    let path = dir.join(format!("{id}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| CoreError::io(&path, e))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // Safety: flock on a descriptor this function owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(CoreError::SessionLive { id: id.to_string(), pid: None });
        }
    }
    Ok(WriterLock(file))
}

/// Every rollout of this id here, live or archived.
fn find_local(adapter: &CodexAdapter, id: &str) -> Vec<PathBuf> {
    let suffix = format!("-{id}.jsonl");
    let mut found = Vec::new();
    let mut stack = vec![adapter.sessions_dir(), adapter.root().join("archived_sessions")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-") && n.ends_with(&suffix)) {
                found.push(path);
            }
        }
    }
    found
}

/// Where the rollout goes inside this CODEX_HOME: where the pushing machine
/// kept it, if that is a plain path under `sessions/` naming this id. One
/// codex had archived (flat, in `archived_sessions/`) goes back under
/// `sessions/<date>/`, from the date in its name.
fn rollout_rel(manifest: &Manifest) -> Option<PathBuf> {
    let rel = PathBuf::from(manifest.extra.get("rollout_rel")?.as_str()?);
    let name = rel.file_name()?.to_str()?;
    let plain = rel.components().all(|c| matches!(c, Component::Normal(_)));
    if !plain || !name.starts_with("rollout-") || !name.ends_with(&format!("-{}.jsonl", manifest.id)) {
        return None;
    }
    if rel.starts_with("sessions") {
        return Some(rel);
    }
    // rollout-YYYY-MM-DDTHH-MM-SS-<id>.jsonl
    let date = name.get(8..18)?;
    let (y, m, d) = (date.get(0..4)?, date.get(5..7)?, date.get(8..10)?);
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    (rel.parent() == Some(Path::new("archived_sessions")) && digits(y) && digits(m) && digits(d))
        .then(|| Path::new("sessions").join(y).join(m).join(d).join(name))
}

/// Write the `threads` row codex would write on the first resume, so its
/// picker lists the session before then. Into a database codex already
/// made, never a new one; a row codex will rewrite anyway, so failure is
/// ignored.
fn remember_in_picker(adapter: &CodexAdapter, id: &str, row: &Value, rollout: &Path) {
    let db = adapter.state_db();
    let Some(row) = row.as_object() else { return };
    if !db.is_file() {
        return;
    }
    let Ok(conn) = rusqlite::Connection::open(&db) else { return };
    let _ = conn.busy_timeout(std::time::Duration::from_secs(2));
    let mut row = row.clone();
    // Its own id, where it now lives, and not archived: the rollout was
    // just filed under sessions/.
    row.insert("id".into(), json!(id));
    row.insert("rollout_path".into(), json!(rollout.display().to_string()));
    for (key, value) in [("archived", json!(0)), ("archived_at", Value::Null)] {
        if row.contains_key(key) {
            row.insert(key.into(), value);
        }
    }
    let columns: Vec<&String> = row.keys().collect();
    let names: Vec<String> = columns.iter().map(|c| format!("\"{}\"", c.replace('"', ""))).collect();
    let values: Vec<rusqlite::types::Value> = columns
        .iter()
        .map(|c| match &row[*c] {
            Value::Null => rusqlite::types::Value::Null,
            Value::Bool(b) => rusqlite::types::Value::Integer(*b as i64),
            Value::Number(n) => n.as_i64().map_or(rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0)), rusqlite::types::Value::Integer),
            Value::String(s) => rusqlite::types::Value::Text(s.clone()),
            other => rusqlite::types::Value::Text(other.to_string()),
        })
        .collect();
    let _ = conn.execute(
        &format!("INSERT OR IGNORE INTO threads ({}) VALUES ({})", names.join(", "), vec!["?"; names.len()].join(", ")),
        rusqlite::params_from_iter(values),
    );
}

/// Install a pulled Codex session here, under its original id, in the
/// directory it started in.
pub(crate) fn install(
    adapter: &CodexAdapter,
    manifest: &Manifest,
    blob: &dyn Fn(&str) -> Result<PathBuf, CoreError>,
    project_dir: Option<&Path>,
    _base: Option<&Base>,
) -> Result<Installed, CoreError> {
    manifest.validate().map_err(|msg| CoreError::Invalid { msg })?;
    let id = &manifest.id;
    for sha in manifest.blob_shas() {
        blob(sha)?;
    }
    let file = |name: &str| manifest.files.iter().find(|f| f.path == name).and_then(|f| f.sha256.as_deref());
    let src = blob(file("rollout.jsonl").ok_or_else(|| CoreError::Invalid { msg: "bundle has no rollout".into() })?)?;
    let raw = std::fs::read(&src).map_err(|e| CoreError::io(&src, e))?;
    let incoming = bundle::complete_lines(&raw);
    let rel = rollout_rel(manifest)
        .ok_or_else(|| CoreError::Invalid { msg: "the bundle does not say where its rollout lived".into() })?;

    let dir = PathBuf::from(&manifest.project_root);
    let same = |a: &Path, b: &Path| a.canonicalize().ok().is_some_and(|a| b.canonicalize().ok() == Some(a));
    if !dir.is_dir() || project_dir.is_some_and(|p| !same(p, &dir)) {
        return Err(CoreError::Invalid {
            msg: format!(
                "codex writes {} into the conversation itself and continues it only there; it \
                 does not exist on this machine, so this session cannot be restored here",
                dir.display()
            ),
        });
    }

    let _lock = take_writer_lock(adapter, id)?;
    let existing = find_local(adapter, id);
    let (outcome, path) = match existing.as_slice() {
        [] => {
            let dest = adapter.root().join(&rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| CoreError::io(parent, e))?;
            }
            fsutil::write_atomic(&dest, incoming)?;
            if let Some(sha) = file("thread.json")
                && let Ok(bytes) = std::fs::read(blob(sha)?)
                && let Ok(row) = serde_json::from_slice::<Value>(&bytes)
            {
                remember_in_picker(adapter, id, &row, &dest);
            }
            (InstallOutcome::New, dest)
        }
        [path] => {
            let local_raw = std::fs::read(path).map_err(|e| CoreError::io(path, e))?;
            if local_raw.last().is_some_and(|&b| b != b'\n') {
                return Err(CoreError::Invalid {
                    msg: format!("{} ends in an unfinished line; resume the session once so codex finishes it", path.display()),
                });
            }
            let outcome = if local_raw == incoming {
                InstallOutcome::InSync
            } else if incoming.starts_with(&local_raw) {
                let delta = &incoming[local_raw.len()..];
                let mut f = std::fs::OpenOptions::new().append(true).open(path).map_err(|e| CoreError::io(path, e))?;
                f.write_all(delta).map_err(|e| CoreError::io(path, e))?;
                InstallOutcome::FastForward { appended: delta.len() as u64 }
            } else if local_raw.starts_with(incoming) {
                InstallOutcome::Ahead
            } else {
                InstallOutcome::Diverged
            };
            (outcome, path.clone())
        }
        many => {
            return Err(CoreError::Invalid {
                msg: format!("session {id} has {} rollouts here already; resolve that first", many.len()),
            });
        }
    };
    Ok(Installed { outcome, project_root: dir, path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const ID: &str = "01a020ab-9a2c-71a1-8384-1e1058de5553";

    fn bundle_of(project: &Path, lines: &[&str]) -> (Manifest, HashMap<String, Vec<u8>>) {
        let rollout: String = lines.iter().map(|l| format!("{{\"type\":\"{l}\"}}\n")).collect();
        let sha = fsutil::sha256_hex(rollout.as_bytes());
        let manifest: Manifest = serde_json::from_value(json!({
            "schema": crate::hub::manifest::SCHEMA, "agent": "codex", "id": ID,
            "project_root": project.display().to_string(),
            "project_root_portable": project.display().to_string(),
            "canonical": sha,
            "files": [{ "path": "rollout.jsonl", "sha256": sha, "size": rollout.len() }],
            "extra": { "rollout_rel": format!("sessions/2026/08/17/rollout-2026-08-17T14-00-00-{ID}.jsonl") },
        }))
        .unwrap();
        (manifest, HashMap::from([(sha, rollout.into_bytes())]))
    }

    fn pull(adapter: &CodexAdapter, pushed: &(Manifest, HashMap<String, Vec<u8>>), dir: &Path) -> Result<Installed, CoreError> {
        let blob = |sha: &str| {
            let path = dir.join(format!("blob-{sha}"));
            std::fs::write(&path, &pushed.1[sha]).unwrap();
            Ok(path)
        };
        install(adapter, &pushed.0, &blob, None, None)
    }

    #[test]
    fn appends_like_a_transcript_and_only_where_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("mercury");
        std::fs::create_dir_all(&project).unwrap();
        let adapter = CodexAdapter::with_root(dir.path().join("codex"));

        let first = pull(&adapter, &bundle_of(&project, &["session_meta", "turn"]), dir.path()).unwrap();
        assert_eq!(first.outcome, InstallOutcome::New);
        assert!(first.path.ends_with(format!("sessions/2026/08/17/rollout-2026-08-17T14-00-00-{ID}.jsonl")));
        let before = std::fs::read(&first.path).unwrap();

        let next = pull(&adapter, &bundle_of(&project, &["session_meta", "turn", "reply"]), dir.path()).unwrap();
        assert!(matches!(next.outcome, InstallOutcome::FastForward { .. }));
        assert!(std::fs::read(&next.path).unwrap().starts_with(&before), "appended, never rewritten");
        let other = pull(&adapter, &bundle_of(&project, &["session_meta", "elsewhere"]), dir.path()).unwrap();
        assert_eq!(other.outcome, InstallOutcome::Diverged);

        let err = pull(&adapter, &bundle_of(Path::new("/nonexistent/mercury"), &["x"]), dir.path()).unwrap_err();
        assert!(err.to_string().contains("continues it only there"), "{err}");
    }

    #[test]
    fn a_rollout_codex_archived_goes_back_under_its_date() {
        let (mut manifest, _) = bundle_of(Path::new("/x"), &["session_meta"]);
        let name = format!("rollout-2026-08-17T14-00-00-{ID}.jsonl");
        manifest.extra = json!({ "rollout_rel": format!("archived_sessions/{name}") });
        assert_eq!(rollout_rel(&manifest).unwrap(), Path::new("sessions/2026/08/17").join(&name));
        for bad in [format!("archived_sessions/x/{name}"), format!("../{name}"), format!("sessions/../../{name}")] {
            manifest.extra = json!({ "rollout_rel": bad });
            assert!(rollout_rel(&manifest).is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn never_while_codex_holds_the_thread() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("mercury");
        std::fs::create_dir_all(&project).unwrap();
        let adapter = CodexAdapter::with_root(dir.path().join("codex"));
        let locks = adapter.root().join("thread-writer-locks");
        std::fs::create_dir_all(&locks).unwrap();
        // A second open file description, as a codex process would hold.
        let held = std::fs::File::create(locks.join(format!("{ID}.lock"))).unwrap();
        assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
        let err = pull(&adapter, &bundle_of(&project, &["session_meta"]), dir.path()).unwrap_err();
        assert!(matches!(err, CoreError::SessionLive { .. }), "{err}");
        assert!(find_local(&adapter, ID).is_empty());
    }
}
