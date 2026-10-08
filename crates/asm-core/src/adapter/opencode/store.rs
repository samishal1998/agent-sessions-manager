//! Read-only queries over OpenCode's SQLite store.

use std::collections::HashMap;
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
    // 2.x keeps sessions in `session_v2` with the same columns this reads
    // (plus `time_suspended`, the only signal that a turn is in flight).
    let v2 = adapter.is_v2();
    // Asked only if a row turns out to be claimed: proving a service is up
    // costs a request.
    let service = std::cell::OnceCell::new();
    let mut sessions = if v2 {
        let mut found = read_table(adapter, &conn, filter, "session_v2", "", true, &session_sizes(&conn, true), &service)?;
        // A 2.x database keeps the 1.x tables it migrates from, and sessions
        // OpenCode has not copied yet exist only there: list them too (read
        // only; asm changes nothing in such a store), so nothing is hidden.
        // Their messages are the 1.x `message` and `part` rows.
        if matches!(super::unmigrated(adapter.db()), Ok(n) if n > 0) {
            let only_there = "WHERE NOT EXISTS (SELECT 1 FROM session_v2 v WHERE v.id = s.id)";
            found.extend(read_table(adapter, &conn, filter, "session", only_there, false, &session_sizes(&conn, false), &service)?);
        }
        found
    } else {
        read_table(adapter, &conn, filter, "session", "", false, &session_sizes(&conn, false), &service)?
    };
    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated));
    Ok(sessions)
}

/// The sessions of one table (`session_v2`, or 1.x's `session`, optionally
/// narrowed by `condition`). `v2` says it has the 2.x columns and meaning.
#[allow(clippy::too_many_arguments)]
fn read_table(
    adapter: &OpenCodeAdapter,
    conn: &Connection,
    filter: &SessionFilter,
    table: &str,
    condition: &str,
    v2: bool,
    sizes: &HashMap<String, u64>,
    service: &std::cell::OnceCell<Option<u32>>,
) -> Result<Vec<Session>, CoreError> {
    let suspended = if v2 { "s.time_suspended" } else { "NULL" };
    let sql_err =
        |e: rusqlite::Error| CoreError::Sqlite { db: adapter.db().to_path_buf(), source: Box::new(e) };

    let mut stmt = conn
        .prepare(&format!(
            "SELECT s.id, s.parent_id, s.slug, s.directory, s.title, s.version, s.agent,
                    s.model, s.cost, s.tokens_input, s.tokens_output, s.tokens_cache_read,
                    s.tokens_cache_write, s.time_created, s.time_updated, s.time_archived,
                    p.worktree, {suspended} AS time_suspended
             FROM {table} s LEFT JOIN project p ON p.id = s.project_id {condition}",
        ))
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
            let time_suspended: Option<i64> = row.get("time_suspended")?;
            Ok((
                id, parent, slug, directory, title, version, model_json, cost,
                tokens_input, tokens_output, tokens_cache_read, tokens_cache_write,
                time_created, time_updated, time_archived, worktree, time_suspended,
            ))
        })
        .map_err(sql_err)?;

    let mut sessions = Vec::new();
    for row in rows {
        let (
            id, parent, slug, directory, title, version, model_json, cost,
            tokens_input, tokens_output, tokens_cache_read, tokens_cache_write,
            time_created, time_updated, time_archived, worktree, time_suspended,
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

        // 2.x claims a session (`time_suspended`) while a turn runs and
        // releases it when the turn settles; a claim whose server is gone is
        // an interrupted turn waiting to resume, not a live session.
        let running = if time_suspended.is_some() { *service.get_or_init(|| super::v2::service_pid(adapter)) } else { None };
        // 2.0.25 ignores `time_archived` (nothing sets it but a migration or
        // an import, and nothing filters on it), so on a 2.x store it does
        // not make a session archived: one archived in 1.x would show as
        // archived here, with no way to ever unarchive it.
        let status = if running.is_some() {
            SessionStatus::Live { pid: running }
        } else if time_archived.is_some() && !v2 {
            SessionStatus::Archived
        } else {
            SessionStatus::Idle
        };

        let id_for_size = id.clone();
        let session = Session {
            handle: SessionRef {
                agent: AgentKind::OpenCode,
                native_id: id,
                location: SessionLocation::SqliteRow {
                    db: adapter.db().to_path_buf(),
                    table: table.to_string(),
                },
            },
            title,
            slug,
            project_root: PathBuf::from(project_root),
            git_branch: None, // 1.17.x: workspace table unused; 2.x: workspace.binding is provider-specific JSON
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
            size_bytes: sizes.get(&id_for_size).copied(),
        };
        if filter.matches(&session) {
            sessions.push(session);
        }
    }

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

/// Bytes each session's rows occupy, in one pass over both payload tables.
///
/// OpenCode has no file per session to stat, so "size" is the length of the
/// JSON it stores: the message envelopes plus their parts, which is where
/// effectively all of a session's bytes live.
/// `data` is a TEXT column, and SQLite's LENGTH counts characters there,
/// not bytes — the cast to BLOB is what makes this a byte count. Real
/// transcripts are a fraction of a percent multibyte, so the difference is
/// small but always in the same direction.
fn session_sizes(conn: &Connection, v2: bool) -> HashMap<String, u64> {
    let mut sizes: HashMap<String, u64> = HashMap::new();
    let tables: &[&str] = if v2 { &["session_message"] } else { &["message", "part"] };
    for table in tables {
        let sql = format!(
            "SELECT session_id, SUM(LENGTH(CAST(COALESCE(data, '') AS BLOB))) \
             FROM {table} GROUP BY session_id"
        );
        let Ok(mut stmt) = conn.prepare(&sql) else { continue };
        let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1).unwrap_or(0)))
        }) else {
            continue;
        };
        for (id, bytes) in rows.filter_map(Result::ok) {
            *sizes.entry(id).or_default() += bytes.max(0) as u64;
        }
    }
    sizes
}
