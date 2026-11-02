//! Listing pi and omp sessions: one level of project folders under the
//! sessions directory, as both agents read it themselves, plus (asked for)
//! omp's subagent sessions in each session's artifact directory.

use std::path::{Path, PathBuf};

use crate::CoreError;
use crate::model::Session;

use super::super::SessionFilter;
use super::PiAdapter;

/// A session file: `.jsonl`, and not one of omp's own side files (a hidden
/// publish lock, an advisor transcript).
fn is_session(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
    name.ends_with(".jsonl") && !name.starts_with('.') && !name.starts_with("__advisor") && path.is_file()
}

fn session_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries.flatten().map(|e| e.path()).filter(|p| is_session(p)).collect()
}

pub(super) fn sessions(adapter: &PiAdapter, filter: &SessionFilter) -> Result<Vec<Session>, CoreError> {
    if filter.agent.is_some_and(|agent| agent != adapter.kind) {
        return Ok(Vec::new());
    }
    let root = adapter.sessions_dir();
    // Files right in the sessions directory too: that is the layout pi and
    // omp write when pointed at a directory of their own (`--session-dir`).
    let mut files = session_files(root);
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(session_files(&path));
            }
        }
    }

    let mut sessions = Vec::new();
    for file in files {
        let Some(session) = super::export_ir::session_from_file(adapter.kind, &file, None) else { continue };
        // Pi has no subagents of its own; omp keeps them beside the session.
        if filter.include_children && adapter.kind == crate::model::AgentKind::Omp {
            children(adapter, &file, &session.handle.native_id, &mut sessions);
        }
        sessions.push(session);
    }
    sessions.retain(|s| filter.matches(s));
    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated));
    Ok(sessions)
}

/// omp's subagents of the session in `file`: `<file stem>/<agent>.jsonl`,
/// and theirs in turn below each of those.
fn children(adapter: &PiAdapter, file: &Path, parent: &str, out: &mut Vec<Session>) {
    for child in session_files(&file.with_extension("")) {
        let Some(session) = super::export_ir::session_from_file(adapter.kind, &child, Some(parent.to_string())) else {
            continue;
        };
        let id = session.handle.native_id.clone();
        out.push(session);
        children(adapter, &child, &id, out);
    }
}
