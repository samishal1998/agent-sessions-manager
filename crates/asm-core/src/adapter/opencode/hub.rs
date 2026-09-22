//! OpenCode over the hub.
//!
//! Built by reading the rows directly, not by `opencode export`: measured
//! against 1.18.31, export writes to the store on every call, and it also
//! drops a session's todos, shares, inputs and subagent sessions. What is
//! uploaded is the session and every descendant, from every table that
//! holds them, plus the per-session diff file.
//!
//! Installed the same way round. `opencode import` (read from the 1.18.31
//! binary) upserts only a session row's project, directory and path, and
//! inserts messages and parts with ON CONFLICT DO NOTHING while stamping
//! them `time_updated = now`: it could never bring an older copy up to date,
//! and it drops the same four things export does. So it is used only for
//! what nothing else can do — registering this machine's project for the
//! target directory, which fixes the session's project id — and the rows
//! then go in exactly as pushed, in one transaction, after a backup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{Map, Value, json};

use super::{OpenCodeAdapter, write};
use crate::hub::bundle::{self, Bundle, InstallOutcome, Installed, Staged};
use crate::hub::manifest::Manifest;
use crate::model::Session;
use crate::{CoreError, fsutil};

const TABLES: [&str; 5] = ["message", "part", "todo", "session_share", "session_input"];

/// Each table with the column that moves when one of its rows changes.
const WATCHED: [(&str, &str); 5] = [
    ("message", "time_updated"),
    ("part", "time_updated"),
    ("todo", "time_updated"),
    ("session_share", "time_updated"),
    ("session_input", "time_created"),
];

/// What moves when anything the bundle holds does: every descendant's
/// session row, and each table's row count and newest time for it. A
/// subagent continued on its own, or a todo ticked off, changes it though
/// the root's messages do not. Push compares it to skip what has not
/// changed, and it is the bundle's identity.
fn watermark(conn: &Connection, root: &str) -> String {
    let mut parts = Vec::new();
    for id in write::with_descendants(conn, root) {
        let ask = |sql: &str| {
            conn.query_row(sql, [&id], |r| r.get::<_, String>(0)).unwrap_or_else(|_| "?".into())
        };
        parts.push(format!("{id}:{}", ask("SELECT COALESCE(time_updated, 0) || '' FROM session WHERE id = ?1")));
        for (table, col) in WATCHED {
            parts.push(ask(&format!(
                "SELECT COUNT(*) || ':' || COALESCE(MAX({col}), 0) FROM {table} WHERE session_id = ?1"
            )));
        }
    }
    format!("tree:{}", fsutil::sha256_hex(parts.join("\n").as_bytes()))
}

pub(crate) fn fingerprint(adapter: &OpenCodeAdapter, session: &Session) -> Option<String> {
    let conn = super::super::open_ro(adapter.db()).ok()?;
    Some(watermark(&conn, &session.handle.native_id))
}

/// Every row of a session and its descendants, by table, as JSON.
fn dump_tree(conn: &Connection, root: &str) -> Map<String, Value> {
    let mut rows = Map::new();
    for id in write::with_descendants(conn, root) {
        let mut dump = Map::new();
        dump.insert("session".into(), Value::Array(super::super::dump_rows(conn, "session", "id", &id)));
        for table in TABLES {
            if write::table_exists(conn, table) {
                dump.insert(table.into(), Value::Array(super::super::dump_rows(conn, table, "session_id", &id)));
            }
        }
        rows.insert(id, Value::Object(dump));
    }
    rows
}

pub(crate) fn collect(adapter: &OpenCodeAdapter, session: &Session) -> Result<Bundle, CoreError> {
    let conn = super::super::open_ro(adapter.db())?;
    let root = &session.handle.native_id;
    // One read snapshot for the watermark and every row, so they agree with
    // each other: a message written meanwhile is in neither, and the next
    // push sees the watermark move.
    let _ = conn.execute_batch("BEGIN");
    let canonical = watermark(&conn, root);
    let rows = dump_tree(&conn, root);
    let _ = conn.execute_batch("COMMIT");
    let mut files = Vec::new();
    for id in rows.keys() {
        if let Some(diff) = write::session_diff_path(adapter, id)
            && let Some(staged) = Staged::file(&format!("session_diff/{id}.json"), &diff)?
        {
            files.push(staged);
        }
    }
    let body = serde_json::to_vec(&json!({ "root": root, "sessions": rows })).unwrap();
    files.insert(0, Staged::bytes("rows.json", body));
    Ok(Bundle { files, canonical, extra: Value::Null })
}

