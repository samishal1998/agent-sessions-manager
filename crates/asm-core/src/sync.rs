//! Sync groundwork over the archive store.
//!
//! The archive (`<data>/archive/<agent>/<id>/`) is deliberately a plain,
//! git-friendly directory tree: a JSON manifest plus the session's native
//! files, moved rather than transformed. That makes "sync" a thin layer —
//! version the archive with git and let the user's existing remote do the
//! transport.
//!
//! Scope here is honest: init, status, and commit. Push/pull are NOT
//! implemented; `status` tells you the exact git command to run instead of
//! pretending asm owns the transport.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::{CoreError, paths};

const GITIGNORE: &str = "\
# asm archive — everything here is session data worth versioning.
# Transient working files only:
*.tmp
.DS_Store
";

const README: &str = "\
# asm session archive

Each directory is one archived agent session:

    <agent>/<session-id>/
        manifest.json   what the session was, and where each file came from
        native/         the session's original files, moved here unchanged

`manifest.json` records the original absolute path of every entry, so
`asm unarchive <id>` restores each file exactly where its agent expects it.

This directory is a normal git repository. Commit it, push it to whatever
remote you like, and clone it on another machine. asm does not manage the
transport: use git directly.

Note that paths inside `manifest.json` are absolute and machine-specific.
Restoring on a different machine with a different home directory needs the
paths adjusted first.
";

