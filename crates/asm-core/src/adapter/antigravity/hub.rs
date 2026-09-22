//! Antigravity over the hub.
//!
//! The conversation is a SQLite database with most of a fresh conversation
//! in its write-ahead log, so it is snapshotted with `VACUUM INTO` — one
//! consistent file, taken through a read-only connection — rather than
//! copied. Antigravity's JSONL rendering of the same steps is the identity
//! when it exists, because the database bytes change on every vacuum.
//!
//! Measured on agy 1.1.22, in two fresh homes: `conversations/<id>.db`
//! alone is enough for `agy --conversation <id>` to resume a conversation
//! with its context, from any directory — nothing in it names the
//! workspace. agy rebuilds its caches itself and, on that first resume,
//! records the directory it ran in. So a pull writes the database and
//! `brain/<id>/` (the transcript asm reads, which agy then appends to) and
//! no cache at all: `last_conversations.json` holds one conversation per
//! directory, and writing it would take the directory from another.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::AntigravityAdapter;
use crate::hub::bundle::{self, Base, Bundle, InstallOutcome, Installed, Staged};
use crate::hub::manifest::Manifest;
use crate::model::Session;
use crate::{CoreError, fsutil, paths};

const TRANSCRIPT: &str = "brain/.system_generated/logs/transcript.jsonl";

