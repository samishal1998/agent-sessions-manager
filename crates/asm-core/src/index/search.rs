//! Refreshing the index and querying it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use rusqlite::params;
use serde::Serialize;

use crate::CoreError;
use crate::adapter::SessionFilter;
use crate::ir::{IrMessage, IrPart, IrRole};
use crate::model::{AgentKind, Session, SessionLocation};
use crate::ops;

use super::Index;

/// Tool output is most of the bulk of a large transcript and the least
/// searchable-per-byte, so only its head is indexed. Everything else goes
/// in whole.
const TOOL_OUTPUT_BUDGET: usize = 2_000;

#[derive(Debug, Default, Serialize)]
pub struct RefreshReport {
    pub scanned: usize,
    pub reindexed: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub messages_indexed: usize,
    /// Sessions that could not be read; the rest of the refresh still
    /// succeeds, because a partial index beats no index.
    pub failed: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct IndexStats {
    pub path: PathBuf,
    pub sessions: usize,
    pub messages: usize,
    pub bytes: u64,
    pub oldest_indexed_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub text: String,
    pub agent: Option<AgentKind>,
    pub project: Option<PathBuf>,
    pub limit: usize,
}

impl Default for SearchQuery {
    fn default() -> Self {
        SearchQuery { text: String::new(), agent: None, project: None, limit: 30 }
    }
}

/// Marks the start of a matched term inside [`SearchHit::snippet`].
pub const MATCH_START: char = '\u{1}';
/// Marks the end of a matched term inside [`SearchHit::snippet`].
pub const MATCH_END: char = '\u{2}';

#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub agent: String,
    pub native_id: String,
    pub title: Option<String>,
    pub project_root: Option<String>,
    pub role: String,
    /// Position of the matching message within the session.
    pub seq: i64,
    pub timestamp: Option<String>,
    /// Match with its surrounding text; matched terms are wrapped in
    /// [`MATCH_START`] / [`MATCH_END`].
    pub snippet: String,
    /// bm25; lower is a better match.
    pub score: f64,
    /// "idle" | "live" | "archived". Archived sessions are searchable but
    /// absent from `asm list` until they are restored.
    pub status: String,
}

fn status_name(status: &crate::model::SessionStatus) -> &'static str {
    match status {
        crate::model::SessionStatus::Live { .. } => "live",
        crate::model::SessionStatus::Idle => "idle",
        crate::model::SessionStatus::Archived => "archived",
    }
}

/// An opaque string that changes whenever a session's content changes.
/// Compared verbatim; nothing outside this function interprets its shape.
/// Also the hub daemon's "did anything happen" pre-filter, which is why it
/// is public: two definitions of "changed" would drift.
pub fn fingerprint(session: &Session) -> String {
    let updated_ms = session.updated.map(|t| t.as_millisecond()).unwrap_or(0);
    match &session.handle.location {
        // Transcripts are append-only, so size plus mtime is decisive.
        SessionLocation::JsonlFile { path } => format!("file:{}", stat(path)),
        SessionLocation::SqliteRow { db, .. } => {
            // `session.time_updated` LAGS: measured against the real store,
            // message rows carry a `time_updated` newer than their
            // session's on the busiest sessions, so trusting the session
            // row alone silently loses streamed tool output from the index.
            // Ask the message table instead.
            match message_watermark(db, &session.handle.native_id) {
                Some((count, max_updated)) => format!("rows:{count}:{max_updated}:{updated_ms}"),
                // No message table (Antigravity keeps one database per
                // conversation). New steps land in the WAL, and in the
                // brain transcript, long before the database's own mtime
                // moves at a checkpoint.
                None => {
                    let mut wal = db.clone().into_os_string();
                    wal.push("-wal");
                    let mut fp = format!("rows:?:{updated_ms}:{}:{}", stat(db), stat(wal.as_ref()));
                    if session.handle.agent == AgentKind::Antigravity
                        && let Some(root) = db.parent().and_then(std::path::Path::parent)
                    {
                        let transcript = crate::adapter::antigravity::AntigravityAdapter::with_root(root)
                            .transcript_of(&session.handle.native_id);
                        fp = format!("{fp}:{}", stat(&transcript));
                    }
                    fp
                }
            }
        }
        SessionLocation::Archive { dir } => format!("archive:{}:{updated_ms}", dir.display()),
    }
}

