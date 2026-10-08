//! Rename and delete on an OpenCode 2.x store.
//!
//! Both go through the CLI (`cli2`), never through SQL: 2.x rows are
//! projections of an event log a running service owns. What asm adds is the
//! guard and the safety net around them:
//!
//! - A session is BUSY when a turn has claimed it (`time_suspended`, whether
//!   the service running it is up or died and left the claim for its next
//!   start) or work is queued for it (`session_inbox`, `session_pending`).
//!   Every change refuses a busy session: starting `opencode` to apply it
//!   could resume that turn, and a delete would pull the session out from
//!   under it.
//! - Before a session is destroyed, a backup is written: one transfer
//!   document per session of the tree, each of which `opencode session
//!   import` takes as is, plus a `manifest.json` naming each session's
//!   directory (a restore is `opencode session import <file> --directory
//!   <directory>`, the root first: a child needs its parent in the store).
//!   Built by the read-only exporter, so the backup never depends on the
//!   thing being backed up behaving. It holds EVERYTHING stored, including a
//!   turn that never settled (the import drops those again, as `session
//!   export` does), it is on disk (file and directory fsynced, read back)
//!   before anything is deleted, and it is private to the user.
//! - Just before the delete the tree is looked at again: if a session
//!   appeared, a turn started or a message arrived since the backup was
//!   read, nothing is deleted (the backup stays) and the user is told.
//! - There is no archive or move here. 2.0.25 has no way to archive (the
//!   column is written by migration and import only, and nothing filters on
//!   it), and `session.move` only queues a request for a persistent server
//!   while a one-shot `--standalone` leaves an orphaned claim.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::adapter::DeleteReport;
use crate::model::Session;
use crate::CoreError;

use super::OpenCodeAdapter;
use super::v2;

const AGENT: &str = "opencode";

fn sql(adapter: &OpenCodeAdapter) -> impl Fn(rusqlite::Error) -> CoreError + '_ {
    |e| CoreError::Sqlite { db: adapter.db().to_path_buf(), source: Box::new(e) }
}

fn invalid(msg: impl Into<String>) -> CoreError {
    CoreError::Invalid { msg: msg.into() }
}

/// Why a session cannot be changed right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Busy {
    /// A turn has claimed it.
    Claimed,
    /// Work is queued for it.
    Queued,
}

/// Why `id` cannot be changed right now, if it cannot.
pub(super) fn busy(conn: &Connection, id: &str) -> rusqlite::Result<Option<Busy>> {
    // A table an older 2.x does not have holds nothing queued.
    let one = |sql: &str| conn.query_row(sql, [id], |_| Ok(())).map(|()| true).or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(false),
        rusqlite::Error::SqliteFailure(_, Some(ref msg)) if msg.starts_with("no such table") => Ok(false),
        e => Err(e),
    });
    if one("SELECT 1 FROM session_v2 WHERE id = ?1 AND time_suspended IS NOT NULL")? {
        return Ok(Some(Busy::Claimed));
    }
    if one("SELECT 1 FROM session_inbox WHERE session_id = ?1 LIMIT 1")? || one("SELECT 1 FROM session_pending WHERE session_id = ?1 LIMIT 1")? {
        return Ok(Some(Busy::Queued));
    }
    Ok(None)
}

/// Refuse when any of `ids` is busy, saying what is true: a turn that is
/// running, or one a service that is no longer up left unfinished.
pub(super) fn refuse_busy(adapter: &OpenCodeAdapter, conn: &Connection, ids: &[String]) -> Result<(), CoreError> {
    for id in ids {
        match busy(conn, id).map_err(sql(adapter))? {
            None => {}
            Some(Busy::Queued) => {
                return Err(CoreError::StoreBusy {
                    agent: AGENT,
                    detail: format!("session {id}: work is queued for it, let OpenCode finish it"),
                });
            }
            Some(Busy::Claimed) if v2::service_pid(adapter).is_some() => {
                return Err(CoreError::StoreBusy { agent: AGENT, detail: format!("session {id}: a turn is running, let it finish") });
            }
            // Not "close OpenCode": it is not running. And starting it is the
            // user's call: its boot is what picks such turns up.
            Some(Busy::Claimed) => {
                return Err(invalid(format!(
                    "session {id}: this session has an unfinished turn from an OpenCode that is no longer running; \
                     start OpenCode once to finish or abandon it, then retry. Nothing was changed"
                )));
            }
        }
    }
    Ok(())
}

/// A private file (0600) that is on disk when this returns.
pub(super) fn write_durable(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path).map_err(|e| CoreError::io(path, e))?;
    file.write_all(bytes).and_then(|()| file.sync_all()).map_err(|e| CoreError::io(path, e))
}

