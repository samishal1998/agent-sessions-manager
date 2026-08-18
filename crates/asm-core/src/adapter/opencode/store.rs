//! Read-only queries over OpenCode's SQLite store.

use std::path::PathBuf;

use jiff::Timestamp;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::CoreError;
use crate::model::{AgentKind, Session, SessionLocation, SessionRef, SessionStatus, Usage};

use super::super::SessionFilter;
use super::OpenCodeAdapter;

fn open_ro(adapter: &OpenCodeAdapter) -> Result<Connection, CoreError> {
    // READ_ONLY without immutable: the WAL is honored, so live appends from
    // a running OpenCode are visible and nothing we do can write.
    Connection::open_with_flags(
        adapter.db(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| CoreError::Sqlite { db: adapter.db().to_path_buf(), source: Box::new(e) })
}

pub(super) fn sessions(
    adapter: &OpenCodeAdapter,
    filter: &SessionFilter,
) -> Result<Vec<Session>, CoreError> {
    if let Some(agent) = filter.agent
        && agent != AgentKind::OpenCode
    {
        return Ok(Vec::new());
    }

    let conn = open_ro(adapter)?;
    let sql_err =
        |e: rusqlite::Error| CoreError::Sqlite { db: adapter.db().to_path_buf(), source: Box::new(e) };

    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.parent_id, s.slug, s.directory, s.title, s.version, s.agent,
                    s.model, s.cost, s.tokens_input, s.tokens_output, s.tokens_cache_read,
                    s.tokens_cache_write, s.time_created, s.time_updated, s.time_archived,
                    p.worktree
             FROM session s LEFT JOIN project p ON p.id = s.project_id",
        )
        .map_err(sql_err)?;

    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get("id")?;
            let parent: Option<String> = row.get("parent_id")?;
            let slug: Option<String> = row.get("slug")?;
            let directory: Option<String> = row.get("directory")?;
            let title: Option<String> = row.get("title")?;
            let version: Option<String> = row.get("version")?;
            let model_json: Option<String> = row.get("model")?;
            let cost: Option<f64> = row.get("cost")?;
            let tokens_input: Option<i64> = row.get("tokens_input")?;
            let tokens_output: Option<i64> = row.get("tokens_output")?;
            let tokens_cache_read: Option<i64> = row.get("tokens_cache_read")?;
            let tokens_cache_write: Option<i64> = row.get("tokens_cache_write")?;
            let time_created: Option<i64> = row.get("time_created")?;
            let time_updated: Option<i64> = row.get("time_updated")?;
            let time_archived: Option<i64> = row.get("time_archived")?;
            let worktree: Option<String> = row.get("worktree")?;
            Ok((
                id, parent, slug, directory, title, version, model_json, cost,
                tokens_input, tokens_output, tokens_cache_read, tokens_cache_write,
                time_created, time_updated, time_archived, worktree,
            ))
        })
        .map_err(sql_err)?;

    let mut sessions = Vec::new();
    for row in rows {
        let (
            id, parent, slug, directory, title, version, model_json, cost,
            tokens_input, tokens_output, tokens_cache_read, tokens_cache_write,
            time_created, time_updated, time_archived, worktree,
        ) = row.map_err(sql_err)?;

        if parent.is_some() && !filter.include_children {
            continue;
        }

        // 1.2.x rows have NULL/empty directory; the project worktree is the
        // authoritative fallback.
        let project_root = directory
            .filter(|d| !d.is_empty())
            .or(worktree)
            .unwrap_or_default();
        if project_root.is_empty() {
            continue;
        }

        let status = if time_archived.is_some() {
            SessionStatus::Archived
        } else {
            SessionStatus::Idle
        };

        let session = Session {
            handle: SessionRef {
                agent: AgentKind::OpenCode,
                native_id: id,
                location: SessionLocation::SqliteRow {
                    db: adapter.db().to_path_buf(),
                    table: "session".to_string(),
                },
            },
            title,
            slug,
            project_root: PathBuf::from(project_root),
            git_branch: None, // workspace table (which models branches) is unused at 1.17.x
            created: time_created.map(from_millis),
            updated: time_updated.map(from_millis),
            model: model_json.as_deref().and_then(model_id),
            usage: Usage {
                cost_usd: cost.filter(|c| *c > 0.0),
                input_tokens: tokens_input.map(|t| t.max(0) as u64),
                output_tokens: tokens_output.map(|t| t.max(0) as u64),
                cache_read_tokens: tokens_cache_read.map(|t| t.max(0) as u64),
                cache_write_tokens: tokens_cache_write.map(|t| t.max(0) as u64),
            },
            status,
            parent,
            agent_version: version,
        };
        if filter.matches(&session) {
            sessions.push(session);
        }
    }

    sessions.sort_by(|a, b| b.updated.cmp(&a.updated));
    Ok(sessions)
}


fn from_millis(ms: i64) -> Timestamp {
    Timestamp::from_millisecond(ms).unwrap_or(Timestamp::UNIX_EPOCH)
}

/// `session.model` holds JSON like `{"id":"...","providerID":"...","variant":"..."}`.
fn model_id(json: &str) -> Option<String> {
    let value: Value = serde_json::from_str(json).ok()?;
    value.get("id").and_then(Value::as_str).map(str::to_string)
}
