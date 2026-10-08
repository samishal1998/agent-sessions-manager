//! Tests that run the real OpenCode 2.x binary.
//!
//! Every one skips (returns early, with a message on stderr) unless an
//! `opencode` whose `--version` is 2.x is on the PATH, and every one runs it
//! only through `cli2::testenv::Isolated`: HOME and all four XDG directories
//! in a fresh temp dir, `--standalone`, an environment holding nothing else
//! but PATH. They never touch the real OpenCode data and never send a
//! prompt: sessions are made with `session import` of documents built here.

use std::path::Path;

use serde_json::{Value, json};

use super::cli2::testenv::Isolated;
use super::{OpenCodeAdapter, hub, v2};
use crate::adapter::{AgentRead, AgentWrite, SessionFilter};
use crate::hub::bundle::{Base, InstallOutcome, Source};
use crate::hub::manifest::Manifest;
use crate::import::{ImportMode, ImportOpts};
use crate::ir::{ExtBag, IrMessage, IrPart, IrProvenance, IrRole, IrSession, PortablePath};
use crate::model::{AgentKind, Session};

const ROOT: &str = "ses_fx_root_0000000001";
const CHILD: &str = "ses_fx_child_000000001a";
const GRANDCHILD: &str = "ses_fx_grandchild_00001b";

/// A session with a message of every kind the schema has.
fn root_doc(dir: &Path) -> Value {
    let d = dir.display().to_string();
    json!({
      "info": { "id": ROOT, "projectID": "x", "agent": "build", "model": {"id": "m", "providerID": "p"},
                "cost": 0.5, "tokens": {"input": 1, "output": 2, "reasoning": 3, "cache": {"read": 4, "write": 5}},
                "outcome": "succeeded",
                "time": {"created": 1760000000000_i64, "updated": 1760000009000_i64, "idle": 1760000009000_i64, "viewed": 1760000009500_i64},
                "title": "All kinds", "metadata": {"k": "v"}, "location": {"directory": d} },
      "messages": [
        {"id": "msg_a01", "type": "user", "time": {"created": 1760000000000_i64}, "text": "hello",
         "files": [{"data": "aGk=", "mime": "text/plain", "source": {"type": "uri", "uri": format!("file://{d}/x.txt")}, "name": "x.txt"}],
         "agents": [{"name": "general"}], "skills": [{"id": "sk1", "name": "sk", "text": "t"}], "metadata": {"m": 1}},
        {"id": "msg_a02", "type": "agent-switched", "time": {"created": 1760000000100_i64}, "agent": "plan"},
        {"id": "msg_a03", "type": "model-switched", "time": {"created": 1760000000200_i64}, "model": {"id": "m2", "providerID": "p"}},
        {"id": "msg_a04", "type": "location-switched", "time": {"created": 1760000000300_i64}, "location": {"directory": d}},
        {"id": "msg_a05", "type": "system", "time": {"created": 1760000000400_i64}, "text": "sys", "description": "d"},
        {"id": "msg_a06", "type": "synthetic", "time": {"created": 1760000000500_i64}, "text": "syn"},
        {"id": "msg_a07", "type": "skill", "time": {"created": 1760000000600_i64}, "skill": "sk1", "name": "sk", "text": "skill text"},
        {"id": "msg_a08", "type": "assistant", "time": {"created": 1760000001000_i64, "streamed": 1760000004000_i64, "completed": 1760000005000_i64},
         "agent": "build", "model": {"id": "m", "providerID": "p"},
         "content": [
           {"type": "reasoning", "text": "hm", "state": {"signature": "s"}, "time": {"created": 1, "completed": 2}},
           {"type": "text", "text": "ok"},
           {"type": "tool", "id": "c1", "name": "shell", "executed": true,
            "state": {"status": "completed", "input": {"command": "ls"},
                      "content": [{"type": "text", "text": "a"}, {"type": "file", "uri": "file:///x", "mime": "image/png", "name": "x"}],
                      "metadata": {"truncated": true}},
            "time": {"created": 3, "ran": 4, "completed": 5}},
           {"type": "tool", "id": "c2", "name": "read",
            "state": {"status": "error", "input": {"filePath": "/n"}, "error": {"type": "tool", "message": "ENOENT"},
                      "content": [{"type": "text", "text": "partial"}]},
            "time": {"created": 3}}],
         "finish": "tool-calls", "rawFinish": "tool_use", "cost": 0.1,
         "tokens": {"input": 1, "output": 2, "reasoning": 0, "cache": {"read": 0, "write": 0}}},
        {"id": "msg_a09", "type": "shell", "time": {"created": 1760000006000_i64, "completed": 1760000007000_i64},
         "shellID": "sh_abc", "command": "echo hi", "status": "exited", "exit": 0,
         "output": {"output": "hi\n", "cursor": 3, "size": 3, "truncated": false}},
        {"id": "msg_a10", "type": "compaction", "time": {"created": 1760000008000_i64}, "status": "completed",
         "reason": "auto", "summary": "sum", "recent": "msg_a09"},
        {"id": "msg_a11", "type": "idle", "time": {"created": 1760000009000_i64}, "outcome": "succeeded"}
      ]
    })
}