/// Flush a directory's entries to disk.
fn sync_dir(dir: &Path) -> Result<(), CoreError> {
    fs::File::open(dir).and_then(|d| d.sync_all()).map_err(|e| CoreError::io(dir, e))
}

/// A new private (0700) directory at `want`, or beside it with a numeric
/// suffix when something is already there: two backups in one millisecond
/// must not share a directory.
fn private_dir(want: &Path) -> Result<PathBuf, CoreError> {
    if let Some(parent) = want.parent() {
        fs::create_dir_all(parent).map_err(|e| CoreError::io(parent, e))?;
    }
    for n in 0..1000 {
        let path = if n == 0 {
            want.to_path_buf()
        } else {
            let mut name = want.file_name().unwrap_or_default().to_os_string();
            name.push(format!("-{n}"));
            want.with_file_name(name)
        };
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        match builder.create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(CoreError::io(&path, e)),
        }
    }
    Err(invalid(format!("cannot find a free backup directory next to {}", want.display())))
}

#[cfg(test)]
pub(super) fn private_dir_for_test(want: &Path) -> Result<PathBuf, CoreError> {
    private_dir(want)
}

fn new_backup_dir(adapter: &OpenCodeAdapter, id: &str) -> Result<PathBuf, CoreError> {
    let want = adapter.backup_dir(id).ok_or_else(|| invalid("cannot determine backup directory"))?;
    private_dir(&want)
}

/// Write the transfer documents of `tree` into a fresh backup directory
/// (`00-<id>.json`, parents first, so importing in name order restores it)
/// and a `manifest.json` that says where each one goes. Everything is
/// fsynced, the directory too, and read back before this returns: the caller
/// destroys the original next.
pub(super) fn backup_tree(adapter: &OpenCodeAdapter, root: &str, tree: &[Value]) -> Result<PathBuf, CoreError> {
    let backup = new_backup_dir(adapter, root)?;
    let mut written: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut sessions = Vec::new();
    for (i, transfer) in tree.iter().enumerate() {
        let id = transfer.pointer("/info/id").and_then(Value::as_str).unwrap_or("unknown");
        let name = format!("{i:02}-{id}.json");
        let bytes = serde_json::to_vec_pretty(transfer).unwrap();
        write_durable(&backup.join(&name), &bytes)?;
        sessions.push(json!({
            "id": id,
            "file": name,
            "directory": transfer.pointer("/info/location/directory"),
            "parentID": transfer.pointer("/info/parentID"),
            "messages": transfer.get("messages").and_then(Value::as_array).map_or(0, Vec::len),
        }));
        written.push((backup.join(name), bytes));
    }
    let manifest = json!({
        "root": root,
        "created": jiff::Timestamp::now().to_string(),
        "restore": "for each session in this order: opencode session import <file> --directory <directory>. \
                    A child needs its parent imported first. Messages of a turn that never finished are in \
                    the files but OpenCode's import leaves them out.",
        "sessions": sessions,
    });
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    write_durable(&backup.join("manifest.json"), &bytes)?;
    written.push((backup.join("manifest.json"), bytes));
    sync_dir(&backup)?;
    if let Some(parent) = backup.parent() {
        sync_dir(parent)?;
    }
    // What is on disk now is what was meant to be, and all of it.
    let listed = fs::read_dir(&backup).map_err(|e| CoreError::io(&backup, e))?.count();
    let intact = written.iter().all(|(path, bytes)| fs::read(path).is_ok_and(|on_disk| &on_disk == bytes));
    if listed != tree.len() + 1 || !intact {
        return Err(invalid(format!(
            "the backup in {} did not read back as written; nothing was changed",
            backup.display()
        )));
    }
    Ok(backup)
}

/// The `info` of every session of `tree` saved, one file each, before titles
/// are changed.
pub(super) fn backup_infos(adapter: &OpenCodeAdapter, root: &str, tree: &[Value]) -> Result<PathBuf, CoreError> {
    let backup = new_backup_dir(adapter, root)?;
    for (i, t) in tree.iter().enumerate() {
        let id = t.pointer("/info/id").and_then(Value::as_str).unwrap_or("unknown");
        write_durable(&backup.join(format!("{i:02}-{id}.info.json")), &serde_json::to_vec_pretty(&t["info"]).unwrap())?;
    }
    sync_dir(&backup)?;
    Ok(backup)
}

/// What to tell the user about restoring a backup made by [`backup_tree`].
pub(super) fn restore_hint(backup: &Path) -> String {
    format!(
        "To restore, run `opencode session import <file> --directory <directory>` for each session in {} in file order \
         (the directories are in manifest.json)",
        backup.display()
    )
}

