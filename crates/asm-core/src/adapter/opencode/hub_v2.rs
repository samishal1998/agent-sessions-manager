//! OpenCode 2.x over the hub.
//!
//! The bundle is ONE file, `transfer.json`: `{generation: "opencode-v2",
//! root, sessions: {<id>: {info, messages}}}`, a session and every
//! descendant, each in the document `opencode session export` prints. It is
//! built from the database with the read-only exporter (`v2`), never by
//! running `opencode`, so a push works on a live store. The manifest's
//! `extra.generation` says the same, and an install checks it before it does
//! anything, because the two generations' bundles are not interchangeable:
//! a 1.x bundle is database rows, this one is documents.
//!
//! What a pull does about it. `opencode session import` is the only way in,
//! and it only creates: an id that exists is answered with success and left
//! alone. So a pull brings in a session that is not here, and brings an
//! older copy up to date by REPLACING it: back the local tree up as export
//! documents, delete it, import the hub's. There is no fast-forward on 2.x.
//! A copy that differs from the hub's only in titles is not replaced: the
//! titles are applied with a rename (the old ones saved first), which is all
//! that changed. The import files the whole tree under one directory (a
//! pulled session is one project here), renumbers each session's messages
//! 1..n, stamps `time.updated` with the import time, regenerates the slug
//! and recomputes the project id; none of that is part of the session's
//! identity, so the hub's canonical form (`v2::canonical_of`) leaves it out.
//!
//! Machine-local state that does not travel, because the export format has
//! no place for it: a session's claim and resume counter (`time_suspended`,
//! `resume_attempts`), queued inbox and pending work, and, because an import
//! drops them, the fork link, the revert state, the workspace and the
//! subpath. `time.viewed` is clamped to `time.idle` by the import.
//!
//! Trust: a pulled session carries its `permissions`, `metadata`, agent and
//! model exactly as pushed. The tree is checked to be what it says (bounded,
//! ids well formed, nothing outside it), not what it contains.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::OpenCodeAdapter;
use super::{v2, write_v2};
use crate::hub::bundle::{self, Base, Bundle, InstallOutcome, Installed, Staged};
use crate::hub::manifest::{Manifest, valid_id};
use crate::model::Session;
use crate::CoreError;

pub(crate) use v2::GENERATION;

/// The bundle's one file.
pub(crate) const FILE: &str = "transfer.json";

/// What a bundle may hold. Real trees are a session and a few subagents; these
/// bound what a hostile or corrupt bundle can make a pull hold in memory and
/// feed to `opencode`. 200 000 messages is hundreds of long sessions' worth.
pub(crate) const MAX_SESSIONS: usize = 1000;
pub(crate) const MAX_MESSAGES: usize = 200_000;

fn invalid(msg: impl Into<String>) -> CoreError {
    CoreError::Invalid { msg: msg.into() }
}

/// The refusal when a bundle of one generation meets a machine of the other:
/// `pushed` is the bundle's OpenCode version when it says, `installed` this
/// machine's `opencode --version`, `absent` that there is no store here yet.
pub(crate) fn wrong_generation(pushed: Option<&str>, bundle_v2: bool, machine_v2: bool, installed: Option<&str>, absent: bool) -> CoreError {
    let major = |v2: bool| if v2 { "2.x" } else { "1.x" };
    let pushed = match pushed {
        Some(v) => format!("OpenCode {} ({v})", major(bundle_v2)),
        None => format!("OpenCode {}", major(bundle_v2)),
    };
    if absent && installed.is_none() {
        return invalid(format!(
            "OpenCode is not installed here, and this session was pushed from {pushed}; install OpenCode {} first. \
             Nothing was changed",
            major(bundle_v2)
        ));
    }
    let here = match installed {
        Some(v) => format!("OpenCode {} ({v})", major(machine_v2)),
        None => format!("OpenCode {}", major(machine_v2)),
    };
    invalid(format!(
        "this session was pushed from {pushed}; this machine has {here}. Pull it on a machine with the same major \
         version. Nothing was changed"
    ))
}

