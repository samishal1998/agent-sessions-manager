//! Listing Antigravity conversations.
//!
//! `conversations/*.db` is the authority on what exists — one file per
//! conversation. Everything else is enrichment, and every piece of it is
//! optional, because a conversation's database can exist before the caches
//! that describe it have been written.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::CoreError;
use crate::model::{AgentKind, Session, SessionLocation, SessionRef, SessionStatus, Usage};

use super::super::SessionFilter;
use super::AntigravityAdapter;

/// One entry of `cache/conversation_metadata.json`. Field names are Go's
/// exported identifiers, capitalized, because that file is a dump of
/// antigravity's own structs.
#[derive(Debug, Default, Deserialize)]
struct Summary {
    #[serde(rename = "Title")]
    title: Option<String>,
    /// Generated one-line description. `Title` is usually empty and this is
    /// what antigravity's own picker shows.
    #[serde(rename = "Preview")]
    preview: Option<String>,
    #[serde(rename = "UpdatedAt")]
    updated_at: Option<String>,
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// conversation id -> its cached summary.
fn summaries(adapter: &AntigravityAdapter) -> HashMap<String, Summary> {
    let Some(root) = read_json(&adapter.metadata_file()) else { return HashMap::new() };
    let Some(map) = root.get("conversations").and_then(Value::as_object) else {
        return HashMap::new();
    };
    map.iter()
        .map(|(id, entry)| {
            // A conversation with an unreadable summary still exists; it
            // just has no title to show.
            let summary = entry
                .get("summary")
                .cloned()
                .and_then(|s| serde_json::from_value::<Summary>(s).ok())
                .unwrap_or_default();
            (id.clone(), summary)
        })
        .collect()
}

/// conversation id -> the workspace it was last opened in.
///
/// The file is keyed the other way round, and it only remembers the most
/// recent conversation per directory: an older conversation in a directory
/// that has since been used again has no workspace recorded anywhere, and
/// is reported with an empty project rather than a guessed one.
fn workspaces(adapter: &AntigravityAdapter) -> HashMap<String, PathBuf> {
    let Some(Value::Object(map)) = read_json(&adapter.workspaces_file()) else {
        return HashMap::new();
    };
    map.into_iter()
        .filter_map(|(dir, id)| Some((id.as_str()?.to_string(), PathBuf::from(dir))))
        .collect()
}

fn parse_time(value: Option<&String>) -> Option<Timestamp> {
    value?.parse::<Timestamp>().ok()
}

fn modified(path: &Path) -> Option<Timestamp> {
    std::fs::metadata(path).ok()?.modified().ok().and_then(|t| Timestamp::try_from(t).ok())
}

/// The conversation database plus its write-ahead log. The WAL is where most
/// of a fresh conversation actually lives — reporting only the `.db` size
/// would understate a new conversation several times over.
fn size_of(db: &Path) -> Option<u64> {
    let base = std::fs::metadata(db).ok()?.len();
    let wal = std::fs::metadata(db.with_extension("db-wal")).map(|m| m.len()).unwrap_or(0);
    Some(base + wal)
}

/// First step's timestamp, from antigravity's own JSONL rendering. One short
/// read per conversation; the alternative is no creation time at all,
/// because none of the caches record one.
fn created_at(adapter: &AntigravityAdapter, id: &str) -> Option<Timestamp> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(adapter.transcript_of(id)).ok()?;
    let line = BufReader::new(file).lines().next()?.ok()?;
    let value: Value = serde_json::from_str(&line).ok()?;
    value.get("created_at")?.as_str()?.parse::<Timestamp>().ok()
}

pub(super) fn sessions(
    adapter: &AntigravityAdapter,
    filter: &SessionFilter,
) -> Result<Vec<Session>, CoreError> {
    if let Some(agent) = filter.agent
        && agent != AgentKind::Antigravity
    {
        return Ok(Vec::new());
    }

    let dir = adapter.conversations_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let summaries = summaries(adapter);
    let workspaces = workspaces(adapter);

    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // `-wal` and `-shm` sit beside every database; only the database
        // itself is a conversation.
        if path.extension().is_none_or(|e| e != "db") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else {
            continue;
        };
        let summary = summaries.get(&id);
        let title = summary
            .and_then(|s| s.title.clone())
            .filter(|t| !t.is_empty())
            .or_else(|| summary.and_then(|s| s.preview.clone()).filter(|p| !p.is_empty()));

        sessions.push(Session {
            handle: SessionRef {
                agent: AgentKind::Antigravity,
                native_id: id.clone(),
                location: SessionLocation::SqliteRow {
                    db: path.clone(),
                    table: "steps".to_string(),
                },
            },
            title,
            slug: None,
            // Empty when nothing on disk ties this conversation to a
            // directory, which is the honest answer.
            project_root: workspaces.get(&id).cloned().unwrap_or_default(),
            git_branch: None,
            created: created_at(adapter, &id),
            updated: parse_time(summary.and_then(|s| s.updated_at.as_ref()))
                .or_else(|| modified(&path)),
            model: None,
            usage: Usage::default(),
            status: SessionStatus::Idle,
            parent: None,
            agent_version: None,
            size_bytes: size_of(&path),
        });
    }

    sessions.retain(|s| filter.matches(s));
    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated));
    Ok(sessions)
}