/// The moment before the destroying call: the tree must be what the backup
/// was made of. `ids` and `signature` were read before the backup; if a
/// session appeared, a turn started or a message arrived since, nothing is
/// deleted and the backup is kept.
pub(super) fn recheck(
    adapter: &OpenCodeAdapter,
    conn: &Connection,
    root: &str,
    ids: &[String],
    signature: &[(String, i64, i64)],
    backup: &Path,
) -> Result<(), CoreError> {
    let changed = |what: &str| {
        invalid(format!(
            "{what} while the backup was being made; nothing was deleted. The backup is in {}; try again",
            backup.display()
        ))
    };
    let now = v2::tree_ids(conn, root).map_err(sql(adapter))?;
    let (mut a, mut b) = (now.clone(), ids.to_vec());
    a.sort();
    b.sort();
    if a != b {
        return Err(changed("the session tree changed (a session was added or removed)"));
    }
    if let Err(e) = refuse_busy(adapter, conn, &now) {
        return Err(invalid(format!("{e}. The backup is in {}; nothing was deleted", backup.display())));
    }
    if v2::signature(conn, &now).map_err(sql(adapter))? != signature {
        return Err(changed("the session was continued"));
    }
    Ok(())
}

/// After a delete: none of `ids` is left, and nothing hangs from them (a
/// session added below the tree at the last moment would be an orphan).
pub(super) fn verify_gone(adapter: &OpenCodeAdapter, conn: &Connection, ids: &[String], backup: &Path) -> Result<(), CoreError> {
    for gone in ids {
        let child: Option<String> = conn
            .query_row("SELECT id FROM session_v2 WHERE parent_id = ?1 LIMIT 1", [gone], |r| r.get(0))
            .map(Some)
            .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e) })
            .map_err(sql(adapter))?;
        let left = if v2::info_of(conn, gone).map_err(sql(adapter))?.is_some() { Some(gone.clone()) } else { child };
        if let Some(left) = left {
            return Err(invalid(format!(
                "opencode accepted the delete but session {left} is still there; a backup is in {}. {}",
                backup.display(),
                restore_hint(backup)
            )));
        }
    }
    Ok(())
}

pub(super) fn rename(adapter: &OpenCodeAdapter, session: &Session, title: &str) -> Result<(), CoreError> {
    let id = &session.handle.native_id;
    if title.trim().is_empty() {
        return Err(invalid("a session title cannot be empty"));
    }
    let conn = super::super::open_ro(adapter.db())?;
    let info = v2::info_of(&conn, id)
        .map_err(sql(adapter))?
        .ok_or_else(|| invalid(format!("session {id} not found in store")))?;
    refuse_busy(adapter, &conn, std::slice::from_ref(id))?;
    // The title is all a rename changes: keep the old one.
    let backup = new_backup_dir(adapter, id)?;
    let path = backup.join("info.json");
    write_durable(&path, &serde_json::to_vec_pretty(&info).unwrap())?;
    sync_dir(&backup)?;

    adapter.cli().update_title(id, title)?;
    let now = v2::info_of(&conn, id).map_err(sql(adapter))?;
    let renamed = now.as_ref().and_then(|i| i.get("title")).and_then(Value::as_str) == Some(title);
    if !renamed {
        return Err(invalid(format!("opencode accepted the rename but the title of {id} did not change; the old one is in {}", path.display())));
    }
    Ok(())
}

pub(super) fn delete(adapter: &OpenCodeAdapter, session: &Session) -> Result<DeleteReport, CoreError> {
    let id = &session.handle.native_id;
    let conn = super::super::open_ro(adapter.db())?;
    let ids = v2::tree_ids(&conn, id).map_err(sql(adapter))?;
    if v2::info_of(&conn, id).map_err(sql(adapter))?.is_none() {
        return Err(invalid(format!("session {id} not found in store")));
    }
    refuse_busy(adapter, &conn, &ids)?;
    let signature = v2::signature(&conn, &ids).map_err(sql(adapter))?;
    let tree = v2::transfer_tree(&conn, id, false).map_err(sql(adapter))?;
    let backup = backup_tree(adapter, id, &tree)?;

    recheck(adapter, &conn, id, &ids, &signature, &backup)?;
    adapter
        .cli()
        .delete(id)
        .map_err(|e| invalid(format!("{e}. Your backup is in {}. {}", backup.display(), restore_hint(&backup))))?;
    verify_gone(adapter, &conn, &ids, &backup)?;
    Ok(DeleteReport {
        backup_dir: Some(backup.clone()),
        removed: vec![adapter.db().to_path_buf()],
        note: Some(restore_hint(&backup)),
    })
}