pub(crate) fn collect(adapter: &OpenCodeAdapter, session: &Session) -> Result<Bundle, CoreError> {
    let sql = |e: rusqlite::Error| CoreError::Sqlite { db: adapter.db().to_path_buf(), source: Box::new(e) };
    let conn = super::super::open_ro(adapter.db())?;
    let root = &session.handle.native_id;
    let tree = v2::transfer_tree(&conn, root, true).map_err(sql)?;
    if tree.is_empty() {
        return Err(invalid(format!("session {root} not found in store")));
    }
    let canonical = v2::canonical_of(&tree);
    let sessions: Map<String, Value> = tree
        .into_iter()
        .map(|t| (t.pointer("/info/id").and_then(Value::as_str).unwrap_or_default().to_string(), t))
        .collect();
    let body = serde_json::to_vec(&json!({ "generation": GENERATION, "root": root, "sessions": sessions })).unwrap();
    Ok(Bundle { files: vec![Staged::bytes(FILE, body)], canonical, extra: json!({ "generation": GENERATION }) })
}

/// The bundle's sessions, parents first, checked: bounded, every id valid
/// and a session id, each document's own id its key, every session part of
/// the tree rooted at `root`, the root itself nobody's child (an import would
/// hang the tree under whatever local session had that id), every message an
/// object with a `msg_` id. The tree is what an install may delete and
/// import, so nothing in it can name another.
fn parse_tree(doc: &Value, root: &str) -> Result<Vec<Value>, CoreError> {
    let bad = |msg: String| Err(invalid(format!("{FILE} {msg}; nothing was changed")));
    if doc.get("generation").and_then(Value::as_str) != Some(GENERATION) {
        return bad("is not an OpenCode 2.x bundle".into());
    }
    let sessions = doc.get("sessions").and_then(Value::as_object).cloned().unwrap_or_default();
    if sessions.len() > MAX_SESSIONS {
        return bad(format!("holds {} sessions (at most {MAX_SESSIONS})", sessions.len()));
    }
    let total: usize = sessions.values().filter_map(|t| t.get("messages").and_then(Value::as_array)).map(Vec::len).sum();
    if total > MAX_MESSAGES {
        return bad(format!("holds {total} messages (at most {MAX_MESSAGES})"));
    }
    let parent_of = |t: &Value| t.pointer("/info/parentID").and_then(Value::as_str).map(str::to_string);
    for (sid, t) in &sessions {
        if !valid_id(sid) || !sid.starts_with("ses") || t.pointer("/info/id").and_then(Value::as_str) != Some(sid) {
            return bad(format!("names a bad session {sid:?}"));
        }
        let mut at = sid.clone();
        for _ in 0..=sessions.len() {
            if at == root {
                break;
            }
            match sessions.get(&at).and_then(parent_of) {
                Some(parent) => at = parent,
                None => return bad(format!("holds {sid}, which is not part of {root}")),
            }
        }
        if at != root {
            return bad(format!("holds {sid}, which is not part of {root}"));
        }
        let messages = t.get("messages").and_then(Value::as_array);
        let sound = messages.is_some_and(|ms| {
            ms.iter().all(|m| {
                m.get("id").and_then(Value::as_str).is_some_and(|id| id.starts_with("msg_") && valid_id(id))
                    && m.get("type").and_then(Value::as_str).is_some()
            })
        });
        if !sound {
            return bad(format!("has messages of {sid} that are not messages"));
        }
    }
    match sessions.get(root) {
        None => return bad(format!("has no session {root}")),
        Some(r) if parent_of(r).is_some() => {
            return bad(format!("has {root} as a subagent of another session, not as a session of its own"));
        }
        Some(_) => {}
    }
    // Parents before children: what an import needs.
    let mut ordered = vec![root.to_string()];
    let mut next = 0;
    while next < ordered.len() {
        let at = ordered[next].clone();
        let mut kids: Vec<&String> = sessions.iter().filter(|(_, t)| parent_of(t).as_deref() == Some(at.as_str())).map(|(k, _)| k).collect();
        kids.sort();
        ordered.extend(kids.into_iter().cloned());
        next += 1;
    }
    Ok(ordered.iter().map(|id| sessions[id].clone()).collect())
}

fn id_of(t: &Value) -> &str {
    t.pointer("/info/id").and_then(Value::as_str).unwrap_or_default()
}