fn child_doc(id: &str, parent: &str, msg: &str, dir: &Path) -> Value {
    json!({
      "info": { "id": id, "parentID": parent, "projectID": "x", "cost": 0,
                "tokens": {"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                "time": {"created": 1760000006000_i64, "updated": 1760000007000_i64},
                "title": format!("Child {id}"), "location": {"directory": dir.display().to_string()} },
      "messages": [{"id": msg, "type": "user", "time": {"created": 1760000006000_i64}, "text": "child prompt"}]
    })
}

fn import(env: &Isolated, doc: &Value, dir: &Path) {
    let file = env.path("scratch").join("doc.json");
    std::fs::write(&file, doc.to_string()).unwrap();
    env.cli.import(&file, dir).unwrap();
}

/// Root, a child and a grandchild (in another directory) under `dir`.
fn seed(env: &Isolated, dir: &Path) {
    import(env, &root_doc(dir), dir);
    let other = env.path("proj/other");
    import(env, &child_doc(CHILD, ROOT, "msg_c01", dir), dir);
    import(env, &child_doc(GRANDCHILD, CHILD, "msg_g01", &other), &other);
}

fn conn(env: &Isolated) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(&env.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn tree(env: &Isolated, root: &str) -> Vec<Value> {
    v2::transfer_tree(&conn(env), root, true).unwrap()
}

fn session(adapter: &OpenCodeAdapter, id: &str) -> Session {
    adapter
        .sessions(&SessionFilter { include_children: true, ..SessionFilter::default() })
        .unwrap()
        .into_iter()
        .find(|s| s.handle.native_id == id)
        .unwrap_or_else(|| panic!("{id} not listed"))
}

/// OpenCode's own export and ours, side by side, are the same document.
#[test]
fn the_exporter_prints_what_opencode_session_export_prints() {
    let Some(env) = Isolated::new() else { return };
    let dir = env.path("proj/a");
    seed(&env, &dir);
    for id in [ROOT, CHILD, GRANDCHILD] {
        let theirs: Value = serde_json::from_str(&env.cli.export(id).unwrap()).unwrap();
        let ours = v2::transfer(&conn(&env), id, true).unwrap().unwrap();
        assert_eq!(ours, theirs, "{id}");
    }

    // And against what went in: the messages are untouched; `info` differs
    // only in what the import recomputes or normalises.
    let original = root_doc(&dir);
    let stored = v2::transfer(&conn(&env), ROOT, true).unwrap().unwrap();
    assert_eq!(stored["messages"], original["messages"], "message ids, order and content survive an import");
    let (a, b) = (&original["info"], &stored["info"]);
    for key in ["id", "agent", "cost", "tokens", "outcome", "title", "metadata", "location"] {
        assert_eq!(a[key], b[key], "{key} is kept");
    }
    assert_ne!(a["projectID"], b["projectID"], "recomputed from the directory");
    assert_eq!(b["model"]["variant"], "default", "filled in on read");
    assert_ne!(a["time"]["updated"], b["time"]["updated"], "stamped with the import time");
    assert_eq!(b["time"]["viewed"], 1760000009000_i64, "clamped to idle");
    assert_eq!(a["time"]["created"], b["time"]["created"]);
    // The child that went into another directory is filed there.
    assert_eq!(tree(&env, ROOT).len(), 3);
    assert_eq!(v2::transfer(&conn(&env), GRANDCHILD, true).unwrap().unwrap()["info"]["location"]["directory"], env.path("proj/other").display().to_string());
}

#[test]
fn rename_goes_through_opencode_and_changes_only_the_title() {
    let Some(env) = Isolated::new() else { return };
    let dir = env.path("proj/a");
    seed(&env, &dir);
    let adapter = env.adapter();
    let before = tree(&env, ROOT);
    adapter.rename(&session(&adapter, ROOT), "A new name: with \"quotes\"").unwrap();
    let after = tree(&env, ROOT);
    assert_eq!(after[0]["info"]["title"], "A new name: with \"quotes\"");
    assert_eq!(after[0]["messages"], before[0]["messages"]);
    assert_eq!(after[1..], before[1..], "the subagents are untouched");
    for key in ["cost", "tokens", "metadata", "agent", "model", "outcome"] {
        assert_eq!(after[0]["info"][key], before[0]["info"][key], "{key}");
    }
    // The old title is kept in the backup.
    let backups: Vec<_> = std::fs::read_dir(env.dir.path().join("backups").join(ROOT)).unwrap().collect();
    assert_eq!(backups.len(), 1);
    let kept: Value = serde_json::from_slice(&std::fs::read(backups[0].as_ref().unwrap().path().join("info.json")).unwrap()).unwrap();
    assert_eq!(kept["title"], "All kinds");

    // An empty title would have OpenCode generate one with a model.
    assert!(adapter.rename(&session(&adapter, ROOT), "").is_err());
    assert_eq!(tree(&env, ROOT)[0]["info"]["title"], "A new name: with \"quotes\"");
}

#[test]
fn delete_removes_the_tree_after_writing_a_backup_that_imports_back() {
    let Some(env) = Isolated::new() else { return };
    let dir = env.path("proj/a");
    seed(&env, &dir);
    let adapter = env.adapter();
    let before = tree(&env, ROOT);
    let report = adapter.delete(&session(&adapter, ROOT)).unwrap();
    let count = |sql: &str| conn(&env).query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(count("SELECT COUNT(*) FROM session_v2"), 0, "children go with their parent");
    assert_eq!(count("SELECT COUNT(*) FROM session_message"), 0);

    // The backup brings the whole tree back into another store: each session
    // as the manifest says, in file order, with plain `session import`.
    let backup = report.backup_dir.unwrap();
    assert!(report.note.unwrap().contains("opencode session import <file> --directory <directory>"));
    let mut files: Vec<_> = std::fs::read_dir(&backup).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    assert_eq!(files.len(), 4, "three sessions and the manifest");
    let manifest: Value = serde_json::from_slice(&std::fs::read(backup.join("manifest.json")).unwrap()).unwrap();
    let other = Isolated::new().unwrap();
    for session in manifest["sessions"].as_array().unwrap() {
        let directory = session["directory"].as_str().unwrap();
        // Another machine: the grandchild's folder is not there; make both.
        std::fs::create_dir_all(directory).unwrap();
        other.cli.import(&backup.join(session["file"].as_str().unwrap()), Path::new(directory)).unwrap();
    }
    let restored = tree(&other, ROOT);
    assert_eq!(v2::canonical_of(&restored), v2::canonical_of(&before));
    assert_eq!(restored.iter().map(|t| &t["messages"]).collect::<Vec<_>>(), before.iter().map(|t| &t["messages"]).collect::<Vec<_>>());
}

/// A session whose project directory is gone can still be deleted: the CLI
/// is not run from it.
#[test]
fn a_session_whose_directory_is_gone_can_still_be_deleted() {
    let Some(env) = Isolated::new() else { return };
    let dir = env.path("proj/gone");
    import(&env, &root_doc(&dir), &dir);
    std::fs::remove_dir_all(&dir).unwrap();
    let adapter = env.adapter();
    adapter.delete(&session(&adapter, ROOT)).unwrap();
    assert!(v2::info_of(&conn(&env), ROOT).unwrap().is_none());
}

fn manifest_and_blobs(
    adapter: &OpenCodeAdapter,
    id: &str,
    dir: &Path,
    blobs: &Path,
) -> (Manifest, String) {
    let bundle = hub::collect(adapter, &session(adapter, id)).unwrap();
    let mut files = Vec::new();
    for staged in &bundle.files {
        let Source::Bytes(bytes) = &staged.source else { panic!("a 2.x bundle is bytes") };
        std::fs::write(blobs.join(staged.entry.sha256.as_ref().unwrap()), bytes).unwrap();
        files.push(json!({ "path": staged.entry.path, "sha256": staged.entry.sha256, "size": staged.entry.size }));
    }
    let manifest: Manifest = serde_json::from_value(json!({
        "schema": 1, "agent": "opencode", "id": id, "project_root": dir, "project_root_portable": "/nonexistent-pusher/proj",
        "canonical": bundle.canonical, "parent_rev": null, "files": files, "extra": bundle.extra,
    }))
    .unwrap();
    (manifest, bundle.canonical)
}

fn pull(
    adapter: &OpenCodeAdapter,
    manifest: &Manifest,
    blobs: &Path,
    dir: Option<&Path>,
    base: Option<&Base>,
) -> Result<crate::hub::bundle::Installed, crate::CoreError> {
    hub::install(adapter, manifest, &|sha| Ok(blobs.join(sha)), dir, base)
}

#[test]
fn a_tree_pushed_from_one_machine_installs_on_another_and_agrees_with_it() {
    let (Some(a), Some(b)) = (Isolated::new(), Isolated::new()) else { return };
    let dir_a = a.path("proj/a");
    seed(&a, &dir_a);
    let (adapter_a, adapter_b) = (a.adapter(), b.adapter());
    let blobs = a.path("blobs");
    let (manifest, canonical) = manifest_and_blobs(&adapter_a, ROOT, &dir_a, &blobs);
    assert_eq!(manifest.extra["generation"], "opencode-v2");
    assert_eq!(canonical, v2::canonical_of(&tree(&a, ROOT)));

    // B has never run OpenCode in this folder; the pushing machine's path does not exist there.
    let dir_b = b.path("proj/b");
    assert!(pull(&adapter_b, &manifest, &blobs, None, None).is_err(), "the pusher's folder is not on this machine");
    assert!(!b.db.is_file(), "refused before anything was written");
    let installed = pull(&adapter_b, &manifest, &blobs, Some(&dir_b), None).unwrap();
    assert_eq!(installed.outcome, InstallOutcome::New);
    let on_b = tree(&b, ROOT);
    assert_eq!(on_b.len(), 3, "the subagents came too");
    assert_eq!(v2::canonical_of(&on_b), canonical, "both machines agree on what the session is");
    for t in &on_b {
        assert_eq!(t["info"]["location"]["directory"], dir_b.display().to_string(), "all filed under B's folder");
    }
    assert_ne!(on_b[0]["info"]["projectID"], tree(&a, ROOT)[0]["info"]["projectID"], "the project id is B's own");

    // Pulled again: nothing to do, nothing touched.
    let again = pull(&adapter_b, &manifest, &blobs, None, None).unwrap();
    assert_eq!(again.outcome, InstallOutcome::InSync);
    assert_eq!(again.project_root, dir_b);

    // What B has now pushes back as the same bundle content.
    let (_, canonical_b) = manifest_and_blobs(&adapter_b, ROOT, &dir_b, &b.path("blobs"));
    assert_eq!(canonical_b, canonical);
}

#[test]
fn an_older_copy_is_replaced_after_a_backup_and_a_changed_one_never_is() {
    let (Some(a), Some(b)) = (Isolated::new(), Isolated::new()) else { return };
    let (dir_a, dir_b) = (a.path("proj/a"), b.path("proj/b"));
    seed(&a, &dir_a);
    let (adapter_a, adapter_b) = (a.adapter(), b.adapter());
    let blobs = a.path("blobs");
    let (full, full_canonical) = manifest_and_blobs(&adapter_a, ROOT, &dir_a, &blobs);

    // B pulled an earlier state: the root with its first messages only.
    let mut early = root_doc(&dir_b);
    early["messages"].as_array_mut().unwrap().truncate(3);
    import(&b, &early, &dir_b);
    let early_canonical = v2::canonical_of(&tree(&b, ROOT));
    assert_ne!(early_canonical, full_canonical);

    // Without a record of the last sync the contents decide: it is behind.
    // With one, unchanged since: the same.
    let base = Base { canonical: early_canonical.clone(), files: Default::default() };
    // A busy session is never replaced: with the service up, a turn is
    // running; with it gone, an unfinished turn is waiting for OpenCode.
    rusqlite::Connection::open(&b.db).unwrap().execute("UPDATE session_v2 SET time_suspended = 5", []).unwrap();
    let err = pull(&adapter_b, &full, &blobs, None, Some(&base)).unwrap_err();
    assert!(err.to_string().contains("no longer running"), "{err}");
    super::cli2::testenv::service_up(&b.path("state/opencode"));
    let err = pull(&adapter_b, &full, &blobs, None, Some(&base)).unwrap_err();
    assert!(matches!(err, crate::CoreError::StoreBusy { .. }), "{err}");
    std::fs::remove_file(b.path("state/opencode").join("service.json")).unwrap();
    assert_eq!(v2::canonical_of(&tree(&b, ROOT)), early_canonical, "refused, nothing changed");
    rusqlite::Connection::open(&b.db).unwrap().execute("UPDATE session_v2 SET time_suspended = NULL", []).unwrap();

    let installed = pull(&adapter_b, &full, &blobs, None, Some(&base)).unwrap();
    assert_eq!(installed.outcome, InstallOutcome::Replaced);
    assert_eq!(v2::canonical_of(&tree(&b, ROOT)), full_canonical);
    assert_eq!(tree(&b, ROOT).len(), 3);
    // The copy that was replaced is in a backup, as export documents.
    let backups: Vec<_> = std::fs::read_dir(b.dir.path().join("backups").join(ROOT)).unwrap().collect();
    let backup = backups[0].as_ref().unwrap().path().join(format!("00-{ROOT}.json"));
    let kept: Value = serde_json::from_slice(&std::fs::read(backup).unwrap()).unwrap();
    assert_eq!(kept["messages"].as_array().unwrap().len(), 3);

    // Now B continues the session; the hub still has the old one: ahead.
    let mut docs = tree(&b, ROOT);
    docs[0]["messages"].as_array_mut().unwrap().push(json!({"id": "msg_b99", "type": "user", "time": {"created": 1760000010000_i64}, "text": "more"}));
    adapter_b.delete(&session(&adapter_b, ROOT)).unwrap();
    for doc in &docs {
        import(&b, doc, &dir_b);
    }
    let base = Base { canonical: full_canonical.clone(), files: Default::default() };
    assert_eq!(pull(&adapter_b, &full, &blobs, None, Some(&base)).unwrap().outcome, InstallOutcome::Ahead);
    assert_eq!(pull(&adapter_b, &full, &blobs, None, None).unwrap().outcome, InstallOutcome::Ahead, "also by content alone");
    // Both moved since they last agreed: diverged.
    // Both moved since they last agreed: A went on with a message of its own.
    let mut docs = tree(&a, ROOT);
    docs[0]["messages"].as_array_mut().unwrap().push(json!({"id": "msg_a99", "type": "user", "time": {"created": 1760000011000_i64}, "text": "from A"}));
    adapter_a.delete(&session(&adapter_a, ROOT)).unwrap();
    for doc in &docs {
        import(&a, doc, &dir_a);
    }
    let (moved_on, _) = manifest_and_blobs(&adapter_a, ROOT, &dir_a, &blobs);
    assert_eq!(pull(&adapter_b, &moved_on, &blobs, None, None).unwrap().outcome, InstallOutcome::Diverged);
    assert_eq!(pull(&adapter_b, &moved_on, &blobs, None, Some(&base)).unwrap().outcome, InstallOutcome::Diverged);
    assert!(tree(&b, ROOT)[0]["messages"].as_array().unwrap().iter().any(|m| m["id"] == "msg_b99"), "never replaced");

    // A project folder that disagrees with where it already is.
    let elsewhere = b.path("proj/elsewhere");
    assert!(pull(&adapter_b, &full, &blobs, Some(&elsewhere), None).unwrap_err().to_string().contains("already here"));
}

/// The other machine only renamed it: the titles are applied with a rename.
/// The session is not deleted and imported again (which would rebuild its
/// project, slug and message numbering for the sake of a title).
#[test]
fn a_title_only_difference_is_applied_as_a_rename_not_a_replace() {
    let (Some(a), Some(b)) = (Isolated::new(), Isolated::new()) else { return };
    let (dir_a, dir_b) = (a.path("proj/a"), b.path("proj/b"));
    seed(&a, &dir_a);
    let (adapter_a, adapter_b) = (a.adapter(), b.adapter());
    let blobs = a.path("blobs");
    let (first, first_canonical) = manifest_and_blobs(&adapter_a, ROOT, &dir_a, &blobs);
    assert_eq!(pull(&adapter_b, &first, &blobs, Some(&dir_b), None).unwrap().outcome, InstallOutcome::New);
    let slug = |env: &Isolated| conn(env).query_row("SELECT slug FROM session_v2 WHERE id = ?1", [ROOT], |r| r.get::<_, String>(0)).unwrap();
    let (slug_before, messages_before) = (slug(&b), tree(&b, ROOT)[0]["messages"].clone());

    adapter_a.rename(&session(&adapter_a, ROOT), "Root, renamed").unwrap();
    adapter_a.rename(&session(&adapter_a, CHILD), "Child, renamed").unwrap();
    let (renamed, _) = manifest_and_blobs(&adapter_a, ROOT, &dir_a, &blobs);
    let base = Base { canonical: first_canonical, files: Default::default() };
    let installed = pull(&adapter_b, &renamed, &blobs, None, Some(&base)).unwrap();
    assert_eq!(installed.outcome, InstallOutcome::Renamed);

    let on_b = tree(&b, ROOT);
    assert_eq!(on_b[0]["info"]["title"], "Root, renamed");
    assert_eq!(on_b.iter().find(|t| t["info"]["id"] == CHILD).unwrap()["info"]["title"], "Child, renamed");
    assert_eq!(on_b[0]["messages"], messages_before);
    assert_eq!(slug(&b), slug_before, "an import would have regenerated the slug: nothing was re-imported");
    assert_eq!(v2::canonical_of(&on_b), v2::canonical_of(&tree(&a, ROOT)), "both machines agree again");
    // What was saved: the old titles, not a copy of the whole tree.
    let backup = std::fs::read_dir(b.dir.path().join("backups").join(ROOT)).unwrap().next().unwrap().unwrap().path();
    let mut names: Vec<String> = std::fs::read_dir(&backup).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    assert_eq!(names, [format!("00-{ROOT}.info.json"), format!("01-{CHILD}.info.json"), format!("02-{GRANDCHILD}.info.json")]);
    let old: Value = serde_json::from_slice(&std::fs::read(backup.join(&names[0])).unwrap()).unwrap();
    assert_eq!(old["title"], "All kinds");

    // Pulled again: the same titles, nothing to do.
    assert_eq!(pull(&adapter_b, &renamed, &blobs, None, Some(&base)).unwrap().outcome, InstallOutcome::InSync);
}

/// If the hub's copy cannot be imported after the old one was deleted, the
/// old one is put back and the backup is named.
#[test]
fn a_failed_replace_restores_what_was_there() {
    let (Some(a), Some(b)) = (Isolated::new(), Isolated::new()) else { return };
    let (dir_a, dir_b) = (a.path("proj/a"), b.path("proj/b"));
    seed(&a, &dir_a);
    let blobs = a.path("blobs");
    let (good, _) = manifest_and_blobs(&a.adapter(), ROOT, &dir_a, &blobs);
    // The bundle's assistant message has a tool that completed with no content: the schema refuses it.
    let path = blobs.join(good.files[0].sha256.as_ref().unwrap());
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    doc["sessions"][ROOT]["messages"][7]["content"][2]["state"]["content"] = json!([]);
    let bytes = serde_json::to_vec(&doc).unwrap();
    let sha = crate::fsutil::sha256_hex(&bytes);
    std::fs::write(blobs.join(&sha), &bytes).unwrap();
    let mut bad = good.clone();
    bad.files[0].sha256 = Some(sha.clone());
    bad.files[0].size = bytes.len() as u64;
    bad.canonical = "tree2:newer".into();

    let mut early = root_doc(&dir_b);
    early["messages"].as_array_mut().unwrap().truncate(3);
    import(&b, &early, &dir_b);
    // ...with a turn it abandoned half way (never settled): the backup keeps it, and
    // putting the copy back must cope with the import leaving it out.
    rusqlite::Connection::open(&b.db)
        .unwrap()
        .execute(
            "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data)
             VALUES ('msg_abandoned', ?1, 'assistant', 99, 1760000099000, 1760000099000, ?2)",
            rusqlite::params![ROOT, json!({"agent": "build", "model": {"id": "m", "providerID": "p"}, "content": [], "time": {"created": 1760000099000_i64}}).to_string()],
        )
        .unwrap();
    let before = v2::canonical_of(&tree(&b, ROOT));
    let base = Base { canonical: before.clone(), files: Default::default() };
    let err = pull(&b.adapter(), &bad, &blobs, None, Some(&base)).unwrap_err().to_string();
    assert!(err.contains("The previous copy was restored") && err.contains("is also in "), "{err}");
    let backup = std::fs::read_dir(b.dir.path().join("backups").join(ROOT)).unwrap().next().unwrap().unwrap().path();
    let kept = std::fs::read_to_string(backup.join(format!("00-{ROOT}.json"))).unwrap();
    assert!(kept.contains("msg_abandoned"), "the abandoned turn is in the backup");
    assert!(err.contains("opencode session import <file> --directory <directory>"), "{err}");
    assert_eq!(v2::canonical_of(&tree(&b, ROOT)), before, "B is as it was");
}

#[test]
fn bundles_of_the_other_generation_are_refused_whatever_the_machine() {
    let Some(a) = Isolated::new() else { return };
    let dir_a = a.path("proj/a");
    seed(&a, &dir_a);
    let blobs = a.path("blobs");
    let (v2_manifest, _) = manifest_and_blobs(&a.adapter(), ROOT, &dir_a, &blobs);

    // A 1.x machine (its schema, its store, a 1.x binary) given a 2.x bundle.
    let one = tempfile::tempdir().unwrap();
    let db = one.path().join("opencode.db");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("CREATE TABLE session (id TEXT); CREATE TABLE message (id TEXT); CREATE TABLE part (id TEXT);")
        .unwrap();
    let old_binary = one.path().join("opencode");
    std::fs::write(&old_binary, "#!/bin/sh\n[ \"$1\" = --version ] && echo 'opencode v1.17.18'\nexit 0\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&old_binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let one_x = OpenCodeAdapter::with_db(&db).with_cli(super::cli2::Cli::isolated(&old_binary, vec![], one.path().join("state"), one.path()));
    let mut with_version = v2_manifest.clone();
    with_version.agent_version = Some("2.0.25".into());
    let err = pull(&one_x, &with_version, &blobs, None, None).unwrap_err().to_string();
    assert!(err.contains("pushed from OpenCode 2.x (2.0.25)") && err.contains("this machine has OpenCode 1.x (1.17.18)") && err.contains("same major version"), "{err}");
    // A store still in the 1.x format with a 2.x OpenCode installed: it has to migrate first.
    let err = pull(&OpenCodeAdapter::with_db(&db).with_cli(a.cli.clone()), &v2_manifest, &blobs, None, None).unwrap_err().to_string();
    assert!(err.contains("start OpenCode 2.x once so it migrates your sessions, then retry"), "{err}");
    // No store and no OpenCode at all.
    let nothing = OpenCodeAdapter::with_db(one.path().join("none.db"))
        .with_cli(super::cli2::Cli::isolated(one.path().join("not-here"), vec![], one.path().join("state"), one.path()));
    let err = pull(&nothing, &v2_manifest, &blobs, None, None).unwrap_err().to_string();
    assert!(err.contains("OpenCode is not installed here"), "{err}");

    // A 2.x machine given 1.x rows.
    let mut rows = v2_manifest.clone();
    rows.extra = Value::Null;
    rows.files[0].path = "rows.json".into();
    let err = pull(&a.adapter(), &rows, &blobs, None, None).unwrap_err().to_string();
    assert!(err.contains("pushed from OpenCode 1.x"), "{err}");
    assert_eq!(tree(&a, ROOT).len(), 3, "nothing was touched");

    // A machine with no store yet follows its binary: here a 2.x one takes the 2.x bundle
    // and refuses 1.x rows.
    let fresh = Isolated::new().unwrap();
    let dir = fresh.path("proj/f");
    assert!(pull(&fresh.adapter(), &rows, &blobs, Some(&dir), None).unwrap_err().to_string().contains("pushed from OpenCode 1.x"));
    assert_eq!(pull(&fresh.adapter(), &v2_manifest, &blobs, Some(&dir), None).unwrap().outcome, InstallOutcome::New);
}

fn ir() -> IrSession {
    let ts = |s: &str| s.parse::<jiff::Timestamp>().ok();
    let text = |role, at, parts, id: &str| IrMessage {
        role,
        timestamp: ts(at),
        parts,
        source_id: Some(id.into()),
        extensions: ExtBag::new(),
    };
    IrSession {
        ir_version: 1,
        source: IrProvenance {
            agent: AgentKind::ClaudeCode,
            native_id: "aaaaaaaa-1111-2222-3333-444444444444".into(),
            agent_version: Some("x".into()),
            exported_at: jiff::Timestamp::UNIX_EPOCH,
            exporter_version: "test".into(),
        },
        title: Some("From Claude".into()),
        slug: None,
        project_path: PortablePath("/tmp/replaced".into()),
        created: ts("2026-08-01T10:00:00Z"),
        updated: ts("2026-08-01T11:00:00Z"),
        model: Some("claude-x".into()),
        usage: Default::default(),
        messages: vec![
            text(IrRole::User, "2026-08-01T10:00:00Z", vec![IrPart::Text { text: "please list files".into() }], "u1"),
            text(
                IrRole::Assistant,
                "2026-08-01T10:00:05Z",
                vec![
                    IrPart::Reasoning { summary: "I should run ls".into(), opaque: true },
                    IrPart::Text { text: "Listing now.".into() },
                    IrPart::ToolCall { call_id: "call_1".into(), name: "Bash".into(), input: json!({"command": "ls"}) },
                    IrPart::ToolCall { call_id: "call_2".into(), name: "mcp__weird__tool".into(), input: json!("raw") },
                    IrPart::ToolCall { call_id: "call_3".into(), name: "Read".into(), input: Value::Null },
                ],
                "a1",
            ),
            text(
                IrRole::User,
                "2026-08-01T10:00:10Z",
                vec![
                    IrPart::ToolResult { call_id: "call_1".into(), output: "a.txt\nb.txt".into(), is_error: false, truncated: false },
                    IrPart::ToolResult { call_id: "call_2".into(), output: "boom".into(), is_error: true, truncated: false },
                ],
                "u2",
            ),
            text(IrRole::Assistant, "2026-08-01T10:00:15Z", vec![IrPart::Text { text: "Two files found.".into() }], "a2"),
        ],
        extensions: ExtBag::new(),
    }
}

#[test]
fn an_ir_imports_as_a_session_opencode_accepts_and_a_second_import_is_a_no_op() {
    let Some(env) = Isolated::new() else { return };
    let dir = env.path("proj/a");
    let adapter = env.adapter();
    let mut source = ir();
    source.project_path = PortablePath::from_path(&dir);
    // A store OpenCode has already set up, as on any machine that runs it.
    import(&env, &root_doc(&dir), &dir);

    let dry = adapter.import_ir(&source, &ImportOpts { mode: ImportMode::Full, project: None, dry_run: true }).unwrap();
    let doc: Value = serde_json::from_str(&dry.dry_run_artifact.unwrap()).unwrap();
    assert_eq!(conn(&env).query_row("SELECT COUNT(*) FROM session_v2", [], |r| r.get::<_, i64>(0)).unwrap(), 1, "a dry run writes nothing");
    assert_eq!(doc["messages"].as_array().unwrap().len(), 4, "three turns and the idle marker (the tool results ride in the call)");

    let done = adapter.import_ir(&source, &ImportOpts { mode: ImportMode::Full, project: None, dry_run: false }).unwrap();
    assert!(!done.in_sync);
    let id = done.target.unwrap().native_id;
    let stored = v2::transfer(&conn(&env), &id, true).unwrap().unwrap();
    assert_eq!(stored["info"]["title"], "From Claude");
    assert_eq!(stored["info"]["location"]["directory"], dir.display().to_string());
    let types: Vec<&str> = stored["messages"].as_array().unwrap().iter().map(|m| m["type"].as_str().unwrap()).collect();
    assert_eq!(types, ["user", "assistant", "assistant", "idle"]);
    let tools = &stored["messages"][1]["content"];
    assert_eq!(tools[2]["name"], "shell", "bash is 2.x's shell");
    assert_eq!(tools[2]["state"]["status"], "completed");
    assert_eq!(tools[2]["state"]["content"][0]["text"], "a.txt\nb.txt");
    assert_eq!(tools[3]["name"], "mcp__weird__tool");
    assert_eq!(tools[3]["state"]["status"], "error");
    assert_eq!(tools[3]["state"]["input"], json!({"value": "raw"}), "an input is always an object");
    assert_eq!(tools[4]["state"]["error"]["type"], "interrupted", "a call with no recorded result says so");

    // What reads it back sees the conversation.
    let back = adapter.export_ir(&session(&adapter, &id)).unwrap();
    assert!(matches!(&back.messages[0].parts[..], [IrPart::Text { text }] if text == "please list files"));
    assert!(back.messages[1].parts.iter().any(|p| matches!(p, IrPart::ToolCall { name, .. } if name == "shell")));

    let again = adapter.import_ir(&source, &ImportOpts { mode: ImportMode::Full, project: None, dry_run: false }).unwrap();
    assert!(again.in_sync);
    assert_eq!(conn(&env).query_row("SELECT COUNT(*) FROM session_v2", [], |r| r.get::<_, i64>(0)).unwrap(), 2);

    let mut missing = ir();
    missing.source.native_id = "other".into();
    missing.project_path = PortablePath::from_path(&env.path("proj/nope").join("deeper"));
    let err = adapter.import_ir(&missing, &ImportOpts { mode: ImportMode::Full, project: None, dry_run: false }).unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");
}
