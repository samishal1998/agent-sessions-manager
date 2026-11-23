//! Recompare against a real hub over HTTP: each way this machine's record of
//! a session can be wrong, and what comparing by content makes of it. Then the
//! hub moving to a new address, keeping those records. One test, because the
//! stores, the data directory and the machine identity are process-wide and
//! every step builds on the one before.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use asm_core::adapter::SessionFilter;
use asm_core::adapter::claude::encode_project_dir;
use asm_core::bulk::{BulkReport, ItemOutcome};
use asm_core::hub::actions::{self, Row, RowState};
use asm_core::hub::client::{self, PutOutcome, Remote};
use asm_core::hub::manifest::Manifest;
use asm_core::hub::state::SyncState;
use asm_core::hub::store::Hub;
use asm_core::model::{AgentKind, Session};
use asm_core::{fsutil, ops};
use asm_web::hub::{HubState, router};

fn isolate() -> &'static Path {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (var, sub) in [
            ("HOME", "home"),
            ("ASM_DATA_DIR", "asm-a"),
            ("XDG_DATA_HOME", "xdg-data"),
            ("XDG_CONFIG_HOME", "xdg-config"),
            ("XDG_STATE_HOME", "xdg-state"),
            ("XDG_CACHE_HOME", "xdg-cache"),
            ("CLAUDE_CONFIG_DIR", "claude"),
            ("CODEX_HOME", "codex"),
            ("PI_CODING_AGENT_DIR", "pi"),
            ("ASM_OMP_ROOT", "omp"),
            ("JCODE_HOME", "jcode"),
            ("ASM_ANTIGRAVITY_ROOT", "antigravity"),
        ] {
            fs::create_dir_all(root.join(sub)).unwrap();
            // Safety: set once per test process, before anything else runs.
            unsafe { std::env::set_var(var, root.join(sub)) };
        }
        unsafe { std::env::set_var("ASM_MACHINE_UID", "machine-a") };
        dir
    })
    .path()
}

/// A hub over `root` on a free loopback port. Its thread lives as long as the
/// test process. Two of them over one root are one hub at two addresses.
fn serve(root: &Path) -> String {
    let state = Arc::new(HubState { hub: Hub::open(root).unwrap(), max_blob: 16 << 20, peer: false });
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap().port()).unwrap();
            axum::serve(listener, router(state)).await.unwrap();
        });
    });
    format!("http://127.0.0.1:{}", rx.recv().unwrap())
}

/// Run `f` as another machine: its own identity and its own data directory.
fn as_machine<T>(uid: &str, data: &Path, f: impl FnOnce() -> T) -> T {
    fs::create_dir_all(data).unwrap();
    let before = (std::env::var_os("ASM_MACHINE_UID"), std::env::var_os("ASM_DATA_DIR"));
    unsafe {
        std::env::set_var("ASM_MACHINE_UID", uid);
        std::env::set_var("ASM_DATA_DIR", data);
    }
    let out = f();
    unsafe {
        std::env::set_var("ASM_MACHINE_UID", before.0.unwrap());
        std::env::set_var("ASM_DATA_DIR", before.1.unwrap());
    }
    out
}

fn transcript(project: &Path, id: &str, texts: &[&str]) -> String {
    texts
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let kind = if i.is_multiple_of(2) { "user" } else { "assistant" };
            let content =
                if kind == "user" { serde_json::json!(text) } else { serde_json::json!([{ "type": "text", "text": text }]) };
            format!(
                "{}\n",
                serde_json::json!({ "type": kind, "cwd": project, "sessionId": id, "uuid": format!("u{i}"),
                                    "parentUuid": i.checked_sub(1).map(|p| format!("u{p}")),
                                    "timestamp": "2026-10-09T10:00:00Z",
                                    "message": { "role": kind, "content": content } })
            )
        })
        .collect()
}

/// Put `text` on the hub as machine B's push on top of `parent`.
fn hub_push(b: &Remote, id: &str, project: &Path, text: &str, parent: &str) -> String {
    let sha = fsutil::sha256_hex(text.as_bytes());
    let file = isolate().join(format!("hub-push-{sha}"));
    fs::write(&file, text).unwrap();
    b.put_blob(&sha, &file).unwrap();
    let manifest: Manifest = serde_json::from_value(serde_json::json!({
        "schema": 1, "agent": "claude-code", "id": id,
        "project_root": project, "project_root_portable": project,
        "updated": "2026-09-01T00:00:00Z",
        "canonical": sha, "parent_rev": parent,
        "files": [{ "path": "transcript.jsonl", "sha256": sha, "size": text.len() }],
    }))
    .unwrap();
    match b.put_revision(&manifest).unwrap() {
        PutOutcome::Created { rev } => rev,
        PutOutcome::Conflict { head } => panic!("the hub's head is {head:?}"),
    }
}

fn local_path(project: &Path, id: &str) -> PathBuf {
    isolate().join("claude/projects").join(encode_project_dir(project.to_str().unwrap())).join(format!("{id}.jsonl"))
}

