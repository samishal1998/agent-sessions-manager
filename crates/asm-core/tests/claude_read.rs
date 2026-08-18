//! Claude Code read-adapter tests against a synthetic fixture store that
//! reproduces the documented traps: last-writer-wins ai-title, the
//! snake_case session_id decoy, relocation markers, memory/ and sidecar-dir
//! siblings, torn tail lines, and PID-keyed liveness records.

use std::fs;
use std::path::Path;

use asm_core::adapter::claude::ClaudeAdapter;
use asm_core::adapter::{AgentRead, SessionFilter};
use asm_core::model::{SessionLocation, SessionStatus};

const SESSION_A: &str = "aaaaaaaa-1111-2222-3333-444444444444";
const SESSION_B: &str = "bbbbbbbb-1111-2222-3333-444444444444";
const DECOY_ID: &str = "dddddddd-9999-8888-7777-666666666666";

fn envelope(session_id: &str, uuid: &str, ts: &str, cwd: &str) -> String {
    format!(
        r#""uuid":"{uuid}","parentUuid":null,"isSidechain":false,"timestamp":"{ts}","cwd":"{cwd}","sessionId":"{session_id}","version":"2.1.230","gitBranch":"main","slug":"alpha-work""#
    )
}

fn write_store(root: &Path) {
    let project = root.join("projects/-home-user-projects-alpha");
    fs::create_dir_all(&project).unwrap();

    // Session A: state records first, conversation records, a decoy
    // session_id, two ai-titles (last must win), and a torn final line.
    let user1 = format!(
        "{{{},\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}",
        envelope(SESSION_A, "u1", "2026-08-10T10:00:00.000Z", "/home/user/projects/alpha")
    );
    let assistant1 = format!(
        "{{{},\"session_id\":\"{DECOY_ID}\",\"type\":\"assistant\",\"message\":{{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-fable-5\",\"content\":[{{\"type\":\"text\",\"text\":\"hi\"}}]}}}}",
        envelope(SESSION_A, "a1", "2026-08-10T10:00:05.000Z", "/home/user/projects/alpha")
    );
    let transcript_a = [
        format!(r#"{{"type":"mode","mode":"normal","sessionId":"{SESSION_A}"}}"#),
        format!(r#"{{"type":"ai-title","aiTitle":"First title","sessionId":"{SESSION_A}"}}"#),
        user1,
        assistant1,
        format!(r#"{{"type":"ai-title","aiTitle":"Final title","sessionId":"{SESSION_A}"}}"#),
        format!(
            r#"{{"type":"last-prompt","lastPrompt":"do x","leafUuid":"a1","sessionId":"{SESSION_A}"}}"#
        ),
        // torn tail: live transcripts end mid-line; must be tolerated
        r#"{"type":"user","uuid":"u2","timesta"#.to_string(),
    ]
    .join("\n");
    fs::write(project.join(format!("{SESSION_A}.jsonl")), transcript_a).unwrap();

    // Session B: relocated marker must override every record cwd.
    let user_b = format!(
        "{{{},\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"start\"}}}}",
        envelope(SESSION_B, "u1", "2026-08-11T09:00:00.000Z", "/home/user/projects/alpha")
    );
    let transcript_b = [
        user_b,
        format!(
            r#"{{"type":"relocated","sessionId":"{SESSION_B}","relocatedCwd":"/home/user/worktrees/alpha-wt"}}"#
        ),
    ]
    .join("\n");
    fs::write(project.join(format!("{SESSION_B}.jsonl")), transcript_b).unwrap();

    // Siblings that must never be mistaken for sessions:
    fs::create_dir_all(project.join("memory")).unwrap();
    fs::write(project.join("memory/MEMORY.md"), "# notes\n").unwrap();
    let sidecar = project.join(SESSION_A).join("subagents");
    fs::create_dir_all(&sidecar).unwrap();
    fs::write(sidecar.join("agent-1.jsonl"), "{}\n").unwrap();
    fs::write(project.join("notes.jsonl"), "{}\n").unwrap();
    fs::write(project.join("README.md"), "hi\n").unwrap();
}

fn adapter(root: &Path) -> ClaudeAdapter {
    ClaudeAdapter::with_root(root)
}

#[test]
fn lists_sessions_with_correct_metadata() {
    let dir = tempfile::tempdir().unwrap();
    write_store(dir.path());
    let sessions = adapter(dir.path()).sessions(&SessionFilter::default()).unwrap();

    assert_eq!(sessions.len(), 2, "exactly the two real transcripts");

    let a = sessions.iter().find(|s| s.handle.native_id == SESSION_A).unwrap();
    assert_eq!(a.title.as_deref(), Some("Final title"), "last ai-title wins");
    assert_eq!(a.project_root, Path::new("/home/user/projects/alpha"));
    assert_eq!(a.git_branch.as_deref(), Some("main"));
    assert_eq!(a.slug.as_deref(), Some("alpha-work"));
    assert_eq!(a.model.as_deref(), Some("claude-fable-5"));
    assert_eq!(a.agent_version.as_deref(), Some("2.1.230"));
    assert_eq!(a.status, SessionStatus::Idle);
    assert_eq!(a.created.unwrap().to_string(), "2026-08-10T10:00:00Z");
    assert_eq!(a.updated.unwrap().to_string(), "2026-08-10T10:00:05Z");
    match &a.handle.location {
        SessionLocation::JsonlFile { path } => {
            assert_eq!(path.file_stem().unwrap().to_str().unwrap(), SESSION_A);
        }
        other => panic!("unexpected location {other:?}"),
    }
}

#[test]
fn identity_is_filename_stem_never_snake_case_session_id() {
    let dir = tempfile::tempdir().unwrap();
    write_store(dir.path());
    let sessions = adapter(dir.path()).sessions(&SessionFilter::default()).unwrap();

    assert!(sessions.iter().any(|s| s.handle.native_id == SESSION_A));
    assert!(
        sessions.iter().all(|s| s.handle.native_id != DECOY_ID),
        "snake_case session_id must never become an identity"
    );
}

#[test]
fn relocated_marker_overrides_record_cwd() {
    let dir = tempfile::tempdir().unwrap();
    write_store(dir.path());
    let sessions = adapter(dir.path()).sessions(&SessionFilter::default()).unwrap();

    let b = sessions.iter().find(|s| s.handle.native_id == SESSION_B).unwrap();
    assert_eq!(b.project_root, Path::new("/home/user/worktrees/alpha-wt"));
}

#[test]
fn project_filter_narrows_results() {
    let dir = tempfile::tempdir().unwrap();
    write_store(dir.path());
    let filter = SessionFilter {
        project: Some("/home/user/worktrees/alpha-wt".into()),
        ..SessionFilter::default()
    };
    let sessions = adapter(dir.path()).sessions(&filter).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].handle.native_id, SESSION_B);
}

#[test]
fn projects_group_by_authoritative_cwd() {
    let dir = tempfile::tempdir().unwrap();
    write_store(dir.path());
    let sessions = adapter(dir.path()).sessions(&SessionFilter::default()).unwrap();
    // Fixture paths are not real repositories, so each authoritative cwd
    // stands alone — which is the point: grouping follows the relocated
    // marker, not the encoded directory name.
    let projects = asm_core::ops::group_projects(&sessions, |_| None, |_| Vec::new());

    assert_eq!(projects.len(), 2, "alpha + relocated worktree");
    let alpha = projects
        .iter()
        .find(|p| p.root == Path::new("/home/user/projects/alpha"))
        .unwrap();
    assert_eq!(alpha.session_count, 1);
    assert!(alpha.repo.is_none());
}

#[test]
fn doctor_finds_duplicate_session_ids_across_projects() {
    let dir = tempfile::tempdir().unwrap();
    write_store(dir.path());
    // Duplicate SESSION_A's transcript into a second project dir — the
    // resume-poisoning scenario.
    let other = dir.path().join("projects/-home-user-projects-beta");
    fs::create_dir_all(&other).unwrap();
    fs::copy(
        dir.path()
            .join("projects/-home-user-projects-alpha")
            .join(format!("{SESSION_A}.jsonl")),
        other.join(format!("{SESSION_A}.jsonl")),
    )
    .unwrap();

    let duplicates = adapter(dir.path()).duplicate_session_ids().unwrap();
    assert_eq!(duplicates.len(), 1);
    assert_eq!(duplicates[0].0, SESSION_A);
    assert_eq!(duplicates[0].1.len(), 2);
}

#[test]
fn live_pid_record_marks_session_live() {
    let dir = tempfile::tempdir().unwrap();
    write_store(dir.path());

    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    let own_pid = std::process::id(); // guaranteed alive
    fs::write(
        sessions_dir.join(format!("{own_pid}.json")),
        format!(r#"{{"pid":{own_pid},"sessionId":"{SESSION_A}","status":"idle"}}"#),
    )
    .unwrap();
    // A dead process must not mark anything live (max Linux pid is 4194304).
    fs::write(
        sessions_dir.join("9999999.json"),
        format!(r#"{{"pid":9999999,"sessionId":"{SESSION_B}","status":"idle"}}"#),
    )
    .unwrap();

    let sessions = adapter(dir.path()).sessions(&SessionFilter::default()).unwrap();
    let a = sessions.iter().find(|s| s.handle.native_id == SESSION_A).unwrap();
    assert_eq!(a.status, SessionStatus::Live { pid: Some(own_pid) });
    let b = sessions.iter().find(|s| s.handle.native_id == SESSION_B).unwrap();
    assert_eq!(b.status, SessionStatus::Idle);
}