/// Everything about a tree that decides whether one copy contains another,
/// keyed: each session's own fields and each message, by hash. A session's
/// cost, tokens, agent and model are NOT among them: they follow the
/// messages (a plain continuation moves them), and counting them would make
/// a copy that is merely behind look diverged. `titles` says whether titles
/// count; they are what a rename changes.
fn items(tree: &[Value], titles: bool) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for t in tree {
        let id = id_of(t);
        let mut info = Map::new();
        let keys: &[&str] = if titles { &["id", "parentID", "title", "metadata", "permissions"] } else { &["id", "parentID", "metadata", "permissions"] };
        for key in keys {
            if let Some(v) = t.pointer(&format!("/info/{key}")) {
                info.insert((*key).into(), v.clone());
            }
        }
        out.insert(format!("session:{id}"), v2::digest(&Value::Object(info)));
        for m in t.get("messages").and_then(Value::as_array).into_iter().flatten() {
            out.insert(format!("message:{id}:{}", m["id"].as_str().unwrap_or_default()), v2::digest(m));
        }
    }
    out
}

fn covers(big: &HashMap<String, String>, small: &HashMap<String, String>) -> bool {
    small.iter().all(|(k, v)| big.get(k) == Some(v))
}

/// The copy here compared with the hub's, by what each holds. Messages
/// that are on both sides and equal make a copy "behind"; a title or other
/// session field that differs belongs to neither and reads as diverged
/// unless the last sync (`Base`) says which side moved.
fn compare(local: &[Value], hub: &[Value]) -> InstallOutcome {
    let (local, hub) = (items(local, true), items(hub, true));
    match (covers(&hub, &local), covers(&local, &hub)) {
        (true, true) => InstallOutcome::InSync,
        (true, false) => InstallOutcome::Replaced,
        (false, true) => InstallOutcome::Ahead,
        (false, false) => InstallOutcome::Diverged,
    }
}

/// The titles to change (session id, the hub's title) when the two copies
/// are the same in everything but titles; `None` when anything else differs
/// (or a title cannot be set: OpenCode generates one for an empty title).
fn title_changes(local: &[Value], hub: &[Value]) -> Option<Vec<(String, String)>> {
    let (l, h) = (items(local, false), items(hub, false));
    if !(covers(&h, &l) && covers(&l, &h)) {
        return None;
    }
    let mut changes = Vec::new();
    for t in hub {
        let theirs = t.pointer("/info/title").and_then(Value::as_str);
        let mine = local.iter().find(|m| id_of(m) == id_of(t)).and_then(|m| m.pointer("/info/title")).and_then(Value::as_str);
        if theirs != mine {
            changes.push((id_of(t).to_string(), theirs.filter(|t| !t.trim().is_empty())?.to_string()));
        }
    }
    (!changes.is_empty()).then_some(changes)
}

/// Import `tree` (parents first) into `dir`: each id must be absent, and is
/// checked present, with its messages, afterwards. On any failure what this
/// call imported is deleted again; whether anything was is read from the
/// store, not from what the CLI said (a timeout after the server committed
/// the root leaves it behind all the same).
pub(super) fn import_tree(adapter: &OpenCodeAdapter, tree: &[Value], dir: &Path) -> Result<(), CoreError> {
    let cli = adapter.cli();
    let scratch = cli.scratch()?;
    let conn_ro = || super::super::open_ro(adapter.db());
    let sql = |e: rusqlite::Error| CoreError::Sqlite { db: adapter.db().to_path_buf(), source: Box::new(e) };
    let root = tree.first().map(id_of).unwrap_or_default().to_string();
    // `import` answers an id that exists with success and leaves it alone;
    // look first, before anything is ours to clean up.
    if adapter.db().is_file() {
        let conn = conn_ro()?;
        for t in tree {
            if v2::info_of(&conn, id_of(t)).map_err(sql)?.is_some() {
                return Err(invalid(format!("session {} is here already", id_of(t))));
            }
        }
    }
    let result = (|| {
        for t in tree {
            let id = id_of(t);
            let file = scratch.join(format!("{id}.import.json"));
            std::fs::write(&file, serde_json::to_vec(t).unwrap()).map_err(|e| CoreError::io(&file, e))?;
            let ran = cli.import(&file, dir);
            let _ = std::fs::remove_file(&file);
            ran?;
            let conn = conn_ro()?;
            let stored = conn
                .query_row("SELECT COUNT(*) FROM session_message WHERE session_id = ?1", [id], |r| r.get::<_, i64>(0))
                .map_err(sql)?;
            // An import leaves unsettled messages out (a backup holds them).
            let wanted = v2::settled(std::slice::from_ref(t))[0].get("messages").and_then(Value::as_array).map_or(0, Vec::len) as i64;
            if v2::info_of(&conn, id).map_err(sql)?.is_none() || stored != wanted {
                return Err(invalid(format!("opencode did not import session {id} completely ({stored} of {wanted} messages)")));
            }
        }
        Ok(())
    })();
    if result.is_err() && conn_ro().ok().and_then(|c| v2::info_of(&c, &root).ok().flatten()).is_some() {
        // Deleting the root takes everything imported below it.
        let _ = cli.delete(&root);
    }
    result
}

