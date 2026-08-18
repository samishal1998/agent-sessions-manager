//! High-level operations shared by every frontend (CLI, TUI, web).
//! Frontends never touch adapters or stores directly.

use std::path::{Path, PathBuf};

use crate::CoreError;
use crate::adapter::{
    Adapter, AgentRead, AgentWrite, ArchiveOutcome, DeleteReport, RelocateOutcome, SessionFilter,
};
use crate::model::{AgentKind, Project, Session};

pub fn list_sessions(filter: &SessionFilter) -> Result<Vec<Session>, CoreError> {
    let mut sessions = Vec::new();
    for adapter in Adapter::available() {
        sessions.extend(adapter.sessions(filter)?);
    }
    sessions.sort_by(|a, b| b.updated.cmp(&a.updated));
    Ok(sessions)
}

pub fn list_projects() -> Result<Vec<Project>, CoreError> {
    let mut projects = Vec::new();
    for adapter in Adapter::available() {
        projects.extend(adapter.projects()?);
    }
    projects.sort_by(|a, b| b.last_updated.cmp(&a.last_updated));
    Ok(projects)
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
            crate::adapter::claude::unarchive_by_id(id_query).map(Some)
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
    let mut matches: Vec<Session> = sessions
        .into_iter()
        .filter(|s| s.handle.native_id.starts_with(id_query))
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
