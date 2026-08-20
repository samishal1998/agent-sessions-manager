//! High-level operations shared by every frontend (CLI, TUI, web).
//! Frontends never touch adapters or stores directly.

use std::path::{Path, PathBuf};

use crate::CoreError;
use crate::adapter::{
    Adapter, AgentRead, AgentWrite, ArchiveOutcome, DeleteReport, RelocateOutcome, SessionFilter,
};
use crate::model::{AgentKind, Project, ProjectWorktree, Session};

pub fn list_sessions(filter: &SessionFilter) -> Result<Vec<Session>, CoreError> {
    let mut sessions = Vec::new();
    for adapter in Adapter::available() {
        sessions.extend(adapter.sessions(filter)?);
    }
    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated));
    Ok(sessions)
}

pub fn list_projects() -> Result<Vec<Project>, CoreError> {
    let sessions = list_sessions(&SessionFilter::default())?;
    Ok(group_projects(&sessions, crate::git::repo_of, |root| {
        crate::git::worktrees(root).unwrap_or_default()
    }))
}

/// Group sessions into projects by the repository that contains them.
///
/// Split out with its resolvers injected so the grouping rules can be
/// tested without real repositories on disk. `repo_of` identifies the
/// repository containing a directory; `worktrees_of` lists a repository's
/// checkouts given its main worktree.
pub fn group_projects(
    sessions: &[Session],
    repo_of: impl Fn(&Path) -> Option<crate::git::Repo>,
    worktrees_of: impl Fn(&Path) -> Vec<crate::git::Worktree>,
) -> Vec<Project> {
    use std::collections::HashMap;

    // Resolving a repository shells out to git, so do it once per distinct
    // directory rather than once per session.
    let mut repo_cache: HashMap<PathBuf, Option<crate::git::Repo>> = HashMap::new();
    let mut groups: HashMap<PathBuf, Vec<&Session>> = HashMap::new();
    let mut repos: HashMap<PathBuf, Option<crate::git::Repo>> = HashMap::new();

    for session in sessions {
        let dir = session.project_root.clone();
        let repo = repo_cache.entry(dir.clone()).or_insert_with(|| repo_of(&dir)).clone();
        // Identity: the repository's common dir, or the bare directory.
        let key = repo.as_ref().map_or(dir.clone(), |r| r.common_dir.clone());
        groups.entry(key.clone()).or_default().push(session);
        repos.entry(key).or_insert(repo);
    }

    let mut projects: Vec<Project> = groups
        .into_iter()
        .map(|(key, group)| {
            let repo = repos.get(&key).cloned().flatten();
            let root = repo
                .as_ref()
                .map_or_else(|| key.clone(), |r| r.main_worktree.clone());

            let mut agents: Vec<AgentKind> = group.iter().map(|s| s.handle.agent).collect();
            agents.sort_by_key(|a| a.as_str());
            agents.dedup();

            // Every checkout git knows about, so an empty worktree is still
            // visible, plus any session directory git did not list.
            let mut worktrees: Vec<ProjectWorktree> = if repo.is_some() {
                worktrees_of(&root)
                    .into_iter()
                    .map(|w| ProjectWorktree {
                        path: w.path,
                        branch: w.branch,
                        is_main: w.is_main,
                        session_count: 0,
                    })
                    .collect()
            } else {
                Vec::new()
            };
            if worktrees.is_empty() {
                worktrees.push(ProjectWorktree {
                    path: root.clone(),
                    branch: None,
                    is_main: true,
                    session_count: 0,
                });
            }

            for session in &group {
                // A session in a subdirectory belongs to the checkout that
                // contains it — the longest matching path, so nested
                // worktrees attribute to the innermost one.
                let best = worktrees
                    .iter_mut()
                    .filter(|w| session.project_root.starts_with(&w.path))
                    .max_by_key(|w| w.path.as_os_str().len());
                match best {
                    Some(worktree) => worktree.session_count += 1,
                    None => worktrees.push(ProjectWorktree {
                        path: session.project_root.clone(),
                        branch: session.git_branch.clone(),
                        is_main: false,
                        session_count: 1,
                    }),
                }
            }

            Project {
                root,
                repo: repo.map(|r| r.common_dir),
                agents,
                session_count: group.len(),
                size_bytes: group.iter().filter_map(|s| s.size_bytes).sum(),
                last_updated: group.iter().filter_map(|s| s.updated).max(),
                worktrees,
            }
        })
        .collect();

    projects.sort_by_key(|p| std::cmp::Reverse(p.last_updated));
    projects
}

/// Health report for `asm doctor`.
#[derive(Debug, serde::Serialize)]
pub struct DoctorReport {
    pub agents: Vec<AgentHealth>,
}

#[derive(Debug, serde::Serialize)]
pub struct AgentHealth {
    pub agent: crate::model::AgentKind,
    pub store_root: std::path::PathBuf,
    pub store_found: bool,
    pub session_count: usize,
    /// Claude only: ids whose transcript exists in >1 project dir, which
    /// poisons cross-project `--resume`.
    pub duplicate_session_ids: Vec<String>,
    pub warnings: Vec<String>,
    /// What this adapter can actually do, so a UI can offer only those
    /// verbs rather than letting the user discover the answer by error.
    pub capabilities: crate::adapter::Capabilities,
}