/// Install a pulled 2.x session here under its original ids.
pub(crate) fn install(
    adapter: &OpenCodeAdapter,
    manifest: &Manifest,
    blob: &dyn Fn(&str) -> Result<PathBuf, CoreError>,
    project_dir: Option<&Path>,
    base: Option<&Base>,
) -> Result<Installed, CoreError> {
    manifest.validate().map_err(invalid)?;
    let id = &manifest.id;
    for sha in manifest.blob_shas() {
        blob(sha)?;
    }
    let sha = manifest
        .files
        .iter()
        .find(|f| f.path == FILE)
        .and_then(|f| f.sha256.as_deref())
        .ok_or_else(|| invalid("bundle has no sessions"))?;
    let path = blob(sha)?;
    let doc: Value = serde_json::from_slice(&std::fs::read(&path).map_err(|e| CoreError::io(&path, e))?)
        .map_err(|e| invalid(format!("{FILE} is unreadable: {e}")))?;
    let hub = parse_tree(&doc, id)?;
    let sql = |e: rusqlite::Error| CoreError::Sqlite { db: adapter.db().to_path_buf(), source: Box::new(e) };
    let conn = adapter.db().is_file().then(|| super::super::open_ro(adapter.db())).transpose()?;
    let local_root = match &conn {
        Some(conn) => v2::info_of(conn, id).map_err(sql)?,
        None => None,
    };
    let installed = |outcome, dir: PathBuf| Ok(Installed { outcome, project_root: dir, path: adapter.db().to_path_buf() });

    let Some(local_root) = local_root else {
        // Not here. None of the bundle's ids may be either (a subagent pulled
        // on its own earlier would be left alone by the import, and then
        // mistaken for ours).
        if let Some(conn) = &conn {
            for t in &hub {
                if v2::info_of(conn, id_of(t)).map_err(sql)?.is_some() {
                    return Err(invalid(format!(
                        "session {} is here already, and not as part of {id}; nothing was changed",
                        id_of(t)
                    )));
                }
            }
        }
        let target = bundle::target_dir(manifest, project_dir)?;
        import_tree(adapter, &hub, &target)?;
        return installed(InstallOutcome::New, target);
    };

    let conn = conn.expect("a session was read from it");
    let place = PathBuf::from(local_root.pointer("/location/directory").and_then(Value::as_str).unwrap_or_default());
    if let Some(dir) = project_dir {
        let dir = dir.canonicalize().map_err(|e| CoreError::io(dir, e))?;
        if dir != place {
            return Err(invalid(format!(
                "session {id} is already here, in {}; pull without --project-dir to update it there",
                place.display()
            )));
        }
    }
    let local_ids = v2::tree_ids(&conn, id).map_err(sql)?;
    for t in &hub {
        if !local_ids.iter().any(|l| l == id_of(t)) && v2::info_of(&conn, id_of(t)).map_err(sql)?.is_some() {
            return Err(invalid(format!(
                "session {} is here already, and not as part of {id}; nothing was changed",
                id_of(t)
            )));
        }
    }
    // Everything stored (a backup keeps it all) and the settled part of it
    // (what an import brings back, and so what is compared).
    let signature = v2::signature(&conn, &local_ids).map_err(sql)?;
    let everything = v2::transfer_tree(&conn, id, false).map_err(sql)?;
    let local = v2::settled(&everything);
    let local_canonical = v2::canonical_of(&local);
    let outcome = bundle::with_base(compare(&local, &hub), &local_canonical, &manifest.canonical, base);
    if outcome != InstallOutcome::Replaced {
        return installed(outcome, place);
    }

    // Behind. Everything that can refuse does so before the first thing is
    // changed.
    write_v2::refuse_busy(adapter, &conn, &local_ids)?;

    // Only titles differ: apply them, nothing else moved.
    if let Some(changes) = title_changes(&local, &hub) {
        let backup = write_v2::backup_infos(adapter, id, &local)?;
        for (sid, title) in &changes {
            adapter.cli().update_title(sid, title)?;
            let now = v2::info_of(&conn, sid).map_err(sql)?;
            if now.as_ref().and_then(|i| i.get("title")).and_then(Value::as_str) != Some(title) {
                return Err(invalid(format!(
                    "opencode accepted the rename but the title of {sid} did not change; the old titles are in {}",
                    backup.display()
                )));
            }
        }
        return installed(InstallOutcome::Renamed, place);
    }

    if !place.is_dir() {
        return Err(invalid(format!(
            "{} does not exist on this machine, so the session cannot be put back there; create it or pull \
             with --project-dir. Nothing was changed",
            place.display()
        )));
    }
    let backup = write_v2::backup_tree(adapter, id, &everything)?;
    write_v2::recheck(adapter, &conn, id, &local_ids, &signature, &backup)?;
    adapter
        .cli()
        .delete(id)
        .map_err(|e| invalid(format!("{e}. The previous copy is in {}. {}", backup.display(), write_v2::restore_hint(&backup))))?;
    let mut gone = local_ids.clone();
    gone.extend(hub.iter().map(|t| id_of(t).to_string()));
    gone.dedup();
    write_v2::verify_gone(adapter, &conn, &gone, &backup)?;
    if let Err(e) = import_tree(adapter, &hub, &place) {
        // Put back what was here.
        let restored = import_tree(adapter, &everything, &place);
        return Err(invalid(format!(
            "could not install the hub's copy of {id}: {e}. {}; the previous copy is also in {}. {}",
            if restored.is_ok() { "The previous copy was restored" } else { "Restoring the previous copy failed too" },
            backup.display(),
            write_v2::restore_hint(&backup)
        )));
    }
    installed(InstallOutcome::Replaced, place)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, parent: Option<&str>, title: &str, messages: &[&str]) -> Value {
        let mut info = json!({ "id": id, "projectID": "p", "title": title, "cost": 0, "location": { "directory": "/a" },
                               "tokens": { "input": 0, "output": 0, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
                               "time": { "created": 1, "updated": 2 } });
        if let Some(parent) = parent {
            info["parentID"] = json!(parent);
        }
        let messages: Vec<Value> = messages.iter().map(|m| json!({ "id": m, "type": "user", "text": m, "time": { "created": 1 } })).collect();
        json!({ "info": info, "messages": messages })
    }

    fn doc(root: &str, sessions: &[Value]) -> Value {
        let map: Map<String, Value> = sessions.iter().map(|s| (id_of(s).to_string(), s.clone())).collect();
        json!({ "generation": GENERATION, "root": root, "sessions": map })
    }

    #[test]
    fn a_tree_is_read_parents_first_and_anything_outside_it_is_refused() {
        let root = session("ses_r", None, "T", &["msg_1"]);
        let kid = session("ses_k", Some("ses_r"), "K", &["msg_2"]);
        let grandkid = session("ses_g", Some("ses_k"), "G", &[]);
        let ordered = parse_tree(&doc("ses_r", &[grandkid.clone(), kid.clone(), root.clone()]), "ses_r").unwrap();
        assert_eq!(ordered.iter().map(id_of).collect::<Vec<_>>(), ["ses_r", "ses_k", "ses_g"]);

        let stray = session("ses_x", None, "X", &[]);
        let orphan = session("ses_o", Some("ses_missing"), "O", &[]);
        let mut renamed = root.clone();
        renamed["info"]["id"] = json!("ses_other");
        let mut bad_id = root.clone();
        bad_id["messages"][0]["id"] = json!("../../etc");
        let mut not_a_message = root.clone();
        not_a_message["messages"] = json!(["text"]);
        // The root hanging from a session this machine may have: an import
        // would file the whole tree under it.
        let mut hangs = root.clone();
        hangs["info"]["parentID"] = json!("ses_local_victim");
        let mut cycle = root.clone();
        cycle["info"]["parentID"] = json!("ses_k");
        let not_a_session = session("msg_r", None, "T", &[]);
        let cases = [
            ("a second root", doc("ses_r", &[root.clone(), stray])),
            ("an orphan", doc("ses_r", &[root.clone(), orphan])),
            ("an id that is not its key", json!({ "generation": GENERATION, "sessions": { "ses_r": renamed } })),
            ("a bad message id", doc("ses_r", &[bad_id])),
            ("a message that is not one", doc("ses_r", &[not_a_message])),
            ("no root", doc("ses_r", std::slice::from_ref(&kid))),
            ("another generation", json!({ "sessions": { "ses_r": root.clone() } })),
            ("a root that hangs from another session", doc("ses_r", &[hangs])),
            ("a root that is its own descendant", doc("ses_r", &[cycle, kid])),
            ("an id that is not a session id", doc("msg_r", &[not_a_session])),
        ];
        for (what, bundle) in cases {
            assert!(parse_tree(&bundle, if what.starts_with("an id that is not a session") { "msg_r" } else { "ses_r" }).is_err(), "{what} was accepted");
        }
    }

    /// A hostile or corrupt bundle cannot make a pull hold or feed `opencode`
    /// an unbounded tree.
    #[test]
    fn a_bundle_is_bounded() {
        let root = session("ses_r", None, "T", &[]);
        let mut sessions = vec![root.clone()];
        for n in 0..MAX_SESSIONS {
            sessions.push(session(&format!("ses_k{n}"), Some("ses_r"), "K", &[]));
        }
        let err = parse_tree(&doc("ses_r", &sessions), "ses_r").unwrap_err().to_string();
        assert!(err.contains("sessions (at most 1000)"), "{err}");
        sessions.truncate(MAX_SESSIONS);
        assert!(parse_tree(&doc("ses_r", &sessions), "ses_r").is_ok(), "exactly the limit is fine");

        // Messages, in total over the tree.
        let ids: Vec<String> = (0..MAX_MESSAGES / 2 + 1).map(|n| format!("msg_{n}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let big = [session("ses_r", None, "T", &refs), session("ses_k", Some("ses_r"), "K", &refs)];
        let err = parse_tree(&doc("ses_r", &big), "ses_r").unwrap_err().to_string();
        assert!(err.contains("messages (at most 200000)"), "{err}");
    }

    #[test]
    fn copies_are_compared_by_what_they_hold() {
        let hub = [session("ses_r", None, "T", &["msg_1", "msg_2"])];
        let same = [session("ses_r", None, "T", &["msg_1", "msg_2"])];
        let behind = [session("ses_r", None, "T", &["msg_1"])];
        let ahead = [session("ses_r", None, "T", &["msg_1", "msg_2", "msg_3"])];
        let other = [session("ses_r", None, "T", &["msg_1", "msg_9"])];
        let renamed = [session("ses_r", None, "New title", &["msg_1", "msg_2"])];
        assert_eq!(compare(&same, &hub), InstallOutcome::InSync);
        assert_eq!(compare(&behind, &hub), InstallOutcome::Replaced);
        assert_eq!(compare(&ahead, &hub), InstallOutcome::Ahead);
        assert_eq!(compare(&other, &hub), InstallOutcome::Diverged);
        assert_eq!(compare(&renamed, &hub), InstallOutcome::Diverged, "a title alone is a change on one side");
        // A subagent the other copy lacks.
        let with_kid = [session("ses_r", None, "T", &["msg_1", "msg_2"]), session("ses_k", Some("ses_r"), "K", &[])];
        assert_eq!(compare(&hub, &with_kid), InstallOutcome::Replaced);
    }

    /// A plain continuation moves cost, tokens, agent and model with the
    /// messages. With no sync record that must still read as "behind", not as
    /// a divergence.
    #[test]
    fn a_continuation_without_a_sync_record_is_behind_not_diverged() {
        let behind = [session("ses_r", None, "T", &["msg_1"])];
        let mut hub = [session("ses_r", None, "T", &["msg_1", "msg_2"])];
        hub[0]["info"]["cost"] = json!(0.25);
        hub[0]["info"]["tokens"]["output"] = json!(500);
        hub[0]["info"]["agent"] = json!("plan");
        hub[0]["info"]["model"] = json!({ "id": "m2", "providerID": "p" });
        assert_eq!(compare(&behind, &hub), InstallOutcome::Replaced);
        assert_eq!(compare(&hub, &behind), InstallOutcome::Ahead);
        // ...while the metadata and permissions a session carries still count.
        let mut changed = hub.clone();
        changed[0]["info"]["permissions"] = json!([{ "p": 1 }]);
        assert_eq!(compare(&behind, &changed), InstallOutcome::Diverged);
    }

    /// Same messages, a different title: a rename, applied as one.
    #[test]
    fn a_title_only_difference_is_a_rename_not_a_replace() {
        let local = vec![session("ses_r", None, "T", &["msg_1"]), session("ses_k", Some("ses_r"), "K", &["msg_2"])];
        let mut hub = local.clone();
        assert!(title_changes(&local, &hub).is_none(), "nothing differs");
        hub[0]["info"]["title"] = json!("Renamed");
        hub[1]["info"]["title"] = json!("Kid renamed");
        let mut changes = title_changes(&local, &hub).unwrap();
        changes.sort();
        assert_eq!(changes, [("ses_k".to_string(), "Kid renamed".to_string()), ("ses_r".to_string(), "Renamed".to_string())]);
        // With a sync record saying the local copy is unchanged, that is the outcome the install acts on.
        let base = Base { canonical: v2::canonical_of(&local), files: Default::default() };
        let outcome = bundle::with_base(compare(&local, &hub), &v2::canonical_of(&local), &v2::canonical_of(&hub), Some(&base));
        assert_eq!(outcome, InstallOutcome::Replaced);
        // More than the title differs: a real replace.
        let mut more = hub.clone();
        more[0]["messages"].as_array_mut().unwrap().push(json!({ "id": "msg_9", "type": "user", "text": "x", "time": { "created": 1 } }));
        assert!(title_changes(&local, &more).is_none());
        let mut metadata = hub.clone();
        metadata[0]["info"]["metadata"] = json!({ "k": 1 });
        assert!(title_changes(&local, &metadata).is_none());
        // A title that cannot be set (OpenCode would generate one with a model).
        let mut untitled = local.clone();
        untitled[0]["info"].as_object_mut().unwrap().remove("title");
        assert!(title_changes(&local, &untitled).is_none());
    }

    /// The hub compares what the conversation is, not where it is filed or
    /// when OpenCode last touched it.
    #[test]
    fn the_canonical_form_ignores_what_an_import_rewrites() {
        let a = vec![session("ses_r", None, "T", &["msg_1"]), session("ses_k", Some("ses_r"), "K", &["msg_2"])];
        let mut b = vec![a[1].clone(), a[0].clone()];
        for t in &mut b {
            t["info"]["projectID"] = json!("another");
            t["info"]["location"] = json!({ "directory": "/elsewhere", "workspaceID": "wrk_1" });
            t["info"]["time"] = json!({ "created": 1, "updated": 99999, "viewed": 5, "idle": 7 });
            t["info"]["subpath"] = json!("sub");
        }
        assert_eq!(v2::canonical_of(&a), v2::canonical_of(&b), "also in any order");
        let mut renamed = a.clone();
        renamed[0]["info"]["title"] = json!("Renamed");
        assert_ne!(v2::canonical_of(&a), v2::canonical_of(&renamed));
        let mut changed = a.clone();
        changed[1]["messages"][0]["text"] = json!("different");
        assert_ne!(v2::canonical_of(&a), v2::canonical_of(&changed));
    }

    #[test]
    fn a_bundle_of_the_other_generation_is_refused_in_words() {
        // Nothing installed: say that, not that the machine has the wrong version.
        let none = wrong_generation(Some("2.0.25"), true, false, None, true).to_string();
        assert!(none.contains("OpenCode is not installed here") && none.contains("OpenCode 2.x (2.0.25)"), "{none}");
        // Installed, the other generation; the bundle's version is named.
        let msg = wrong_generation(Some("2.0.25"), true, false, Some("1.17.18"), false).to_string();
        assert!(msg.contains("pushed from OpenCode 2.x (2.0.25)") && msg.contains("this machine has OpenCode 1.x (1.17.18)"), "{msg}");
        assert!(msg.contains("same major version"), "{msg}");
        let msg = wrong_generation(None, false, true, Some("2.0.25"), false).to_string();
        assert!(msg.contains("pushed from OpenCode 1.x;") && msg.contains("this machine has OpenCode 2.x (2.0.25)"), "{msg}");
        // A store with no OpenCode at hand is not "not installed" when it has a store.
        assert!(!wrong_generation(None, true, false, None, false).to_string().contains("not installed"));
    }
}
