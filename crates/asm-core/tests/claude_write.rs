//! Claude Code write-verb tests: rename (ai-title append), the relocate
//! procedure (encoder, sidecar move, marker, symlink repoint, job rehome),
//! archive/unarchive round-trip, delete with backups, and the live guard.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use asm_core::CoreError;
use asm_core::adapter::claude::{ClaudeAdapter, encode_project_dir, unarchive_by_id};
use asm_core::adapter::{AgentRead, AgentWrite, SessionFilter};
use asm_core::model::Session;

const SESSION: &str = "cccccccc-1111-2222-3333-444444444444";

/// Per-process asm data dir (backups + archive) — set once, before any test
/// touches paths::data_dir().
fn asm_data_dir() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        // Safety: set once per test process, before concurrent readers.
        unsafe { std::env::set_var("ASM_DATA_DIR", dir.path()) };
        dir
    });
    dir.path()
}

struct Fixture {
    _root_dir: tempfile::TempDir,
    root: PathBuf,
    project_a: PathBuf,
    adapter: ClaudeAdapter,
}

fn fixture() -> Fixture {
    asm_data_dir();
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path().to_path_buf();
    let project_a = root_dir.path().join("proj-a");
    fs::create_dir_all(&project_a).unwrap();
    let project_a = project_a.canonicalize().unwrap();

    let enc = encode_project_dir(project_a.to_str().unwrap());
    let store_project = root.join("projects").join(&enc);
    fs::create_dir_all(&store_project).unwrap();

    let cwd = project_a.display();
    let transcript = [
        format!(
            r#"{{"uuid":"u1","parentUuid":null,"timestamp":"2026-08-01T10:00:00Z","cwd":"{cwd}","sessionId":"{SESSION}","version":"2.1.230","gitBranch":"main","slug":"test","type":"user","message":{{"role":"user","content":"hi"}}}}"#
        ),
        format!(r#"{{"type":"ai-title","aiTitle":"Original title","sessionId":"{SESSION}"}}"#),
    ]
    .join("\n");
    fs::write(store_project.join(format!("{SESSION}.jsonl")), transcript).unwrap();

    // In-project sidecar dir with a task-output symlink into itself.
    let sidecar = store_project.join(SESSION);
    fs::create_dir_all(sidecar.join("tool-results")).unwrap();
    fs::create_dir_all(sidecar.join("tasks")).unwrap();
    fs::write(sidecar.join("tool-results/r1.txt"), "big output").unwrap();
    std::os::unix::fs::symlink(
        sidecar.join("tool-results/r1.txt"),
        sidecar.join("tasks/1.output"),
    )
    .unwrap();

    // UUID-keyed sidecars in the root.
    for dir in ["file-history", "tasks", "session-env"] {
        let d = root.join(dir).join(SESSION);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("marker"), dir).unwrap();
    }

    // A background job pointing at the transcript.
    let job = root.join("jobs/ab12cd34");
    fs::create_dir_all(&job).unwrap();
    fs::write(
        job.join("state.json"),
        format!(
            r#"{{"sessionId":"{SESSION}","linkScanPath":"{}","linkScanOffset":123,"cwd":"{cwd}","originCwd":"{cwd}"}}"#,
            store_project.join(format!("{SESSION}.jsonl")).display()
        ),
    )
    .unwrap();

    let adapter = ClaudeAdapter::with_root(&root);
    Fixture { _root_dir: root_dir, root, project_a, adapter }
}

fn only_session(adapter: &ClaudeAdapter) -> Session {
    let sessions = adapter.sessions(&SessionFilter::default()).unwrap();
    assert_eq!(sessions.len(), 1);
    sessions.into_iter().next().unwrap()
}

#[test]
fn rename_writes_the_record_claude_codes_own_rename_writes() {
    let f = fixture();
    let session = only_session(&f.adapter);
    assert_eq!(session.title.as_deref(), Some("Original title"), "starts from ai-title");

    f.adapter.rename(&session, "Renamed via asm").unwrap();
    let session = only_session(&f.adapter);
    assert_eq!(session.title.as_deref(), Some("Renamed via asm"));

    // It must be a `custom-title` record: `ai-title` is Claude Code's
    // auto-summarizer slot, and its picker ranks customTitle above it, so
    // writing the wrong slot leaves the rename invisible there.
    let transcript = fs::read_to_string(
        f.root
            .join("projects")
            .join(encode_project_dir(f.project_a.to_str().unwrap()))
            .join(format!("{SESSION}.jsonl")),
    )
    .unwrap();
    let last: serde_json::Value =
        serde_json::from_str(transcript.lines().last().unwrap()).unwrap();
    assert_eq!(last["type"], "custom-title");
    assert_eq!(last["customTitle"], "Renamed via asm");

    // A later ai-title (Claude Code re-summarizing) must not clobber it.
    let line = serde_json::json!({
        "type": "ai-title", "aiTitle": "Auto summary", "sessionId": SESSION
    });
    asm_core::fsutil::append_jsonl_line(
        &f.root
            .join("projects")
            .join(encode_project_dir(f.project_a.to_str().unwrap()))
            .join(format!("{SESSION}.jsonl")),
        &line.to_string(),
    )
    .unwrap();
    assert_eq!(
        only_session(&f.adapter).title.as_deref(),
        Some("Renamed via asm"),
        "customTitle outranks a newer aiTitle"
    );
}

