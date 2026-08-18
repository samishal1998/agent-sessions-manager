//! Listing jcode sessions without reading their conversations.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Deserialize;

use crate::CoreError;
use crate::model::{
    AgentKind, Session, SessionLocation, SessionRef, SessionStatus, Usage,
};

use super::super::SessionFilter;
use super::JCodeAdapter;

/// The snapshot's scalar fields. `messages` is deserialized into
/// `IgnoredAny`, so serde walks the conversation without materializing it —
/// the metadata we need (`working_dir`, `short_name`, `status`) is
/// serialized after the messages, so there is no head-only shortcut.
#[derive(Deserialize)]
struct Snapshot {
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    custom_title: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    /// Never read — its only job is to make serde walk the conversation
    /// instead of building it, so the fields after it can be reached.
    #[allow(dead_code)]
    #[serde(default)]
    messages: serde::de::IgnoredAny,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    short_name: Option<String>,
    #[serde(default)]
    is_debug: bool,
}

pub(super) fn sessions(
    adapter: &JCodeAdapter,
    filter: &SessionFilter,
) -> Result<Vec<Session>, CoreError> {
    if let Some(agent) = filter.agent
        && agent != AgentKind::JCode
    {
        return Ok(Vec::new());
    }

    let dir = adapter.sessions_dir();
    if !dir.is_dir() {
        return Err(CoreError::StoreNotFound { path: dir });
    }
    let live = live_pids(adapter.root());

    let mut sessions = Vec::new();
    let entries = std::fs::read_dir(&dir).map_err(|e| CoreError::io(&dir, e))?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(id) = snapshot_session_id(&path) else {
            continue; // journals, backups, anything else
        };
        let Some(snapshot) = read_snapshot(&path) else {
            continue; // unreadable or truncated mid-write
        };
        // Debug/test sessions are jcode's own scaffolding, hidden like
        // subagent sessions are elsewhere.
        if snapshot.is_debug && !filter.include_children {
            continue;
        }
        if snapshot.parent_id.is_some() && !filter.include_children {
            continue;
        }

        let status = if let Some(pid) = live.get(&id) {
            SessionStatus::Live { pid: Some(*pid) }
        } else {
            SessionStatus::Idle
        };

        let session = Session {
            handle: SessionRef {
                agent: AgentKind::JCode,
                native_id: id,
                location: SessionLocation::JsonlFile { path: path.clone() },
            },
            // jcode's own rename writes `custom_title`, which takes
            // precedence over the generated `title`.
            title: snapshot.custom_title.or(snapshot.title),
            // The memorable short name ("fox") is what `--resume` takes and
            // what jcode shows, so it lands in the slug slot.
            slug: snapshot.short_name,
            project_root: snapshot.working_dir.map(PathBuf::from).unwrap_or_default(),
            git_branch: None,
            created: snapshot.created_at.as_deref().and_then(parse_time),
            updated: snapshot.updated_at.as_deref().and_then(parse_time),
            model: snapshot.model,
            usage: Usage::default(),
            status,
            parent: snapshot.parent_id,
            agent_version: None,
        };
        if session.project_root.as_os_str().is_empty() {
            continue; // nothing to group it under
        }
        if filter.matches(&session) {
            sessions.push(session);
        }
    }

    sessions.sort_by(|a, b| b.updated.cmp(&a.updated));
    Ok(sessions)
}

/// `<id>.json`, but not `<id>.journal.jsonl` or a backup.
fn snapshot_session_id(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    if path.extension().and_then(|e| e.to_str()) != Some("json") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    // `<id>.journal.jsonl` has extension `jsonl`, but a `.journal.json`
    // would slip through on stem alone.
    (!stem.ends_with(".journal") && !stem.is_empty()).then(|| stem.to_string())
}

fn read_snapshot(path: &Path) -> Option<Snapshot> {
    let file = File::open(path).ok()?;
    // Streamed, not slurped: the document holds the whole conversation.
    serde_json::from_reader(BufReader::with_capacity(64 * 1024, file)).ok()
}

/// jcode writes one file per running session under `active_pids/`, named by
/// session id, containing the owning PID. A file whose process is gone is
/// stale and must not mark the session live.
fn live_pids(root: &Path) -> HashMap<String, u32> {
    let mut live = HashMap::new();
    let Ok(entries) = std::fs::read_dir(root.join("active_pids")) else {
        return live;
    };
    for entry in entries.filter_map(Result::ok) {
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let pid: Option<u32> = std::fs::read_to_string(entry.path())
            .ok()
            .and_then(|text| text.trim().parse().ok());
        match pid {
            Some(pid) if Path::new(&format!("/proc/{pid}")).exists() => {
                live.insert(id, pid);
            }
            _ => {}
        }
    }
    live
}

/// jcode timestamps are chrono RFC 3339.
fn parse_time(text: &str) -> Option<Timestamp> {
    text.parse::<Timestamp>().ok()
}