#[derive(Debug, Serialize)]
pub struct ArchivedSession {
    pub agent: String,
    pub id: String,
    pub title: Option<String>,
    pub project_root: Option<String>,
    pub archived_at: Option<String>,
    pub bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct SyncStatus {
    pub archive_root: PathBuf,
    pub archive_exists: bool,
    pub git_initialized: bool,
    pub branch: Option<String>,
    pub remote: Option<String>,
    pub last_commit: Option<String>,
    /// Paths with uncommitted changes (staged, unstaged, or untracked).
    pub uncommitted: usize,
    pub sessions: Vec<ArchivedSession>,
    pub total_bytes: u64,
    /// Things the user should know, including what asm deliberately does
    /// not do.
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct InitOutcome {
    pub archive_root: PathBuf,
    pub created_repo: bool,
    pub remote_set: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommitOutcome {
    pub committed: bool,
    pub message: String,
    pub files_changed: usize,
}

fn archive_root() -> Result<PathBuf, CoreError> {
    paths::data_dir()
        .map(|d| d.join("archive"))
        .ok_or_else(|| CoreError::Invalid { msg: "cannot determine asm data directory".into() })
}

fn git(dir: &Path, args: &[&str]) -> Result<std::process::Output, CoreError> {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| CoreError::Invalid { msg: format!("failed to run git: {e}") })
}

fn git_stdout(dir: &Path, args: &[&str]) -> Option<String> {
    let output = git(dir, args).ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn is_git_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}

pub fn init(remote: Option<&str>) -> Result<InitOutcome, CoreError> {
    let root = archive_root()?;
    std::fs::create_dir_all(&root).map_err(|e| CoreError::io(&root, e))?;

    let created_repo = if is_git_repo(&root) {
        false
    } else {
        let output = git(&root, &["init"])?;
        if !output.status.success() {
            return Err(CoreError::Invalid {
                msg: format!(
                    "git init failed in {}: {}",
                    root.display(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        true
    };

    // Only write the scaffolding files if absent — never clobber a user's
    // edits to their own repo.
    for (name, content) in [(".gitignore", GITIGNORE), ("README.md", README)] {
        let path = root.join(name);
        if !path.exists() {
            std::fs::write(&path, content).map_err(|e| CoreError::io(&path, e))?;
        }
    }

    let mut remote_set = None;
    if let Some(url) = remote {
        let existing = git_stdout(&root, &["remote", "get-url", "origin"]);
        let args: Vec<&str> = match existing {
            Some(_) => vec!["remote", "set-url", "origin", url],
            None => vec!["remote", "add", "origin", url],
        };
        let output = git(&root, &args)?;
        if !output.status.success() {
            return Err(CoreError::Invalid {
                msg: format!(
                    "failed to set remote: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        remote_set = Some(url.to_string());
    }

    Ok(InitOutcome { archive_root: root, created_repo, remote_set })
}

pub fn status() -> Result<SyncStatus, CoreError> {
    let root = archive_root()?;
    let archive_exists = root.is_dir();
    let git_initialized = archive_exists && is_git_repo(&root);

    let mut sessions = Vec::new();
    let mut total_bytes = 0;
    if archive_exists {
        for agent_dir in read_dirs(&root)? {
            let Some(agent) = agent_dir.file_name().and_then(|n| n.to_str()) else { continue };
            if agent.starts_with('.') {
                continue;
            }
            for session_dir in read_dirs(&agent_dir)? {
                let manifest_path = session_dir.join("manifest.json");
                if !manifest_path.is_file() {
                    continue;
                }
                let manifest: serde_json::Value = std::fs::read(&manifest_path)
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .unwrap_or(serde_json::Value::Null);
                let bytes = dir_size(&session_dir);
                total_bytes += bytes;
                sessions.push(ArchivedSession {
                    agent: agent.to_string(),
                    id: session_dir
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default()
                        .to_string(),
                    title: manifest
                        .get("title")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    project_root: manifest
                        .get("project_root")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    archived_at: manifest
                        .get("archived_at")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    bytes,
                });
            }
        }
    }
    sessions.sort_by(|a, b| b.archived_at.cmp(&a.archived_at));

    let mut notes = Vec::new();
    let (branch, remote, last_commit, uncommitted) = if git_initialized {
        let porcelain = git_stdout(&root, &["status", "--porcelain"]).unwrap_or_default();
        let uncommitted = porcelain.lines().filter(|l| !l.trim().is_empty()).count();
        let remote = git_stdout(&root, &["remote", "get-url", "origin"]);
        if remote.is_some() {
            notes.push(format!(
                "push with: git -C {} push -u origin HEAD   (asm does not manage transport)",
                root.display()
            ));
        } else {
            notes.push(
                "no remote configured — set one with `asm sync init --remote <url>`".to_string(),
            );
        }
        (
            git_stdout(&root, &["rev-parse", "--abbrev-ref", "HEAD"]),
            remote,
            git_stdout(&root, &["log", "-1", "--format=%h %s (%ar)"]),
            uncommitted,
        )
    } else {
        notes.push("archive is not a git repository yet — run `asm sync init`".to_string());
        (None, None, None, 0)
    };

    notes.push(
        "manifest paths are absolute; restoring on another machine needs them adjusted"
            .to_string(),
    );

    Ok(SyncStatus {
        archive_root: root,
        archive_exists,
        git_initialized,
        branch,
        remote,
        last_commit,
        uncommitted,
        sessions,
        total_bytes,
        notes,
    })
}

pub fn commit(message: &str) -> Result<CommitOutcome, CoreError> {
    let root = archive_root()?;
    if !is_git_repo(&root) {
        return Err(CoreError::Invalid {
            msg: "archive is not a git repository — run `asm sync init` first".into(),
        });
    }
    let porcelain = git_stdout(&root, &["status", "--porcelain"]).unwrap_or_default();
    let files_changed = porcelain.lines().filter(|l| !l.trim().is_empty()).count();
    if files_changed == 0 {
        return Ok(CommitOutcome {
            committed: false,
            message: "nothing to commit".to_string(),
            files_changed: 0,
        });
    }

    let add = git(&root, &["add", "-A"])?;
    if !add.status.success() {
        return Err(CoreError::Invalid {
            msg: format!("git add failed: {}", String::from_utf8_lossy(&add.stderr).trim()),
        });
    }
    let output = git(&root, &["commit", "-m", message])?;
    if !output.status.success() {
        return Err(CoreError::Invalid {
            msg: format!(
                "git commit failed: {}{}",
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(CommitOutcome { committed: true, message: message.to_string(), files_changed })
}

/// Archived sessions rebuilt as `Session` values pointing at their archived
/// files, so they can still be read (and searched) without restoring them.
/// They are deliberately absent from `asm list`, which reflects what the
/// agents themselves can see.
pub fn archived_sessions() -> Result<Vec<crate::model::Session>, CoreError> {
    use crate::model::{AgentKind, Session, SessionLocation, SessionRef, SessionStatus, Usage};

    let root = archive_root()?;
    let mut sessions = Vec::new();
    for agent_dir in read_dirs(&root)? {
        let Some(agent_name) = agent_dir.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(agent) = AgentKind::parse(agent_name) else { continue };

        for session_dir in read_dirs(&agent_dir)? {
            let Ok(bytes) = std::fs::read(session_dir.join("manifest.json")) else { continue };
            let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                continue;
            };
            let Some(id) = manifest.get("id").and_then(|v| v.as_str()) else { continue };

            // The transcript keeps its manifest name inside `native/`.
            let transcript = session_dir.join("native/transcript.jsonl");
            if !transcript.is_file() {
                continue;
            }
            let archived_at = manifest
                .get("archived_at")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<jiff::Timestamp>().ok());

            sessions.push(Session {
                handle: SessionRef {
                    agent,
                    native_id: id.to_string(),
                    location: SessionLocation::JsonlFile { path: transcript },
                },
                title: manifest.get("title").and_then(|v| v.as_str()).map(str::to_string),
                slug: None,
                project_root: manifest
                    .get("project_root")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .unwrap_or_default(),
                git_branch: None,
                created: None,
                updated: archived_at,
                model: None,
                usage: Usage::default(),
                status: SessionStatus::Archived,
                parent: None,
                agent_version: None,
            });
        }
    }
    Ok(sessions)
}

fn read_dirs(dir: &Path) -> Result<Vec<PathBuf>, CoreError> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return Ok(out) };
    for entry in entries.filter_map(Result::ok) {
        if entry.path().is_dir() {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    for entry in entries.filter_map(Result::ok) {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}