fn write_local(project: &Path, id: &str, text: &str) {
    let path = local_path(project, id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Change the file's time and nothing else, as an editor or a sync tool can.
fn touch(project: &Path, id: &str) {
    let file = fs::File::options().write(true).open(local_path(project, id)).unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(120)).unwrap();
}

fn session(id: &str) -> Session {
    ops::list_sessions(&SessionFilter::default()).unwrap().into_iter().find(|s| s.handle.native_id == id).unwrap()
}

fn push(a: &Remote, id: &str) {
    let report = actions::push(a, &[session(id)], false, false).unwrap();
    assert!(matches!(&report.items[0].outcome, ItemOutcome::Ok { .. }), "{:?}", report.items[0].outcome);
}

fn head_rev(a: &Remote, id: &str) -> String {
    a.heads().unwrap().into_iter().find(|h| h.manifest.id == id).unwrap().rev
}

fn row(a: &Remote, id: &str) -> Row {
    let local = ops::list_sessions(&SessionFilter::default()).unwrap();
    actions::remote_list(a, &local).unwrap().into_iter().find(|r| r.id == id).unwrap()
}

fn recompare(a: &Remote, ids: &[&str]) -> BulkReport {
    let local = ops::list_sessions(&SessionFilter::default()).unwrap();
    let only: Vec<_> = ids.iter().map(|id| (AgentKind::ClaudeCode, id.to_string())).collect();
    actions::recompare_all(a, &local, &only, |_, _| {}).unwrap()
}

fn outcome<'a>(report: &'a BulkReport, id: &str) -> &'a ItemOutcome {
    &report.items.iter().find(|i| i.native_id == id).unwrap().outcome
}

fn noted(report: &BulkReport, id: &str) -> String {
    match outcome(report, id) {
        ItemOutcome::Ok { note } => note.clone(),
        other => panic!("{id}: {other:?}"),
    }
}

fn recorded(a: &Remote, id: &str) -> Option<asm_core::hub::state::Tracked> {
    SyncState::load(a).unwrap().get(&format!("claude-code:{id}")).cloned()
}

const TOUCHED: &str = "0b2c0001-0000-4000-8000-000000000001";
const SAME_AGAIN: &str = "0b2c0002-0000-4000-8000-000000000002";
const HUB_AHEAD: &str = "0b2c0003-0000-4000-8000-000000000003";
const DIVERGED: &str = "0b2c0004-0000-4000-8000-000000000004";
const LOCAL_AHEAD: &str = "0b2c0005-0000-4000-8000-000000000005";
const UNREADABLE: &str = "0b2c0006-0000-4000-8000-000000000006";
const NOT_ON_HUB: &str = "0b2c0007-0000-4000-8000-000000000007";