#[test]
fn live_session_refuses_mutation() {
    let f = fixture();
    let sessions_dir = f.root.join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    let pid = std::process::id();
    fs::write(
        sessions_dir.join(format!("{pid}.json")),
        format!(r#"{{"pid":{pid},"sessionId":"{SESSION}"}}"#),
    )
    .unwrap();

    let session = only_session(&f.adapter);
    assert!(matches!(
        f.adapter.rename(&session, "nope"),
        Err(CoreError::SessionLive { .. })
    ));
    let dest = tempfile::tempdir().unwrap();
    assert!(matches!(
        f.adapter.relocate(&session, dest.path()),
        Err(CoreError::SessionLive { .. })
    ));
    assert!(matches!(f.adapter.delete(&session), Err(CoreError::SessionLive { .. })));
}

#[test]
fn relocate_moves_transcript_sidecar_and_rewrites_references() {
    let f = fixture();
    let session = only_session(&f.adapter);
    let old_transcript = f.root
        .join("projects")
        .join(encode_project_dir(f.project_a.to_str().unwrap()))
        .join(format!("{SESSION}.jsonl"));

    let dest_dir = tempfile::tempdir().unwrap();
    let dest = dest_dir.path().canonicalize().unwrap();
    let outcome = f.adapter.relocate(&session, &dest).unwrap();

    let enc_new = encode_project_dir(dest.to_str().unwrap());
    let new_project = f.root.join("projects").join(&enc_new);
    let new_transcript = new_project.join(format!("{SESSION}.jsonl"));
    assert_eq!(outcome.new_transcript.as_deref(), Some(new_transcript.as_path()));

    // Old files gone; new files present.
    assert!(!old_transcript.exists());
    assert!(new_transcript.is_file());
    assert!(new_project.join(SESSION).is_dir(), "sidecar dir moved");

    // Relocation marker is the last line.
    let content = fs::read_to_string(&new_transcript).unwrap();
    let last = content.lines().last().unwrap();
    assert!(last.contains(r#""type":"relocated""#));
    assert!(last.contains(dest.to_str().unwrap()));

    // The session now lists under the new project root.
    let session = only_session(&f.adapter);
    assert_eq!(session.project_root, dest);

    // Task-output symlink repointed into the new sidecar location.
    let link = new_project.join(SESSION).join("tasks/1.output");
    let target = fs::read_link(&link).unwrap();
    assert!(target.starts_with(new_project.join(SESSION)));
    assert!(target.exists(), "repointed symlink resolves");

    // Background job rehomed: path + cwd updated, byte offset untouched.
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(f.root.join("jobs/ab12cd34/state.json")).unwrap())
            .unwrap();
    assert_eq!(state["linkScanPath"], new_transcript.to_str().unwrap());
    assert_eq!(state["cwd"], dest.to_str().unwrap());
    assert_eq!(state["originCwd"], dest.to_str().unwrap());
    assert_eq!(state["linkScanOffset"], 123);

    // UUID-keyed sidecars stayed put.
    assert!(f.root.join("file-history").join(SESSION).is_dir());
}

#[test]
fn relocate_refuses_existing_destination() {
    let f = fixture();
    let session = only_session(&f.adapter);
    let dest_dir = tempfile::tempdir().unwrap();
    let dest = dest_dir.path().canonicalize().unwrap();
    let enc = encode_project_dir(dest.to_str().unwrap());
    let clash = f.root.join("projects").join(&enc);
    fs::create_dir_all(&clash).unwrap();
    fs::write(clash.join(format!("{SESSION}.jsonl")), "{}").unwrap();

    assert!(matches!(
        f.adapter.relocate(&session, &dest),
        Err(CoreError::DestinationExists { .. })
    ));
}

#[test]
fn archive_unarchive_roundtrip_and_delete_with_backup() {
    // Archive
    let f = fixture();
    let session = only_session(&f.adapter);
    let outcome = f.adapter.archive(&session).unwrap();
    let archive_dir = outcome.archived_to.unwrap();
    assert!(archive_dir.join("manifest.json").is_file());
    assert!(archive_dir.join("native/transcript.jsonl").is_file());
    assert!(archive_dir.join("native/session-dir").is_dir());
    assert!(archive_dir.join("native/file-history").is_dir());
    assert!(f.adapter.sessions(&SessionFilter::default()).unwrap().is_empty());

    // Unarchive restores everything exactly.
    let restored = unarchive_by_id(SESSION).unwrap();
    assert!(restored.is_file());
    assert!(!archive_dir.exists());
    let session = only_session(&f.adapter);
    assert_eq!(session.title.as_deref(), Some("Original title"));
    assert!(f.root.join("file-history").join(SESSION).is_dir());

    // Delete backs up all five locations then removes them.
    let report = f.adapter.delete(&session).unwrap();
    let backup = report.backup_dir.unwrap();
    assert!(backup.join("transcript.jsonl").is_file());
    assert!(backup.join("session-dir").is_dir());
    assert!(backup.join("file-history").is_dir());
    assert!(f.adapter.sessions(&SessionFilter::default()).unwrap().is_empty());
    assert!(!f.root.join("file-history").join(SESSION).exists());
    assert!(!f.root.join("tasks").join(SESSION).exists());
}