/// `size:mtime_ms` of a file, `0:0` when it is not there.
fn stat(path: &std::path::Path) -> String {
    let meta = std::fs::metadata(path).ok();
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime_ms = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{size}:{mtime_ms}")
}

/// (message count, newest message timestamp) for a row-backed session.
fn message_watermark(db: &std::path::Path, session_id: &str) -> Option<(i64, i64)> {
    let conn = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.query_row(
        "SELECT COUNT(*), COALESCE(MAX(MAX(COALESCE(time_updated,0), COALESCE(time_created,0))), 0)
         FROM message WHERE session_id = ?1",
        [session_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .ok()
}

/// How far a refresh has got, for a frontend that shows it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RefreshProgress {
    pub done: usize,
    /// Sessions that need re-reading — nothing to do with how many exist.
    pub total: usize,
    /// The session being read, when there is one.
    pub note: Option<String>,
}

/// How much a transaction may hold before it is written. Sessions rather
/// than bytes would be the obvious bound, and the wrong one: one machine's
/// session is a hundred kilobytes and another's is a hundred megabytes.
const BATCH_BYTES: usize = 32 * 1024 * 1024;
const BATCH_SESSIONS: usize = 512;
/// The first commit is small and they grow from there: a search run while
/// the index is still building finds something in the first second,
/// without paying for a commit per session afterwards.
const FIRST_BATCH: usize = 32;

/// What the index already believes about a session.
struct Labels {
    fingerprint: String,
    title: Option<String>,
    project_root: String,
    status: String,
}

impl Labels {
    fn differ_from(&self, session: &Session) -> bool {
        self.title != session.title
            || self.project_root != session.project_root.display().to_string()
            || self.status != status_name(&session.status)
    }
}

/// A session read and ready to write: no transcript is held any longer
/// than it takes the writer to get to it.
struct Documents {
    agent: String,
    id: String,
    label: String,
    fingerprint: String,
    title: Option<String>,
    project_root: String,
    updated_ms: Option<i64>,
    status: &'static str,
    /// (sequence, role, text, timestamp)
    messages: Vec<(usize, &'static str, String, Option<String>)>,
}

impl Documents {
    /// Roughly what this is holding, for the writer's memory bound.
    fn bytes(&self) -> usize {
        self.messages.iter().map(|(_, _, text, _)| text.len()).sum()
    }

    fn of(session: &Session, ir: &crate::ir::IrSession) -> Documents {
        Documents {
            agent: session.handle.agent.to_string(),
            id: session.handle.native_id.clone(),
            label: format!("{} {}", session.handle.agent, session.short_id()),
            fingerprint: fingerprint(session),
            title: session.title.clone(),
            project_root: session.project_root.display().to_string(),
            updated_ms: session.updated.map(|t| t.as_millisecond()),
            status: status_name(&session.status),
            messages: ir
                .messages
                .iter()
                .enumerate()
                .filter_map(|(seq, message)| {
                    let text = message_text(message);
                    (!text.trim().is_empty()).then(|| {
                        (seq, role_name(message.role), text, message.timestamp.map(|t| t.to_string()))
                    })
                })
                .collect(),
        }
    }
}

enum Extracted {
    Read(Documents),
    Failed(String),
}

impl Index {
    /// Bring the index in line with the agents' stores. `progress` is called
    /// with a short message per session actually re-extracted.
    pub fn refresh(&mut self, progress: impl FnMut(RefreshProgress)) -> Result<RefreshReport, CoreError> {
        // Subagent sessions are included: their content is worth finding
        // even though the pickers hide them.
        let filter = SessionFilter { include_children: true, ..SessionFilter::default() };
        let mut sessions = ops::list_sessions(&filter)?;
        // Archived sessions have left their agent's store but not our
        // archive, and "where did I do that thing" should still find them.
        sessions.extend(crate::sync::archived_sessions().unwrap_or_default());
        self.refresh_with(&sessions, ops::export_ir, progress)
    }

    /// An error mapper that does not borrow the index, so a transaction
    /// can hold the connection while it is used.
    fn sql_err_at(db: PathBuf) -> impl Fn(rusqlite::Error) -> CoreError {
        move |e| CoreError::Sqlite { db: db.clone(), source: Box::new(e) }
    }

    /// Every session the index knows, in one query: on a busy machine the
    /// per-session lookups cost more than reading the changed ones.
    fn known(&self) -> Result<HashMap<(String, String), Labels>, CoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT agent, native_id, fingerprint, title, project_root, status FROM indexed_session")
            .map_err(self.sql_err())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                    Labels {
                        fingerprint: row.get(2)?,
                        title: row.get(3)?,
                        project_root: row.get(4)?,
                        status: row.get(5)?,
                    },
                ))
            })
            .map_err(self.sql_err())?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// Metadata that moved without the conversation changing.
    fn relabel(&mut self, sessions: &[&Session]) -> Result<(), CoreError> {
        if sessions.is_empty() {
            return Ok(());
        }
        let sql_err = Self::sql_err_at(self.path.clone());
        let tx = self.conn.transaction().map_err(&sql_err)?;
        for session in sessions {
            tx.execute(
                "UPDATE indexed_session SET title = ?3, project_root = ?4, status = ?5
                 WHERE agent = ?1 AND native_id = ?2",
                params![
                    session.handle.agent.to_string(),
                    session.handle.native_id,
                    session.title,
                    session.project_root.display().to_string(),
                    status_name(&session.status),
                ],
            )
            .map_err(&sql_err)?;
        }
        tx.commit().map_err(&sql_err)
    }

    /// One transaction for a batch of read sessions.
    fn write(&mut self, batch: &[Documents]) -> Result<(), CoreError> {
        if batch.is_empty() {
            return Ok(());
        }
        let sql_err = Self::sql_err_at(self.path.clone());
        let tx = self.conn.transaction().map_err(&sql_err)?;
        // Let FTS5 keep more, smaller segments while a batch goes in and
        // merge them afterwards, rather than merging as it writes.
        tx.execute_batch("INSERT INTO message_fts(message_fts, rank) VALUES('automerge', 16);")
            .map_err(&sql_err)?;
        {
            let mut insert = tx
                .prepare(
                    "INSERT INTO message_fts (text, agent, native_id, role, seq, ts)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(&sql_err)?;
            let mut forget = tx
                .prepare("DELETE FROM message_fts WHERE agent = ?1 AND native_id = ?2")
                .map_err(&sql_err)?;
            let mut record = tx
                .prepare(
                    "INSERT OR REPLACE INTO indexed_session
                        (agent, native_id, title, project_root, updated_ms, status,
                         fingerprint, message_count, indexed_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )
                .map_err(&sql_err)?;
            let now = jiff::Timestamp::now().as_millisecond();
            for documents in batch {
                forget.execute(params![documents.agent, documents.id]).map_err(&sql_err)?;
                for (seq, role, text, ts) in &documents.messages {
                    insert
                        .execute(params![text, documents.agent, documents.id, role, *seq as i64, ts])
                        .map_err(&sql_err)?;
                }
                record
                    .execute(params![
                        documents.agent,
                        documents.id,
                        documents.title,
                        documents.project_root,
                        documents.updated_ms,
                        documents.status,
                        documents.fingerprint,
                        documents.messages.len() as i64,
                        now,
                    ])
                    .map_err(&sql_err)?;
            }
        }
        tx.commit().map_err(&sql_err)
    }

    /// The injectable half of `refresh`: which sessions to index and how to
    /// read them. Keeps the index testable without a real agent store.
    ///
    /// On a machine with thousands of sessions the shape of this matters:
    /// what each session is known to be is loaded in one query rather than
    /// two per session, the reading is spread over threads, and the writing
    /// is batched — SQLite takes one writer, and a transaction per session
    /// spends most of its time in fsync.
    pub fn refresh_with(
        &mut self,
        sessions: &[Session],
        export: impl Fn(&Session) -> Result<crate::ir::IrSession, CoreError> + Sync,
        mut progress: impl FnMut(RefreshProgress),
    ) -> Result<RefreshReport, CoreError> {
        let mut report = RefreshReport { scanned: sessions.len(), ..RefreshReport::default() };
        let known = self.known()?;
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut changed: Vec<&Session> = Vec::new();
        let mut relabel: Vec<&Session> = Vec::new();

        for session in sessions {
            let key = (session.handle.agent.to_string(), session.handle.native_id.clone());
            seen.insert(key.clone());
            match known.get(&key) {
                Some(labels) if labels.fingerprint == fingerprint(session) => {
                    // The conversation is unchanged, but cheap metadata can
                    // still have moved — a session goes live and idle again
                    // without its transcript changing, and archiving moves
                    // the file without rewriting it.
                    if labels.differ_from(session) {
                        relabel.push(session);
                    }
                    report.unchanged += 1;
                }
                _ => changed.push(session),
            }
        }

        let total = changed.len();
        progress(RefreshProgress { done: 0, total, note: None });
        self.relabel(&relabel)?;

        // One thread reads and parses while another writes: extraction is
        // most of the work and none of it touches the database.
        let readers = std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1).clamp(1, 8))
            .unwrap_or(1);
        let next = std::sync::atomic::AtomicUsize::new(0);
        // Bounded, so a fast reader cannot hold every transcript in memory
        // at once while the writer catches up.
        let (tx, rx) = std::sync::mpsc::sync_channel::<Extracted>(readers * 2);
        let changed = &changed;
        let export = &export;
        std::thread::scope(|scope| -> Result<(), CoreError> {
            for _ in 0..readers {
                let tx = tx.clone();
                let next = &next;
                scope.spawn(move || {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(session) = changed.get(i) else { break };
                        let extracted = match export(session) {
                            Ok(ir) => Extracted::Read(Documents::of(session, &ir)),
                            // One unreadable session must not abort the refresh.
                            Err(e) => Extracted::Failed(format!(
                                "{}:{}: {e}",
                                session.handle.agent, session.handle.native_id
                            )),
                        };
                        if tx.send(extracted).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(tx);

            let mut batch: Vec<Documents> = Vec::new();
            let mut held = 0usize;
            let mut flush_at = FIRST_BATCH;
            for item in rx {
                match item {
                    Extracted::Failed(why) => report.failed.push(why),
                    Extracted::Read(documents) => {
                        report.reindexed += 1;
                        report.messages_indexed += documents.messages.len();
                        progress(RefreshProgress {
                            done: report.reindexed + report.failed.len(),
                            total,
                            note: Some(documents.label.clone()),
                        });
                        held += documents.bytes();
                        batch.push(documents);
                        if batch.len() >= flush_at || held >= BATCH_BYTES {
                            self.write(&std::mem::take(&mut batch))?;
                            held = 0;
                            flush_at = (flush_at * 4).min(BATCH_SESSIONS);
                        }
                    }
                }
            }
            self.write(&batch)
        })?;

        // Drop sessions that have gone away (deleted, or archived out).
        let stale: Vec<(String, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT agent, native_id FROM indexed_session")
                .map_err(self.sql_err())?;
            let rows = stmt
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                .map_err(self.sql_err())?;
            rows.filter_map(Result::ok).filter(|key| !seen.contains(key)).collect()
        };
        for (agent, id) in &stale {
            self.conn
                .execute(
                    "DELETE FROM message_fts WHERE agent = ?1 AND native_id = ?2",
                    params![agent, id],
                )
                .map_err(self.sql_err())?;
            self.conn
                .execute(
                    "DELETE FROM indexed_session WHERE agent = ?1 AND native_id = ?2",
                    params![agent, id],
                )
                .map_err(self.sql_err())?;
            report.removed += 1;
        }

        Ok(report)
    }

    pub fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHit>, CoreError> {
        // Input that reduces to nothing searchable (whitespace, or stray
        // quotes) means "no results", never an FTS5 syntax error.
        let match_expression = fts_query(&query.text);
        if match_expression.is_empty() {
            return Ok(Vec::new());
        }
        // Delimit matches with control characters rather than brackets:
        // transcripts are full of code, so any printable delimiter would be
        // ambiguous with the content itself. Renderers turn these into
        // whatever they use for emphasis.
        let mut sql = String::from(
            "SELECT m.agent, m.native_id, s.title, s.project_root, m.role, m.seq, m.ts,
                    snippet(message_fts, 0, '\u{1}', '\u{2}', '…', 14), bm25(message_fts),
                    s.status
             FROM message_fts m
             LEFT JOIN indexed_session s
                    ON s.agent = m.agent AND s.native_id = m.native_id
             WHERE message_fts MATCH ?1",
        );
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(match_expression)];
        if let Some(agent) = query.agent {
            binds.push(Box::new(agent.to_string()));
            sql.push_str(&format!(" AND m.agent = ?{}", binds.len()));
        }
        if let Some(project) = &query.project {
            binds.push(Box::new(project.display().to_string()));
            sql.push_str(&format!(" AND s.project_root = ?{}", binds.len()));
        }
        sql.push_str(" ORDER BY bm25(message_fts) LIMIT ?");
        binds.push(Box::new(query.limit as i64));
        sql.push_str(&binds.len().to_string());

        let mut stmt = self.conn.prepare(&sql).map_err(self.sql_err())?;
        let refs: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
        let rows = stmt
            .query_map(refs.as_slice(), |row| {
                Ok(SearchHit {
                    agent: row.get(0)?,
                    native_id: row.get(1)?,
                    title: row.get(2)?,
                    project_root: row.get(3)?,
                    role: row.get(4)?,
                    seq: row.get(5)?,
                    timestamp: row.get(6)?,
                    snippet: row.get(7)?,
                    score: row.get(8)?,
                    status: row.get::<_, Option<String>>(9)?.unwrap_or_else(|| "idle".into()),
                })
            })
            .map_err(self.sql_err())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(self.sql_err())
    }

    /// Reclaim space left by re-extracted sessions.
    ///
    /// Repeatedly re-indexing a growing session leaves free pages behind.
    /// FTS5's `optimize` and `rebuild` restructure the index but do NOT
    /// return pages to the filesystem (measured: 57.2 MB before and after);
    /// only `VACUUM` does (38.0 MB). Run from the explicit `asm index`, not
    /// from the refresh on the search path.
    pub fn compact(&self) -> Result<u64, CoreError> {
        let before = std::fs::metadata(self.path()).map(|m| m.len()).unwrap_or(0);
        self.conn.execute_batch("VACUUM;").map_err(self.sql_err())?;
        let after = std::fs::metadata(self.path()).map(|m| m.len()).unwrap_or(0);
        Ok(before.saturating_sub(after))
    }

    pub fn stats(&self) -> Result<IndexStats, CoreError> {
        let sessions: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM indexed_session", [], |r| r.get(0))
            .map_err(self.sql_err())?;
        let messages: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM message_fts", [], |r| r.get(0))
            .map_err(self.sql_err())?;
        let oldest: Option<i64> = self
            .conn
            .query_row("SELECT MIN(indexed_at_ms) FROM indexed_session", [], |r| r.get(0))
            .ok()
            .flatten();
        let bytes = std::fs::metadata(self.path()).map(|m| m.len()).unwrap_or(0);
        Ok(IndexStats {
            path: self.path().to_path_buf(),
            sessions: sessions as usize,
            messages: messages as usize,
            bytes,
            oldest_indexed_ms: oldest,
        })
    }
}

fn role_name(role: IrRole) -> &'static str {
    match role {
        IrRole::User => "user",
        IrRole::Assistant => "assistant",
        IrRole::System => "system",
    }
}