pub(crate) fn collect(adapter: &AntigravityAdapter, session: &Session) -> Result<Bundle, CoreError> {
    let id = &session.handle.native_id;
    let db = adapter.conversations_dir().join(format!("{id}.db"));

    let tmp_dir = paths::tmp_dir()?;
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

/// Install a pulled conversation here, under its original id. The database
/// is rewritten whole by agy, so an older copy here is one whose transcript
/// is a prefix of the hub's; it is backed up and replaced.
pub(crate) fn install(
    adapter: &AntigravityAdapter,
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
    let db_src = blob(file("conversation.db").ok_or_else(|| CoreError::Invalid { msg: "bundle has no conversation".into() })?)?;
    let hub_lines = match file(TRANSCRIPT) {
        Some(sha) => {
            let path = blob(sha)?;
            std::fs::read(&path).map_err(|e| CoreError::io(&path, e))?
        }
        None => Vec::new(),
    };
    let hub_lines = bundle::complete_lines(&hub_lines).to_vec();

    // agy holds this while a process has the conversation open; holding it
    // here keeps agy out while asm writes.
    let _lock = fsutil::lock_exclusive(&adapter.root().join("presence").join(format!("{id}.lock")))?
        .ok_or_else(|| CoreError::SessionLive { id: id.clone(), pid: None })?;

    let db = adapter.conversations_dir().join(format!("{id}.db"));
    let mut wal = db.clone().into_os_string();
    wal.push("-wal");
    let wal = PathBuf::from(wal);
    if std::fs::metadata(&wal).is_ok_and(|m| m.len() > 0) {
        // SQLite would replay it onto whatever database is put here.
        return Err(CoreError::Invalid {
            msg: format!(
                "{} holds changes agy has not written into the conversation yet; resume it once \
                 so agy does, then pull again",
                wal.display()
            ),
        });
    }
    let local_transcript = adapter.transcript_of(id);
    let outcome = if db.is_file() {
        let raw = std::fs::read(&local_transcript).unwrap_or_default();
        let local = bundle::complete_lines(&raw);
        let content = if local == hub_lines.as_slice() {
            InstallOutcome::InSync
        } else if hub_lines.starts_with(local) {
            InstallOutcome::Replaced
        } else if local.starts_with(&hub_lines) {
            InstallOutcome::Ahead
        } else {
            InstallOutcome::Diverged
        };
        bundle::with_base(content, &fsutil::sha256_hex(local), &manifest.canonical, base)
    } else {
        InstallOutcome::New
    };
    // Not bound to a directory: this is only where the resume hint points.
    let project_root = bundle::target_dir(manifest, project_dir).unwrap_or_default();
    let installed = Installed { outcome, project_root, path: db.clone() };
    if !matches!(outcome, InstallOutcome::New | InstallOutcome::Replaced) {
        return Ok(installed);
    }

    let brain = adapter.root().join("brain").join(id);
    if outcome == InstallOutcome::Replaced {
        let backup = paths::backup_dir("antigravity", id)
            .ok_or_else(|| CoreError::Invalid { msg: "cannot determine backup directory".into() })?;
        std::fs::create_dir_all(&backup).map_err(|e| CoreError::io(&backup, e))?;
        fsutil::copy_atomic(&db, &backup.join(format!("{id}.db")))?;
        if brain.is_dir() {
            fsutil::copy_recursive(&brain, &backup.join("brain"))?;
        }
    }
    // The brain first and the database last: until the database is there,
    // agy does not know the conversation.
    for entry in &manifest.files {
        let (Some(rest), Some(sha)) = (entry.path.strip_prefix("brain/"), &entry.sha256) else {
            continue;
        };
        let dest = brain.join(rest);
        bundle::real_dirs(&brain, dest.parent().unwrap_or(&brain))?;
        if std::fs::symlink_metadata(&dest).is_ok_and(|m| !m.is_file()) {
            continue;
        }
        fsutil::copy_atomic(&blob(sha)?, &dest)?;
    }
    let dir = adapter.conversations_dir();
    std::fs::create_dir_all(&dir).map_err(|e| CoreError::io(&dir, e))?;
    for stale in [PathBuf::from(format!("{}-shm", db.display())), wal] {
        // Empty (checked above for the log): what an unclean exit leaves.
        let _ = std::fs::remove_file(stale);
    }
    fsutil::copy_atomic(&db_src, &db)?;
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const ID: &str = "9417417d-592d-49fc-b8b3-662992f52122";

    fn bundle_of(lines: &[&str]) -> (Manifest, HashMap<String, Vec<u8>>) {
        let transcript: String = lines.iter().map(|l| format!("{{\"step\":\"{l}\"}}\n")).collect();
        let db = b"SQLite format 3\0 stand-in".to_vec();
        let (ts, ds) = (fsutil::sha256_hex(transcript.as_bytes()), fsutil::sha256_hex(&db));
        let manifest: Manifest = serde_json::from_value(json!({
            "schema": crate::hub::manifest::SCHEMA, "agent": "antigravity", "id": ID,
            "project_root": "", "project_root_portable": "",
            "canonical": ts,
            "files": [
                { "path": "conversation.db", "sha256": ds, "size": db.len() },
                { "path": TRANSCRIPT, "sha256": ts, "size": transcript.len() },
            ],
        }))
        .unwrap();
        (manifest, HashMap::from([(ts, transcript.into_bytes()), (ds, db)]))
    }

    fn pull(adapter: &AntigravityAdapter, pushed: &(Manifest, HashMap<String, Vec<u8>>), scratch: &Path) -> Result<Installed, CoreError> {
        let blob = |sha: &str| {
            let path = scratch.join(sha);
            std::fs::write(&path, &pushed.1[sha]).unwrap();
            Ok(path)
        };
        install(adapter, &pushed.0, &blob, None, None)
    }

    #[test]
    fn a_new_conversation_is_the_database_and_its_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = AntigravityAdapter::with_root(dir.path().join("agy"));
        let pushed = bundle_of(&["hello", "reply"]);
        assert_eq!(pull(&adapter, &pushed, dir.path()).unwrap().outcome, InstallOutcome::New);
        assert!(adapter.conversations_dir().join(format!("{ID}.db")).is_file());
        assert_eq!(std::fs::read(adapter.transcript_of(ID)).unwrap(), pushed.1[&pushed.0.canonical]);
        // Never a cache entry: that would take a directory's binding.
        assert!(!adapter.workspaces_file().exists() && !adapter.metadata_file().exists());

        assert_eq!(pull(&adapter, &pushed, dir.path()).unwrap().outcome, InstallOutcome::InSync);
        assert_eq!(pull(&adapter, &bundle_of(&["hello", "other"]), dir.path()).unwrap().outcome, InstallOutcome::Diverged);
    }

    #[test]
    fn never_while_agy_has_it_or_has_unwritten_changes() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = AntigravityAdapter::with_root(dir.path().join("agy"));
        pull(&adapter, &bundle_of(&["hello"]), dir.path()).unwrap();

        let held = fsutil::lock_exclusive(&adapter.root().join("presence").join(format!("{ID}.lock"))).unwrap();
        let err = pull(&adapter, &bundle_of(&["hello", "more"]), dir.path()).unwrap_err();
        assert!(matches!(err, CoreError::SessionLive { .. }), "{err}");
        drop(held);

        std::fs::write(adapter.conversations_dir().join(format!("{ID}.db-wal")), b"frames").unwrap();
        let err = pull(&adapter, &bundle_of(&["hello", "more"]), dir.path()).unwrap_err();
        assert!(err.to_string().contains("not written"), "{err}");
    }
}
