//! Listing Codex sessions.
//!
//! `state_5.sqlite` is the source of truth for what codex itself will show,
//! so it drives the listing; the filesystem is then swept for rollouts the
//! database has not adopted, which codex hides but asm should not.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rusqlite::{Connection, OpenFlags};

use crate::CoreError;
use crate::model::{AgentKind, Session, SessionLocation, SessionRef, SessionStatus, Usage};

use super::super::SessionFilter;
use super::CodexAdapter;

/// Columns worth reading that were added by later sqlx migrations. Selecting
/// one that does not exist fails the whole statement, so the set is
/// intersected with `PRAGMA table_info` before the query is built.
const OPTIONAL_COLUMNS: &[&str] =
    &["model", "cli_version", "preview", "first_user_message", "name"];

fn open_ro(db: &Path) -> Result<Connection, CoreError> {
    // READ_ONLY without immutable, so a running codex's WAL is honored.
    Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| CoreError::Sqlite { db: db.to_path_buf(), source: Box::new(e) })
}

fn present_columns(conn: &Connection, table: &str) -> HashSet<String> {
    let mut found = HashSet::new();
    if let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})"))
        && let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(1))
    {
        found.extend(rows.flatten());
    }
    found
}

/// Epoch seconds as stored by codex; 0 means "never set".
fn secs(value: Option<i64>) -> Option<Timestamp> {
    value.filter(|v| *v > 0).and_then(|v| Timestamp::from_second(v).ok())
}

pub(super) fn sessions(
    adapter: &CodexAdapter,
    filter: &SessionFilter,
) -> Result<Vec<Session>, CoreError> {
    if let Some(agent) = filter.agent
        && agent != AgentKind::Codex
    {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();
    let mut seen_rollouts: HashSet<PathBuf> = HashSet::new();

    let db = adapter.state_db();
    if db.is_file() {
        let conn = open_ro(&db)?;
        let available = present_columns(&conn, "threads");
        let children = spawned_children(&conn);
        let optional: Vec<&str> =
            OPTIONAL_COLUMNS.iter().copied().filter(|c| available.contains(*c)).collect();

        let mut select = String::from(
            "SELECT id, rollout_path, created_at, updated_at, cwd, title, archived, \
             archived_at, git_branch, tokens_used",
        );
        for column in &optional {
            select.push_str(", ");
            select.push_str(column);
        }
        select.push_str(" FROM threads");

        let sql_err =
            |e: rusqlite::Error| CoreError::Sqlite { db: db.clone(), source: Box::new(e) };
        let mut stmt = conn.prepare(&select).map_err(sql_err)?;
        let rows = stmt
            .query_map([], |row| {
                let optional_str = |name: &str| -> Option<String> {
                    optional
                        .contains(&name)
                        .then(|| row.get::<_, Option<String>>(name).ok().flatten())
                        .flatten()
                        .filter(|s| !s.is_empty())
                };
                let id: String = row.get("id")?;
                let rollout_path: String = row.get("rollout_path")?;
                let cwd: String = row.get("cwd")?;
                let archived: i64 = row.get("archived").unwrap_or(0);
                let tokens: Option<i64> = row.get("tokens_used").ok().flatten();
                // `title` is codex's own label; when it is empty the picker
                // falls back to the first user message, so do the same.
                let title = row
                    .get::<_, Option<String>>("title")
                    .ok()
                    .flatten()
                    .filter(|s| !s.is_empty())
                    .or_else(|| optional_str("name"))
                    .or_else(|| optional_str("preview"))
                    .or_else(|| optional_str("first_user_message"));

                let rollout = PathBuf::from(&rollout_path);
                Ok(Session {
                    handle: SessionRef {
                        agent: AgentKind::Codex,
                        native_id: id.clone(),
                        location: SessionLocation::JsonlFile { path: rollout.clone() },
                    },
                    title,
                    slug: None,
                    project_root: PathBuf::from(cwd),
                    git_branch: row
                        .get::<_, Option<String>>("git_branch")
                        .ok()
                        .flatten()
                        .filter(|s| !s.is_empty()),
                    created: secs(row.get("created_at").ok().flatten()),
                    updated: secs(row.get("updated_at").ok().flatten()),
                    model: optional_str("model"),
                    usage: Usage {
                        output_tokens: tokens.filter(|t| *t > 0).map(|t| t as u64),
                        ..Usage::default()
                    },
                    status: if archived != 0 {
                        SessionStatus::Archived
                    } else {
                        SessionStatus::Idle
                    },
                    parent: children.iter().find(|(child, _)| *child == id).map(|(_, p)| p.clone()),
                    agent_version: optional_str("cli_version"),
                    size_bytes: std::fs::metadata(&rollout).ok().map(|m| m.len()),
                })
            })
            .map_err(sql_err)?;

        for session in rows.flatten() {
            if let SessionLocation::JsonlFile { path } = &session.handle.location {
                seen_rollouts.insert(path.clone());
            }
            sessions.push(session);
        }
    }

    sessions.extend(orphan_rollouts(adapter, &seen_rollouts));
    sessions.retain(|s| filter.matches(s));
    if !filter.include_children {
        sessions.retain(|s| s.parent.is_none());
    }
    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated));
    Ok(sessions)
}

/// child id -> parent id, for the sessions codex spawned as subagents.
fn spawned_children(conn: &Connection) -> Vec<(String, String)> {
    let Ok(mut stmt) =
        conn.prepare("SELECT child_thread_id, parent_thread_id FROM thread_spawn_edges")
    else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?))) else {
        return Vec::new();
    };
    rows.flatten().collect()
}

/// Rollouts on disk with no `threads` row. Codex adopts these lazily when
/// resumed by id, so they are real sessions that its own picker cannot see.
fn orphan_rollouts(adapter: &CodexAdapter, seen: &HashSet<PathBuf>) -> Vec<Session> {
    let mut found = Vec::new();
    let mut stack = vec![adapter.sessions_dir()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if seen.contains(&path) || !is_rollout(&path) {
                continue;
            }
            if let Some(session) = super::export_ir::session_from_rollout(&path) {
                found.push(session);
            }
        }
    }
    found
}

/// `.jsonl` and its zstd-compressed form, which older codex versions wrote.
/// The compressed ones are recognized so they can be reported rather than
/// silently missed; reading them is not supported.
fn is_rollout(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
    name.starts_with("rollout-") && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"))
}