pub fn doctor() -> Result<DoctorReport, CoreError> {
    let mut agents = Vec::new();
    for adapter in Adapter::available() {
        let detect = adapter.detect();
        let session_count = adapter.sessions(&SessionFilter::default())?.len();
        let mut duplicate_session_ids = Vec::new();
        let mut warnings = Vec::new();
        match &adapter {
            Adapter::ClaudeCode(claude) => {
                for (id, paths) in claude.duplicate_session_ids()? {
                    warnings.push(format!(
                        "session {id} exists in {} project dirs — cross-project --resume \
                         will fail for it",
                        paths.len()
                    ));
                    duplicate_session_ids.push(id);
                }
            }
            // jcode keeps one snapshot per session id in one directory, so
            // it has neither Claude's duplicate-id hazard nor OpenCode's
            // single-writer lock.
            Adapter::JCode(_) => {}
            // Codex keeps one rollout per thread id and asm never writes to
            // it, so there is no divergence for doctor to find.
            Adapter::Codex(_) => {}
            Adapter::OpenCode(opencode) => {
                for lock in opencode.locks() {
                    if lock.held {
                        warnings.push(format!(
                            "store is in use ({}); mutations will be refused until it exits",
                            lock.reason
                        ));
                    } else {
                        warnings.push(format!(
                            "stale lock ({}) — it does not block asm, and you can remove it: {}",
                            lock.reason,
                            lock.path.display()
                        ));
                    }
                }
            }
        }
        agents.push(AgentHealth {
            agent: detect.agent,
            store_root: detect.store_root,
            store_found: detect.store_found,
            session_count,
            duplicate_session_ids,
            warnings,
            capabilities: adapter.capabilities(),
        });
    }
    Ok(DoctorReport { agents })
}

/// The command that resumes `session` in its native agent.
pub fn resume_command(session: &Session) -> Result<std::process::Command, ResolveError> {
    for adapter in Adapter::available() {
        if adapter.kind() == session.handle.agent
            && let Some(cmd) = adapter.resume_command(session)
        {
            return Ok(cmd);
        }
    }
    Err(ResolveError::NotFound { query: session.handle.native_id.clone() })
}

/// The adapter that owns a session's store, for callers outside `ops`.
pub fn adapter_for(agent: AgentKind) -> Option<Adapter> {
    Adapter::available().into_iter().find(|a| a.kind() == agent)
}

fn owning_adapter(session: &Session) -> Result<Adapter, CoreError> {
    Adapter::available()
        .into_iter()
        .find(|a| a.kind() == session.handle.agent)
        .ok_or(CoreError::StoreNotFound { path: PathBuf::new() })
}

pub fn rename(session: &Session, title: &str) -> Result<(), CoreError> {
    owning_adapter(session)?.rename(session, title)
}

pub fn archive(session: &Session) -> Result<ArchiveOutcome, CoreError> {
    owning_adapter(session)?.archive(session)
}

/// Unarchive by user-supplied reference. OpenCode sessions unarchive in
/// place (clearing `time_archived`); Claude sessions are restored from our
/// archive store by id.
pub fn unarchive(query: &str, filter: &SessionFilter) -> Result<Option<PathBuf>, CoreError> {
    match resolve_ref(query, filter) {
        Ok(session) => {
            owning_adapter(&session)?.unarchive(&session)?;
            Ok(None)
        }
        Err(ResolveError::Core(e)) => Err(e),
        Err(ResolveError::Ambiguous { query, candidates }) => Err(CoreError::Invalid {
            msg: format!("'{query}' is ambiguous; matches: {}", candidates.join(", ")),
        }),
        // Not resolvable as a live session — try the Claude archive store.
        Err(ResolveError::NotFound { .. }) => {
            let id_query = query.split_once(':').map(|(_, rest)| rest).unwrap_or(query);
            crate::archive::unarchive_by_id(id_query).map(Some)
        }
    }
}

pub fn relocate(session: &Session, new_dir: &Path) -> Result<RelocateOutcome, CoreError> {
    owning_adapter(session)?.relocate(session, new_dir)
}

pub fn delete(session: &Session) -> Result<DeleteReport, CoreError> {
    owning_adapter(session)?.delete(session)
}

pub fn export_ir(session: &Session) -> Result<crate::ir::IrSession, CoreError> {
    owning_adapter(session)?.export_ir(session)
}

pub fn import(
    session: &Session,
    target: AgentKind,
    opts: &crate::import::ImportOpts,
) -> Result<crate::import::ImportOutcome, CoreError> {
    crate::import::import(session, target, opts)
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("no session matches '{query}'")]
    NotFound { query: String },
    #[error("'{query}' is ambiguous; matches: {}", candidates.join(", "))]
    Ambiguous { query: String, candidates: Vec<String> },
    #[error(transparent)]
    Core(#[from] CoreError),
}

/// Resolve a user-supplied session reference: a native id, a unique id
/// prefix, or `agent:id-prefix`.
pub fn resolve_ref(query: &str, filter: &SessionFilter) -> Result<Session, ResolveError> {
    let mut filter = filter.clone();
    let id_query = match query.split_once(':') {
        Some((agent, rest)) if AgentKind::parse(agent).is_some() => {
            filter.agent = AgentKind::parse(agent);
            rest
        }
        _ => query,
    };

    let sessions = list_sessions(&filter)?;
    // An id prefix, or the memorable name the agent itself uses — jcode
    // calls a session "hog", never `session_hog_1787…`, and its own
    // `--resume` takes that name.
    let mut matches: Vec<Session> = sessions
        .into_iter()
        .filter(|s| {
            s.handle.native_id.starts_with(id_query)
                || s.slug.as_deref().is_some_and(|slug| slug == id_query)
        })
        .collect();

    match matches.len() {
        0 => Err(ResolveError::NotFound { query: query.to_string() }),
        1 => Ok(matches.remove(0)),
        _ => Err(ResolveError::Ambiguous {
            query: query.to_string(),
            candidates: matches
                .iter()
                .map(|s| format!("{}:{}", s.handle.agent, s.handle.native_id))
                .collect(),
        }),
    }
}