#[test]
fn recompare_replaces_a_wrong_record_with_what_the_content_says_and_a_moved_hub_keeps_them() {
    let root = isolate();
    let project = root.join("home/code/app");
    fs::create_dir_all(&project).unwrap();
    let hub_root = root.join("hub");
    let token = Hub::open(&hub_root).unwrap().join_token().unwrap();
    let url = serve(&hub_root);
    let a = client::join(&url, &token, "laptop", false).unwrap();
    let b = as_machine("machine-b", &root.join("asm-b"), || client::join(&url, &token, "desktop", false).unwrap());

    // Each session pushed from here first, so this machine holds a record.
    let one = |id| transcript(&project, id, &["one"]);
    for id in [TOUCHED, SAME_AGAIN, HUB_AHEAD, DIVERGED, LOCAL_AHEAD, UNREADABLE] {
        write_local(&project, id, &one(id));
        push(&a, id);
    }
    write_local(&project, NOT_ON_HUB, &one(NOT_ON_HUB));

    // The file's time moved, its content did not: the record says "Needs push".
    touch(&project, TOUCHED);
    // The other machine pushed the very same conversation again: "Needs pull".
    hub_push(&b, SAME_AGAIN, &project, &one(SAME_AGAIN), &head_rev(&a, SAME_AGAIN));
    // The hub went on and this file was only touched: the record says
    // "Diverged", the content says the hub is ahead.
    hub_push(&b, HUB_AHEAD, &project, &transcript(&project, HUB_AHEAD, &["one", "two"]), &head_rev(&a, HUB_AHEAD));
    touch(&project, HUB_AHEAD);
    // Each side went on its own way: diverged by record and by content.
    hub_push(&b, DIVERGED, &project, &transcript(&project, DIVERGED, &["one", "from the hub"]), &head_rev(&a, DIVERGED));
    write_local(&project, DIVERGED, &transcript(&project, DIVERGED, &["one", "from here"]));
    // This machine went on: ahead by record and by content.
    write_local(&project, LOCAL_AHEAD, &transcript(&project, LOCAL_AHEAD, &["one", "two"]));
    // The hub went on, this file was touched, and the hub's copy is gone.
    let gone = transcript(&project, UNREADABLE, &["one", "two"]);
    hub_push(&b, UNREADABLE, &project, &gone, &head_rev(&a, UNREADABLE));
    touch(&project, UNREADABLE);
    let sha = fsutil::sha256_hex(gone.as_bytes());
    fs::remove_file(hub_root.join("blobs").join(&sha[..2]).join(&sha)).unwrap();

    // What the records say, before.
    assert_eq!(row(&a, TOUCHED).state, RowState::Ahead);
    assert_eq!(row(&a, SAME_AGAIN).state, RowState::Behind);
    assert_eq!(row(&a, HUB_AHEAD).state, RowState::Diverged);
    assert_eq!(row(&a, DIVERGED).state, RowState::Diverged);
    assert_eq!(row(&a, LOCAL_AHEAD).state, RowState::Ahead);
    assert_eq!(row(&a, UNREADABLE).state, RowState::Diverged);

    let report = recompare(&a, &[TOUCHED, SAME_AGAIN, HUB_AHEAD, DIVERGED, LOCAL_AHEAD, UNREADABLE, NOT_ON_HUB]);

    // Same content: in sync, recorded against the hub's current head.
    assert_eq!(noted(&report, TOUCHED), "in sync");
    assert_eq!(row(&a, TOUCHED).state, RowState::InSync);
    assert_eq!(noted(&report, SAME_AGAIN), "in sync");
    assert_eq!(row(&a, SAME_AGAIN).state, RowState::InSync);
    assert_eq!(recorded(&a, SAME_AGAIN).unwrap().hub_rev, head_rev(&a, SAME_AGAIN));

    // The hub is ahead: the record that said "Diverged" is gone, the listing
    // says "Needs pull", and the pull applies it.
    assert_eq!(noted(&report, HUB_AHEAD), "the hub is ahead: pull it");
    assert!(recorded(&a, HUB_AHEAD).is_none());
    assert_eq!(row(&a, HUB_AHEAD).state, RowState::Behind);
    actions::pull(&a, &format!("claude-code:{HUB_AHEAD}"), None).unwrap();
    assert_eq!(row(&a, HUB_AHEAD).state, RowState::InSync);

    // Really diverged: still diverged, now from content.
    assert_eq!(noted(&report, DIVERGED), "diverged: changed here and on the hub");
    assert_eq!(row(&a, DIVERGED).state, RowState::Diverged);

    // Really ahead: still ahead, and the push goes through on the recorded base.
    assert_eq!(noted(&report, LOCAL_AHEAD), "this machine is ahead: push it");
    assert_eq!(row(&a, LOCAL_AHEAD).state, RowState::Ahead);
    push(&a, LOCAL_AHEAD);
    assert_eq!(row(&a, LOCAL_AHEAD).state, RowState::InSync);

    // Nothing could be proven: skipped, and the record is left as it was.
    assert!(matches!(outcome(&report, UNREADABLE), ItemOutcome::Skipped { reason } if reason.contains("could not be read")));
    assert!(recorded(&a, UNREADABLE).is_some());
    assert_eq!(row(&a, UNREADABLE).state, RowState::Diverged);

    // Named, but not on the hub: said so, not silently dropped.
    assert!(matches!(outcome(&report, NOT_ON_HUB), ItemOutcome::Skipped { reason } if reason.contains("nothing to compare")));

    // Everything at once finds the same.
    let all = actions::recompare_all(&a, &ops::list_sessions(&SessionFilter::default()).unwrap(), &[], |_, _| {}).unwrap();
    assert_eq!(noted(&all, TOUCHED), "in sync");
    assert!(all.items.iter().all(|i| i.native_id != NOT_ON_HUB), "only sessions on both sides");

    // The hub, reached at another address: the same hub, so the move keeps
    // this machine's registration and its records.
    let moved_url = serve(&hub_root);
    let moved = client::relocate(&moved_url, false).unwrap();
    assert_eq!((moved.url.as_str(), &moved.machine.id), (moved_url.as_str(), &a.machine.id));
    assert_eq!(client::load().unwrap().url, moved_url);
    assert_eq!(recorded(&moved, TOUCHED).unwrap().hub_rev, head_rev(&moved, TOUCHED));
    assert_eq!(row(&moved, TOUCHED).state, RowState::InSync, "no re-comparing after a move");

    // Another hub does not know this machine: refused, and nothing changes.
    let other = serve(&root.join("other-hub"));
    let refused = client::relocate(&other, false).err().expect("refused").to_string();
    assert!(refused.contains("did not accept this machine's credential"), "{refused}");
    assert_eq!(client::load().unwrap().url, moved_url);
    // Nor does something that is not a hub at all.
    let refused = client::relocate("http://127.0.0.1:9", false).err().expect("refused").to_string();
    assert!(!refused.is_empty());
    assert_eq!(client::load().unwrap().url, moved_url);
}
