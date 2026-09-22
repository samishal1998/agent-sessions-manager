//! Git worktree queries via `git worktree list --porcelain` — the porcelain
//! format is stable, and shelling out avoids a libgit2 build for what is
//! (so far) a read-only need.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::CoreError;

#[derive(Debug, Clone, Serialize)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub is_main: bool,
    pub detached: bool,
    pub locked: bool,
    pub prunable: bool,
}

/// The identity of a git repository, shared by all of its worktrees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Repo {
    /// The common `.git` directory. Every worktree of a repository reports
    /// the same one, which is what makes it usable as an identity.
    pub common_dir: PathBuf,
    /// The primary checkout — the directory holding `common_dir`.
    pub main_worktree: PathBuf,
}

/// The repository's `origin` remote as a cross-machine project key:
/// `github.com/owner/repo`, whatever form the clone used.
///
/// Absolute paths differ between machines, so this is how the same project
/// is recognised on two of them. One machine may clone over SSH and another
/// over HTTPS, so both forms reduce to host and path. Userinfo is always
/// dropped — `https://<token>@host/...` remotes exist, and this value is
/// uploaded to the hub with every session.
pub fn origin(dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    normalize_remote(String::from_utf8_lossy(&output.stdout).trim())
}

fn normalize_remote(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let (host, path) = match url.split_once("://") {
        // scheme://[user[:pass]@]host[:port]/path
        Some((_, rest)) => {
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            let host = authority.rsplit('@').next().unwrap_or(authority);
            (host.split(':').next().unwrap_or(host), path)
        }
        // scp-like: [user@]host:path — but a bare local path has no host.
        None => {
            let (authority, path) = url.split_once(':')?;
            if authority.contains('/') {
                return None;
            }
            (authority.rsplit('@').next().unwrap_or(authority), path)
        }
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{path}", host.to_ascii_lowercase()))
}

/// Identify the repository containing `dir`, if any.
///
/// This is what makes a project more than a directory: linked worktrees and
/// subdirectories of one checkout all resolve to the same repository, so
/// sessions started in any of them belong together.
pub fn repo_of(dir: &Path) -> Option<Repo> {
    if !dir.is_dir() {
        return None;
    }
    let output = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let common_dir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    if common_dir.as_os_str().is_empty() {
        return None;
    }
    let common_dir = common_dir.canonicalize().unwrap_or(common_dir);
    // `<repo>/.git` → `<repo>`; a bare repository is its own worktree.
    let main_worktree = if common_dir.file_name().is_some_and(|n| n == ".git") {
        common_dir.parent().unwrap_or(&common_dir).to_path_buf()
    } else {
        common_dir.clone()
    };
    Some(Repo { common_dir, main_worktree })
}

pub fn worktrees(repo: &Path) -> Result<Vec<Worktree>, CoreError> {
    let output = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo)
        .output()
        .map_err(|e| CoreError::io(repo, e))?;
    if !output.status.success() {
        return Err(CoreError::Invalid {
            msg: format!(
                "git worktree list failed in {}: {}",
                repo.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(parse_porcelain(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_porcelain(porcelain: &str) -> Vec<Worktree> {
    let mut result = Vec::new();
    for (i, block) in porcelain.split("\n\n").enumerate() {
        let mut path = None;
        let mut branch = None;
        let mut detached = false;
        let mut locked = false;
        let mut prunable = false;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            } else if line == "detached" {
                detached = true;
            } else if line == "locked" || line.starts_with("locked ") {
                locked = true;
            } else if line == "prunable" || line.starts_with("prunable ") {
                prunable = true;
            }
        }
        if let Some(path) = path {
            result.push(Worktree { path, branch, is_main: i == 0, detached, locked, prunable });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_blocks() {
        let porcelain = "worktree /home/u/repo\nHEAD abc123\nbranch refs/heads/main\n\n\
                         worktree /home/u/repo-wt\nHEAD def456\nbranch refs/heads/feature\nlocked\n\n\
                         worktree /home/u/repo-detached\nHEAD 0123ab\ndetached\nprunable gitdir file points to non-existent location\n";
        let wts = parse_porcelain(porcelain);
        assert_eq!(wts.len(), 3);
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert!(wts[0].is_main);
        assert!(wts[1].locked);
        assert_eq!(wts[1].branch.as_deref(), Some("feature"));
        assert!(wts[2].detached);
        assert!(wts[2].prunable);
        assert!(!wts[2].is_main);
    }
}

#[cfg(test)]
mod origin_tests {
    use super::normalize_remote;

    #[test]
    fn ssh_and_https_clones_of_one_repository_agree() {
        let want = Some("github.com/owner/repo".to_string());
        assert_eq!(normalize_remote("git@github.com:owner/repo.git"), want);
        assert_eq!(normalize_remote("https://github.com/owner/repo"), want);
        assert_eq!(normalize_remote("https://github.com/owner/repo.git/"), want);
        assert_eq!(normalize_remote("ssh://git@GitHub.com:22/owner/repo.git"), want);
    }

    /// The key is uploaded with every session, so a credential embedded in
    /// the remote must never survive into it.
    #[test]
    fn credentials_never_survive() {
        let key = normalize_remote("https://x-access-token:ghp_secret@github.com/owner/repo.git");
        assert_eq!(key.as_deref(), Some("github.com/owner/repo"));
    }

    #[test]
    fn local_and_empty_remotes_are_not_a_project_key() {
        assert_eq!(normalize_remote(""), None);
        assert_eq!(normalize_remote("/srv/git/repo.git"), None);
        assert_eq!(normalize_remote("../sibling"), None);
    }
}