/// Subagent runs can nest; this bounds the recursion regardless.
const MAX_AGENT_DEPTH: usize = 4;

/// Flatten one message into the text that gets indexed. Tool *inputs* are
/// worth searching in full (they hold commands, paths, patterns); tool
/// *outputs* are truncated because they dominate transcript bulk.
fn message_text(message: &IrMessage) -> String {
    message_text_at(message, 0)
}

fn message_text_at(message: &IrMessage, depth: usize) -> String {
    let mut out = String::new();
    for part in &message.parts {
        match part {
            IrPart::Text { text } => push_line(&mut out, text),
            IrPart::Reasoning { summary, .. } => push_line(&mut out, summary),
            IrPart::ToolCall { name, input, .. } => {
                push_line(&mut out, name);
                push_line(&mut out, &input_text(input));
            }
            IrPart::ToolResult { output, .. } => push_line(&mut out, &clip(output, TOOL_OUTPUT_BUDGET)),
            IrPart::File { path, .. } => {
                if let Some(path) = path {
                    push_line(&mut out, &path.0);
                }
            }
            IrPart::Agent { name, description, transcript } => {
                push_line(&mut out, name);
                if let Some(description) = description {
                    push_line(&mut out, description);
                }
                // Subagent transcripts are the majority of the searchable
                // text in a delegating session — indexing only the agent's
                // name would hide most of the corpus. Their parse cost is
                // already paid by the exporter.
                if depth < MAX_AGENT_DEPTH {
                    for sub in transcript {
                        push_line(&mut out, &message_text_at(sub, depth + 1));
                    }
                }
            }
            IrPart::Unknown => {}
        }
    }
    out
}

