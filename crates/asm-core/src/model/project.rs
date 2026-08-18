use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::AgentKind;

/// A project an agent has sessions for.
///
/// A project is a **repository**, not a directory. Git worktrees are
/// separate checkouts of one repository, and a session started in a
/// subdirectory is still a session about the same codebase — so all of them
/// group together under the repository that contains them, and sessions
/// from different agents in the same repository share one project.
/// Directories outside any repository stand alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    /// The repository's main worktree, or the directory itself when the
    /// path is not in a repository.
    pub root: PathBuf,
    /// The repository's common git directory — the identity every worktree
    /// of the repository agrees on. `None` outside a repository.
    pub repo: Option<PathBuf>,
    /// Which agents have sessions here.
    pub agents: Vec<AgentKind>,
    /// Every checkout of this repository, whether or not it has sessions.
    pub worktrees: Vec<ProjectWorktree>,
    pub session_count: usize,
    /// Bytes its sessions occupy, summed from them.
    #[serde(default)]
    pub size_bytes: u64,
    pub last_updated: Option<Timestamp>,
}

/// One checkout of a project's repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectWorktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    /// True for the repository's primary checkout.
    pub is_main: bool,
    /// Sessions whose working directory is inside this checkout.
    pub session_count: usize,
}

impl Project {
    /// Worktrees that actually hold sessions, most-used first.
    pub fn active_worktrees(&self) -> Vec<&ProjectWorktree> {
        let mut active: Vec<&ProjectWorktree> =
            self.worktrees.iter().filter(|w| w.session_count > 0).collect();
        active.sort_by_key(|w| std::cmp::Reverse(w.session_count));
        active
    }
}
