//! A session this machine has but never synced, listed against a real hub
//! over HTTP: the hub's copy is read and related to this one, so the row says
//! which side is ahead instead of leaving it to a pull. One test, because the
//! stores, the data directory and the machine identity are process-wide and
//! every step builds on the one before.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use asm_core::adapter::SessionFilter;
use asm_core::adapter::claude::encode_project_dir;
use asm_core::bulk::ItemOutcome;
use asm_core::hub::actions::{self, Lean, Row, RowState};
use asm_core::hub::bundle::Relation;
use asm_core::hub::client::{self, PutOutcome, Remote};
use asm_core::hub::manifest::Manifest;
use asm_core::hub::state::SyncState;
use asm_core::hub::store::Hub;
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

/// The hub on a free loopback port. Its thread lives as long as the test
/// process.
fn serve(hub: Hub) -> String {
    let state = Arc::new(HubState { hub, max_blob: 16 << 20, peer: false });
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

/// A Claude record; `pad` makes a transcript as big as a test needs.
fn record(project: &Path, id: &str, i: usize, text: &str, pad: usize) -> String {
    let kind = if i.is_multiple_of(2) { "user" } else { "assistant" };
    let text = format!("{text}{}", "x".repeat(pad));
    let content = if kind == "user" {
        serde_json::json!(text)
    } else {
        serde_json::json!([{ "type": "text", "text": text }])
    };
    format!(
        "{}\n",
        serde_json::json!({ "type": kind, "cwd": project, "sessionId": id, "uuid": format!("u{i}"),
                            "parentUuid": i.checked_sub(1).map(|p| format!("u{p}")),
                            "timestamp": "2026-10-09T10:00:00Z",
                            "message": { "role": kind, "content": content } })
    )
}

fn transcript(project: &Path, id: &str, texts: &[&str], pad: usize) -> String {
    texts.iter().enumerate().map(|(i, t)| record(project, id, i, t, pad)).collect()
}

/// Put `text` on the hub as machine B's push, dated a month before the local files.
fn hub_push(b: &Remote, id: &str, project: &Path, text: &str, parent: Option<&str>) -> String {
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

/// This machine's copy of the session.
fn write_local(project: &Path, id: &str, text: &str) {
    let dir = isolate().join("claude/projects").join(encode_project_dir(project.to_str().unwrap()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("{id}.jsonl")), text).unwrap();
}

fn rows(a: &Remote) -> Vec<Row> {
    let local = ops::list_sessions(&SessionFilter::default()).unwrap();
    actions::remote_list(a, &local).unwrap()
}

fn row<'a>(rows: &'a [Row], id: &str) -> &'a Row {
    rows.iter().find(|r| r.id == id).unwrap_or_else(|| panic!("{id} is not listed"))
}

const AHEAD: &str = "0a1c0001-0000-4000-8000-000000000001";
const BEHIND: &str = "0a1c0002-0000-4000-8000-000000000002";
const DIVERGED: &str = "0a1c0003-0000-4000-8000-000000000003";
const BIG: &str = "0a1c0004-0000-4000-8000-000000000004";
const SAME: &str = "0a1c0005-0000-4000-8000-000000000005";
const UNREADABLE: &str = "0a1c0006-0000-4000-8000-000000000006";

fn recorded(a: &Remote, id: &str) -> Option<asm_core::hub::state::Tracked> {
    SyncState::load(a).unwrap().get(&format!("claude-code:{id}")).cloned()
}