/// How each table's rows are told apart, and the column that moves when
/// one changes.
const KEYS: [(&str, &[&str], &str); 6] = [
    ("session", &["id"], "time_updated"),
    ("message", &["id"], "time_updated"),
    ("part", &["id"], "time_updated"),
    ("todo", &["session_id", "position"], "time_updated"),
    ("session_share", &["session_id"], "time_updated"),
    ("session_input", &["id"], "time_created"),
];

type Times = HashMap<(&'static str, String), i64>;

fn times(tree: &Map<String, Value>) -> Times {
    let mut out = Times::new();
    for dump in tree.values() {
        for (table, key, time) in KEYS {
            for row in dump.get(table).and_then(Value::as_array).into_iter().flatten() {
                let k: Vec<String> = key.iter().map(|c| row.get(*c).map(Value::to_string).unwrap_or_default()).collect();
                out.insert((table, k.join("\0")), row.get(time).and_then(Value::as_i64).unwrap_or(0));
            }
        }
    }
    out
}

/// Every row of `small` is in `big`, at the same time or a later one.
fn covers(big: &Times, small: &Times) -> bool {
    small.iter().all(|(k, t)| big.get(k).is_some_and(|b| b >= t))
}

/// The copy here compared with the hub's, row by row: behind when every
/// row here is in the hub's copy at the same or a later time — the rows
/// analogue of the Claude transcript being a prefix.
fn compare(local: &Map<String, Value>, hub: &Map<String, Value>) -> InstallOutcome {
    let (local, hub) = (times(local), times(hub));
    match (covers(&hub, &local), covers(&local, &hub)) {
        (true, true) => InstallOutcome::InSync,
        (true, false) => InstallOutcome::Replaced,
        (false, true) => InstallOutcome::Ahead,
        (false, false) => InstallOutcome::Diverged,
    }
}

/// Where this machine files the session: its project, directory, path and
/// workspace, as the session row here says.
struct Place {
    project_id: Value,
    directory: String,
    path: Value,
    workspace_id: Value,
}

fn place_of(conn: &Connection, id: &str) -> Option<Place> {
    conn.query_row(
        "SELECT project_id, directory, path, workspace_id FROM session WHERE id = ?1",
        [id],
        |r| {
            Ok(Place {
                project_id: json!(r.get::<_, String>(0)?),
                directory: r.get(1)?,
                path: r.get::<_, Option<String>>(2)?.map_or(Value::Null, Value::String),
                workspace_id: r.get::<_, Option<String>>(3)?.map_or(Value::Null, Value::String),
            })
        },
    )
    .ok()
}

/// Have OpenCode itself file a bare session row under this machine's
/// project for `dir`: the one thing only OpenCode knows how to compute.
fn register(adapter: &OpenCodeAdapter, root: &Value, dir: &Path) -> Result<(), CoreError> {
    let text = |k: &str| root.get(k).and_then(Value::as_str).unwrap_or_default();
    let int = |k: &str| root.get(k).and_then(Value::as_i64).unwrap_or(0);
    let info = json!({
        "id": text("id"),
        "slug": text("slug"),
        "projectID": "",
        "directory": "",
        "title": text("title"),
        "version": text("version"),
        "time": { "created": int("time_created"), "updated": int("time_updated") },
    });
    let doc = crate::paths::tmp_dir()?.join(format!("{}.register.json", text("id")));
    std::fs::write(&doc, json!({ "info": info, "messages": [] }).to_string())
        .map_err(|e| CoreError::io(&doc, e))?;
    let output = std::process::Command::new("opencode")
        .arg("import")
        .arg(&doc)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output();
    let _ = std::fs::remove_file(&doc);
    let output = output.map_err(|e| CoreError::Invalid { msg: format!("failed to run opencode: {e}") })?;
    let conn = super::super::open_ro(adapter.db())?;
    if place_of(&conn, text("id")).is_none() {
        return Err(CoreError::Invalid {
            msg: format!(
                "opencode import did not file the session ({}): {}{}",
                output.status,
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(())
}

fn to_sql(value: &Value) -> Result<rusqlite::types::Value, CoreError> {
    use rusqlite::types::Value as Sql;
    Ok(match value {
        Value::Null => Sql::Null,
        Value::Bool(b) => Sql::Integer(*b as i64),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Sql::Integer(i),
            None => Sql::Real(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => Sql::Text(s.clone()),
        // dump_rows writes a BLOB as its length: nothing to put back.
        _ => return Err(CoreError::Invalid { msg: "the bundle holds a value asm cannot restore".into() }),
    })
}

fn insert(conn: &Connection, table: &str, row: &Map<String, Value>) -> Result<(), rusqlite::Error> {
    let columns: Vec<&String> = row.keys().collect();
    let names: Vec<String> = columns.iter().map(|c| format!("\"{}\"", c.replace('"', ""))).collect();
    let marks = vec!["?"; columns.len()].join(", ");
    let values: Vec<rusqlite::types::Value> =
        columns.iter().map(|c| to_sql(&row[*c]).unwrap_or(rusqlite::types::Value::Null)).collect();
    conn.execute(
        &format!("INSERT INTO {table} ({}) VALUES ({marks})", names.join(", ")),
        rusqlite::params_from_iter(values),
    )
    .map(|_| ())
}

/// Replace this machine's rows for the tree with the hub's, filed at
/// `place`. All or nothing.
fn write_tree(
    conn: &mut Connection,
    local_ids: &[String],
    hub: &Map<String, Value>,
    place: &Place,
) -> Result<(), rusqlite::Error> {
    let tx = conn.transaction()?;
    let mut ids: Vec<&str> = local_ids.iter().map(String::as_str).collect();
    ids.extend(hub.keys().map(String::as_str));
    for id in &ids {
        for table in TABLES {
            if write::table_exists(&tx, table) {
                tx.execute(&format!("DELETE FROM {table} WHERE session_id = ?1"), [id])?;
            }
        }
        tx.execute("DELETE FROM session WHERE id = ?1", [id])?;
    }
    for dump in hub.values() {
        // Sessions first, then what hangs off them.
        for table in std::iter::once("session").chain(TABLES) {
            for row in dump.get(table).and_then(Value::as_array).into_iter().flatten() {
                let Some(row) = row.as_object() else { continue };
                let mut row = row.clone();
                if table == "session" {
                    // Subagent sessions run in their parent's directory, so
                    // the whole tree is filed where the root is.
                    row.insert("project_id".into(), place.project_id.clone());
                    row.insert("directory".into(), json!(place.directory));
                    row.insert("path".into(), place.path.clone());
                    row.insert("workspace_id".into(), place.workspace_id.clone());
                }
                insert(&tx, table, &row)?;
            }
        }
    }
    tx.commit()
}

/// Install a pulled OpenCode session here, under its original id, with
/// every descendant, todo, share and input it had.
pub(crate) fn install(
    adapter: &OpenCodeAdapter,
    manifest: &Manifest,
    blob: &dyn Fn(&str) -> Result<PathBuf, CoreError>,
    project_dir: Option<&Path>,
) -> Result<Installed, CoreError> {
    manifest.validate().map_err(|msg| CoreError::Invalid { msg })?;
    let id = &manifest.id;
    for sha in manifest.blob_shas() {
        blob(sha)?;
    }
    let rows_sha = manifest
        .files
        .iter()
        .find(|f| f.path == "rows.json")
        .and_then(|f| f.sha256.as_deref())
        .ok_or_else(|| CoreError::Invalid { msg: "bundle has no rows".into() })?;
    let rows_path = blob(rows_sha)?;
    let doc: Value = serde_json::from_slice(&std::fs::read(&rows_path).map_err(|e| CoreError::io(&rows_path, e))?)
        .map_err(|e| CoreError::Invalid { msg: format!("rows.json is unreadable: {e}") })?;
    let hub = doc.get("sessions").and_then(Value::as_object).cloned().unwrap_or_default();
    let root_row = hub
        .get(id)
        .and_then(|d| d.get("session"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .ok_or_else(|| CoreError::Invalid { msg: format!("rows.json has no session {id}") })?;
    for (sid, dump) in &hub {
        if !crate::hub::manifest::valid_id(sid) {
            return Err(CoreError::Invalid { msg: format!("rows.json names a bad session id {sid:?}") });
        }
        for rows in dump.as_object().into_iter().flat_map(|d| d.values()) {
            for row in rows.as_array().into_iter().flatten() {
                for value in row.as_object().into_iter().flat_map(|r| r.values()) {
                    to_sql(value)?;
                }
            }
        }
    }
    write::guard_not_busy(adapter)?;

    let (here, local_ids, local) = if adapter.db().is_file() {
        let conn = super::super::open_ro(adapter.db())?;
        let here = place_of(&conn, id);
        let ids = if here.is_some() { write::with_descendants(&conn, id) } else { Vec::new() };
        (here, ids, dump_tree(&conn, id))
    } else {
        (None, Vec::new(), Map::new())
    };

    let (outcome, place) = match here {
        Some(place) => {
            if let Some(dir) = project_dir {
                let dir = dir.canonicalize().map_err(|e| CoreError::io(dir, e))?;
                if dir != Path::new(&place.directory) {
                    return Err(CoreError::Invalid {
                        msg: format!(
                            "session {id} is already here, in {}; pull without --project-dir to \
                             update it there",
                            place.directory
                        ),
                    });
                }
            }
            (compare(&local, &hub), place)
        }
        None => {
            let target = bundle::target_dir(manifest, project_dir)?;
            register(adapter, &root_row, &target)?;
            let conn = super::super::open_ro(adapter.db())?;
            let place = place_of(&conn, id).expect("register checked it");
            (InstallOutcome::New, place)
        }
    };
    let project_root = PathBuf::from(&place.directory);
    let installed = |outcome| Ok(Installed { outcome, project_root: project_root.clone(), path: adapter.db().to_path_buf() });
    if !matches!(outcome, InstallOutcome::New | InstallOutcome::Replaced) {
        return installed(outcome);
    }

    let mut conn = write::open_rw(adapter)?;
    if outcome == InstallOutcome::Replaced {
        write::backup_session_rows(adapter, &conn, id, &local_ids)?;
    }
    if let Err(e) = write_tree(&mut conn, &local_ids, &hub, &place) {
        if outcome == InstallOutcome::New {
            // The bare row `register` filed: nothing else is left behind.
            let _ = conn.execute("DELETE FROM session WHERE id = ?1", [id]);
        }
        return Err(CoreError::Invalid {
            msg: format!(
                "the rows did not fit this machine's OpenCode store ({e}); run the same OpenCode \
                 version on both machines. Nothing was changed"
            ),
        });
    }
    for file in &manifest.files {
        if let (Some(sid), Some(sha)) = (
            file.path.strip_prefix("session_diff/").and_then(|n| n.strip_suffix(".json")),
            &file.sha256,
        ) && hub.contains_key(sid)
            && let Some(dest) = write::session_diff_path(adapter, sid)
        {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| CoreError::io(parent, e))?;
            }
            fsutil::copy_atomic(&blob(sha)?, &dest)?;
        }
    }
    installed(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "ses_root0000000000000000000";
    const CHILD: &str = "ses_child000000000000000000";

    /// A store with OpenCode 1.18.31's real schema, and one project.
    fn store(dir: &Path, project: &str, worktree: &str) -> Connection {
        let conn = Connection::open(dir.join(format!("{project}.db"))).unwrap();
        conn.execute_batch(include_str!("../../../../../demo/schema.sql")).unwrap();
        conn.execute(
            "INSERT INTO project (id, worktree, time_created, time_updated, sandboxes) VALUES (?1, ?2, 1, 1, '[]')",
            [project, worktree],
        )
        .unwrap();
        conn
    }

    fn session(conn: &Connection, id: &str, parent: Option<&str>, project: &str, dir: &str, updated: i64) {
        conn.execute(
            "INSERT INTO session (id, project_id, parent_id, slug, directory, path, title, version, \
             model, time_created, time_updated) VALUES (?1, ?2, ?3, 'calm-otter', ?4, '', 'Retry schema', \
             '1.18.31', '{\"id\":\"m\",\"providerID\":\"p\"}', 1, ?5)",
            rusqlite::params![id, project, parent, dir, updated],
        )
        .unwrap();
    }

    fn message(conn: &Connection, session: &str, id: &str, updated: i64) {
        conn.execute(
            "INSERT INTO message VALUES (?1, ?2, 1, ?3, '{\"role\":\"user\"}')",
            rusqlite::params![id, session, updated],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES (?1, ?2, ?3, 1, ?4, '{\"type\":\"text\",\"text\":\"hi\"}')",
            rusqlite::params![format!("prt_{id}"), id, session, updated],
        )
        .unwrap();
    }

    /// Machine A: a root with a subagent child, a todo and an input.
    fn machine_a(dir: &Path) -> Connection {
        let a = store(dir, "proj_a", "/home/a/billing");
        session(&a, ROOT, None, "proj_a", "/home/a/billing", 50);
        session(&a, CHILD, Some(ROOT), "proj_a", "/home/a/billing", 40);
        message(&a, ROOT, "msg_1", 10);
        message(&a, ROOT, "msg_2", 20);
        message(&a, CHILD, "msg_3", 30);
        a.execute("INSERT INTO todo VALUES (?1, 'write it', 'pending', 'high', 0, 1, 5)", [ROOT]).unwrap();
        a.execute("INSERT INTO session_input VALUES ('inp_1', ?1, 'go', 'queued', 1, NULL, 7)", [ROOT]).unwrap();
        a
    }

    #[test]
    fn an_older_copy_is_replaced_and_ends_up_identical() {
        let dir = tempfile::tempdir().unwrap();
        let a = machine_a(dir.path());
        let hub = dump_tree(&a, ROOT);

        // B pulled earlier: only the first message, and at an older time.
        let mut b = store(dir.path(), "proj_b", "/home/b/src/billing");
        session(&b, ROOT, None, "proj_b", "/home/b/src/billing", 10);
        message(&b, ROOT, "msg_1", 5);
        assert_eq!(compare(&dump_tree(&b, ROOT), &hub), InstallOutcome::Replaced);

        let place = place_of(&b, ROOT).unwrap();
        let local = write::with_descendants(&b, ROOT);
        write_tree(&mut b, &local, &hub, &place).unwrap();

        assert_eq!(compare(&dump_tree(&b, ROOT), &hub), InstallOutcome::InSync);
        assert_eq!(watermark(&b, ROOT), watermark(&a, ROOT), "asm sees the same session on both");
        for id in [ROOT, CHILD] {
            let here = place_of(&b, id).unwrap();
            assert_eq!(here.directory, "/home/b/src/billing", "{id} filed at B's path");
            assert_eq!(here.project_id, json!("proj_b"));
        }
        let todos: i64 = b.query_row("SELECT COUNT(*) FROM todo", [], |r| r.get(0)).unwrap();
        let inputs: i64 = b.query_row("SELECT COUNT(*) FROM session_input", [], |r| r.get(0)).unwrap();
        assert_eq!((todos, inputs), (1, 1), "what `opencode import` would drop arrives too");
    }

    #[test]
    fn a_copy_changed_here_is_ahead_or_diverged_never_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let a = machine_a(dir.path());
        let hub = dump_tree(&a, ROOT);

        let b = store(dir.path(), "proj_b", "/home/b/billing");
        for (id, parent, t) in [(ROOT, None, 60), (CHILD, Some(ROOT), 40)] {
            session(&b, id, parent, "proj_b", "/home/b/billing", t);
        }
        for (s, m, t) in [(ROOT, "msg_1", 10), (ROOT, "msg_2", 20), (CHILD, "msg_3", 30), (ROOT, "msg_9", 60)] {
            message(&b, s, m, t);
        }
        b.execute("INSERT INTO todo VALUES (?1, 'write it', 'pending', 'high', 0, 1, 5)", [ROOT]).unwrap();
        b.execute("INSERT INTO session_input VALUES ('inp_1', ?1, 'go', 'queued', 1, NULL, 7)", [ROOT]).unwrap();
        assert_eq!(compare(&dump_tree(&b, ROOT), &hub), InstallOutcome::Ahead);

        // And A moved on too: neither contains the other.
        message(&a, ROOT, "msg_4", 70);
        assert_eq!(compare(&dump_tree(&b, ROOT), &dump_tree(&a, ROOT)), InstallOutcome::Diverged);
    }

    #[test]
    fn rows_that_do_not_fit_change_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let a = machine_a(dir.path());
        let mut hub = dump_tree(&a, ROOT);
        // A newer OpenCode on A added a column B's store does not have.
        hub[CHILD]["message"][0].as_object_mut().unwrap().insert("time_edited".into(), json!(1));

        let mut b = store(dir.path(), "proj_b", "/home/b/billing");
        session(&b, ROOT, None, "proj_b", "/home/b/billing", 10);
        message(&b, ROOT, "msg_1", 5);
        let before = dump_tree(&b, ROOT);
        let place = place_of(&b, ROOT).unwrap();
        let local = write::with_descendants(&b, ROOT);
        assert!(write_tree(&mut b, &local, &hub, &place).is_err());
        assert_eq!(dump_tree(&b, ROOT), before, "the transaction left B as it was");
    }
}
