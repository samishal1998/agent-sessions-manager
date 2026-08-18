//! Project grouping: a project is a repository, not a directory.
//!
//! The resolvers are injected, so these exercise the grouping rules without
//! creating real repositories on disk.

use std::path::{Path, PathBuf};

use asm_core::git::{Repo, Worktree};
use asm_core::model::{
    AgentKind, Session, SessionLocation, SessionRef, SessionStatus, Usage,
};
use asm_core::ops::group_projects;

fn session(agent: AgentKind, id: &str, cwd: &str) -> Session {
    Session {
        handle: SessionRef {
            agent,
            native_id: id.to_string(),
            location: SessionLocation::JsonlFile { path: PathBuf::from("/t/x.jsonl") },
        },
        title: Some(id.to_string()),
        slug: None,
        project_root: PathBuf::from(cwd),
        git_branch: None,
        created: None,
        updated: "2026-08-01T10:00:00Z".parse().ok(),
        model: None,
        usage: Usage::default(),
        status: SessionStatus::Idle,
        parent: None,
        agent_version: None,
        size_bytes: None,
    }
}

/// Everything under /repo (including its linked worktree at /wt) belongs to
/// one repository; anything else is not in a repository at all.
fn repo_of(dir: &Path) -> Option<Repo> {
    let d = dir.to_string_lossy().to_string();
    (d.starts_with("/repo") || d.starts_with("/wt")).then(|| Repo {
        common_dir: PathBuf::from("/repo/.git"),
        main_worktree: PathBuf::from("/repo"),
    })
}

fn worktree(path: &str, branch: &str, is_main: bool) -> Worktree {
    Worktree {
        path: PathBuf::from(path),
        branch: Some(branch.to_string()),
        is_main,
        detached: false,
        locked: false,
        prunable: false,
    }
}

fn worktrees_of(_root: &Path) -> Vec<Worktree> {
    vec![worktree("/repo", "main", true), worktree("/wt", "feature", false)]
}

#[test]
fn worktrees_of_one_repository_are_one_project() {
    let sessions = vec![
        session(AgentKind::ClaudeCode, "a", "/repo"),
        session(AgentKind::ClaudeCode, "b", "/wt"),
    ];
    let projects = group_projects(&sessions, repo_of, worktrees_of);

    assert_eq!(projects.len(), 1, "a linked worktree is not its own project");
    let project = &projects[0];
    assert_eq!(project.root, Path::new("/repo"), "named by the main worktree");
    assert_eq!(project.repo.as_deref(), Some(Path::new("/repo/.git")));
    assert_eq!(project.session_count, 2);

    let counts: Vec<(String, usize)> = project
        .worktrees
        .iter()
        .map(|w| (w.path.display().to_string(), w.session_count))
        .collect();
    assert_eq!(counts, vec![("/repo".into(), 1), ("/wt".into(), 1)]);
}

#[test]
fn a_session_in_a_subdirectory_belongs_to_its_worktree() {
    // This is the everyday case: the agent's cwd wandered into a crate or
    // package directory. It is still the same project.
    let sessions = vec![
        session(AgentKind::ClaudeCode, "a", "/repo"),
        session(AgentKind::ClaudeCode, "b", "/repo/crates/web/frontend"),
        session(AgentKind::ClaudeCode, "c", "/wt/src"),
    ];
    let projects = group_projects(&sessions, repo_of, worktrees_of);

    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].session_count, 3);
    let main = projects[0].worktrees.iter().find(|w| w.is_main).unwrap();
    let linked = projects[0].worktrees.iter().find(|w| !w.is_main).unwrap();
    assert_eq!(main.session_count, 2, "the subdirectory counts against its worktree");
    assert_eq!(linked.session_count, 1);
}

#[test]
fn agents_sharing_a_repository_share_a_project() {
    let sessions = vec![
        session(AgentKind::ClaudeCode, "a", "/repo"),
        session(AgentKind::OpenCode, "b", "/repo"),
        session(AgentKind::OpenCode, "c", "/wt"),
    ];
    let projects = group_projects(&sessions, repo_of, worktrees_of);

    assert_eq!(projects.len(), 1, "a project is a repository, not a repository per agent");
    assert_eq!(projects[0].agents, vec![AgentKind::ClaudeCode, AgentKind::OpenCode]);
    assert_eq!(projects[0].session_count, 3);
}

#[test]
fn directories_outside_a_repository_stand_alone() {
    let sessions = vec![
        session(AgentKind::ClaudeCode, "a", "/repo"),
        session(AgentKind::ClaudeCode, "b", "/tmp/scratch"),
        session(AgentKind::ClaudeCode, "c", "/tmp/other"),
    ];
    let projects = group_projects(&sessions, repo_of, worktrees_of);

    assert_eq!(projects.len(), 3);
    let loose = projects.iter().find(|p| p.root == Path::new("/tmp/scratch")).unwrap();
    assert!(loose.repo.is_none());
    assert_eq!(loose.worktrees.len(), 1, "a plain directory is its own single worktree");
    assert_eq!(loose.worktrees[0].session_count, 1);
}

#[test]
fn worktrees_without_sessions_are_still_listed() {
    let sessions = vec![session(AgentKind::ClaudeCode, "a", "/repo")];
    let projects = group_projects(&sessions, repo_of, worktrees_of);

    assert_eq!(projects[0].worktrees.len(), 2, "the empty checkout is still part of the project");
    assert_eq!(projects[0].active_worktrees().len(), 1, "but it is not active");
}

#[test]
fn a_session_in_an_unlisted_directory_is_not_dropped() {
    // git did not report this checkout (pruned, or listed after the scan);
    // the session must still be counted somewhere.
    let sessions = vec![session(AgentKind::ClaudeCode, "a", "/repo-elsewhere")];
    let projects = group_projects(
        &sessions,
        |_| Some(Repo { common_dir: "/repo/.git".into(), main_worktree: "/repo".into() }),
        |_| vec![worktree("/repo", "main", true)],
    );

    assert_eq!(projects[0].session_count, 1);
    let total: usize = projects[0].worktrees.iter().map(|w| w.session_count).sum();
    assert_eq!(total, 1, "every session is attributed to exactly one worktree");
    assert!(projects[0].worktrees.iter().any(|w| w.path == Path::new("/repo-elsewhere")));
}

#[test]
fn nested_worktrees_attribute_to_the_innermost() {
    let sessions = vec![session(AgentKind::ClaudeCode, "a", "/repo/nested/src")];
    let projects = group_projects(
        &sessions,
        |_| Some(Repo { common_dir: "/repo/.git".into(), main_worktree: "/repo".into() }),
        |_| vec![worktree("/repo", "main", true), worktree("/repo/nested", "inner", false)],
    );

    let inner = projects[0].worktrees.iter().find(|w| w.path == Path::new("/repo/nested")).unwrap();
    let outer = projects[0].worktrees.iter().find(|w| w.path == Path::new("/repo")).unwrap();
    assert_eq!(inner.session_count, 1);
    assert_eq!(outer.session_count, 0);
}