#[test]
fn an_uncompared_session_says_which_side_is_ahead_by_reading_the_hubs_copy() {
    let root = isolate();
    let project = root.join("home/code/app");
    fs::create_dir_all(&project).unwrap();
    let hub_root = root.join("hub");
    let hub = Hub::open(&hub_root).unwrap();
    let token = hub.join_token().unwrap();
    let url = serve(hub);
    let a = client::join(&url, &token, "laptop", false).unwrap();
    let b = as_machine("machine-b", &root.join("asm-b"), || client::join(&url, &token, "desktop", false).unwrap());
    assert_ne!(a.machine.id, b.machine.id);

    // The user's own report: the hub's copy is older and smaller, this
    // machine went on from it, and this machine has no record of syncing.
    let hub_rev = hub_push(&b, AHEAD, &project, &transcript(&project, AHEAD, &["one", "two"], 0), None);
    write_local(&project, AHEAD, &transcript(&project, AHEAD, &["one", "two", "three", "four"], 0));
    // The reverse: the hub has gone on, this machine is behind.
    hub_push(&b, BEHIND, &project, &transcript(&project, BEHIND, &["one", "two", "three"], 0), None);
    write_local(&project, BEHIND, &transcript(&project, BEHIND, &["one"], 0));
    // Each side has a turn the other lacks.
    hub_push(&b, DIVERGED, &project, &transcript(&project, DIVERGED, &["one", "from the hub"], 0), None);
    write_local(&project, DIVERGED, &transcript(&project, DIVERGED, &["one", "from here"], 0));
    // The same on both.
    let same = transcript(&project, SAME, &["one", "two"], 0);
    hub_push(&b, SAME, &project, &same, None);
    write_local(&project, SAME, &same);
    // Over what a listing reads (2 MiB): the hub's copy is 2.5 MiB.
    let big = transcript(&project, BIG, &["one", "two"], 1300 * 1024);
    hub_push(&b, BIG, &project, &big, None);
    write_local(&project, BIG, &(big.clone() + &record(&project, BIG, 2, "three", 1300 * 1024)));

    let listed = rows(&a);

    // This machine is ahead: Needs push, with the base recorded the way a
    // pull records a copy that is ahead.
    let r = row(&listed, AHEAD);
    assert_eq!((r.state, r.label), (RowState::Ahead, "Needs push"), "{r:?}");
    assert_eq!(r.detail.compare.unwrap().relation, Some(Relation::LocalAhead));
    assert!(r.hint.contains("goes on from the hub's"), "{}", r.hint);
    let base = recorded(&a, AHEAD).expect("the base was recorded");
    assert_eq!(base.hub_rev, hub_rev);
    assert_eq!(base.fingerprint, "", "so the next push looks at this copy");
    assert_ne!(base.canonical, fsutil::sha256_hex(transcript(&project, AHEAD, &["one", "two", "three", "four"], 0).as_bytes()));
    // ...and a push from here is accepted by the hub: it names that revision.
    let local = ops::list_sessions(&SessionFilter::default()).unwrap();
    let ahead_here: Vec<_> = local.iter().filter(|s| s.handle.native_id == AHEAD).cloned().collect();
    let pushed = actions::push(&a, &ahead_here, false, false).unwrap();
    assert!(
        matches!(&pushed.items[0].outcome, ItemOutcome::Ok { note } if note.starts_with("pushed")),
        "{:?}",
        pushed.items[0].outcome
    );
    let history = a.history("claude-code", AHEAD).unwrap().unwrap();
    assert_eq!(history.revisions.len(), 2);
    assert_eq!(history.manifest.parent_rev.as_deref(), Some(hub_rev.as_str()));
    assert_eq!(history.manifest.machine.as_ref().map(|m| m.name.as_str()), Some("laptop"));
    assert_eq!(row(&rows(&a), AHEAD).state, RowState::InSync, "and now it is synced");

    // The hub is ahead: Needs pull, nothing recorded.
    let r = row(&listed, BEHIND);
    assert_eq!((r.state, r.label), (RowState::Behind, "Needs pull"), "{r:?}");
    assert_eq!(r.detail.compare.unwrap().relation, Some(Relation::HubAhead));
    assert!(recorded(&a, BEHIND).is_none());

    // Both went on: Diverged, nothing recorded.
    let r = row(&listed, DIVERGED);
    assert_eq!((r.state, r.label), (RowState::Diverged, "Diverged"), "{r:?}");
    assert_eq!(r.detail.compare.unwrap().relation, Some(Relation::Diverged));
    assert!(recorded(&a, DIVERGED).is_none());

    // The same conversation: in sync, and recorded as such.
    assert_eq!(row(&listed, SAME).state, RowState::InSync);
    assert!(recorded(&a, SAME).is_some());

    // Too big for a listing to read: still "Differs" and still a pull, with
    // only an advisory lean from the times and sizes, which agree here.
    let r = row(&listed, BIG);
    assert_eq!((r.state, r.label), (RowState::Untracked, "Differs"), "{r:?}");
    let compare = r.detail.compare.unwrap();
    assert_eq!((compare.relation, compare.lean), (None, Some(Lean::Local)));
    assert!(r.hint.contains("looks newer") && r.hint.contains("only a guess"), "{}", r.hint);
    assert_eq!(r.action, Some(actions::SyncAction::Pull), "a lean decides nothing");
    assert!(recorded(&a, BIG).is_none());

    // The explicit compare reads it, up to a larger cap, and then knows.
    let big_here = local.iter().find(|s| s.handle.native_id == BIG).unwrap();
    let head = a.heads().unwrap().into_iter().find(|h| h.manifest.id == BIG).unwrap();
    let mut state = SyncState::load(&a).unwrap();
    let found = actions::compare_session(&a, &mut state, big_here, &head).unwrap();
    assert_eq!(found.relation, Some(Relation::LocalAhead));
    assert_eq!(row(&rows(&a), BIG).state, RowState::Ahead, "and a listing agrees from then on");

    // The hub's file cannot be fetched: the listing still answers, the row
    // stays "Differs" (a lean at most), and nothing is recorded or failed.
    let text = transcript(&project, UNREADABLE, &["one", "two"], 0);
    hub_push(&b, UNREADABLE, &project, &text, None);
    write_local(&project, UNREADABLE, &(text.clone() + &record(&project, UNREADABLE, 2, "three", 0)));
    let sha = fsutil::sha256_hex(text.as_bytes());
    fs::remove_file(hub_root.join("blobs").join(&sha[..2]).join(&sha)).unwrap();
    let listed = rows(&a);
    let r = row(&listed, UNREADABLE);
    assert_eq!((r.state, r.label), (RowState::Untracked, "Differs"), "{r:?}");
    assert_eq!(r.detail.compare.unwrap().relation, None);
    assert!(recorded(&a, UNREADABLE).is_none());
    // Everything else in that listing was answered from what was worked out.
    assert_eq!(row(&listed, BEHIND).state, RowState::Behind);

}