fn push_line(out: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(text);
}

fn input_text(input: &serde_json::Value) -> String {
    match input {
        serde_json::Value::Object(map) => map
            .values()
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(" "),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    // Cut on a character boundary, not a byte index.
    let end = text
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= max)
        .last()
        .unwrap_or(0);
    text[..end].to_string()
}

/// Turn user input into an FTS5 query.
///
/// FTS5's query syntax would otherwise make ordinary text a syntax error —
/// a bare `-`, `"` or `*` is enough. Each whitespace-separated word becomes
/// a quoted term (AND-ed), which is what a user typing words expects, while
/// an explicitly quoted phrase is passed through as a phrase.
fn fts_query(input: &str) -> String {
    let trimmed = input.trim();
    // A quoted phrase is passed through, but only when the quotes actually
    // wrap something — `"` alone is not a phrase.
    if trimmed.len() >= 3 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        return trimmed.to_string();
    }
    trimmed
        .split_whitespace()
        .map(|word| format!("\"{}\"", word.replace('"', "")))
        .filter(|word| word != "\"\"")
        .collect::<Vec<_>>()
        .join(" AND ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_text_becomes_a_safe_fts_query() {
        assert_eq!(fts_query("hello world"), "\"hello\" AND \"world\"");
        // Characters that are FTS5 operators must not blow up the query.
        assert_eq!(fts_query("--flag"), "\"--flag\"");
        assert_eq!(fts_query("a*b"), "\"a*b\"");
        // An explicit phrase is honored.
        assert_eq!(fts_query("\"exact phrase\""), "\"exact phrase\"");
        assert_eq!(fts_query("  "), "");
    }

    #[test]
    fn clip_respects_character_boundaries() {
        let text = "é".repeat(100); // two bytes each
        let clipped = clip(&text, 51);
        assert!(clipped.len() <= 52);
        assert!(clipped.chars().all(|c| c == 'é'));
    }
}
