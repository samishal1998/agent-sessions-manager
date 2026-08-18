//! Full-text search over every session, in asm's own SQLite database.
//!
//! Design constraints that shaped this:
//! - The content lives in two foreign stores we must not write to, so the
//!   index is a standalone FTS5 table holding its own copy of the text (an
//!   external-content table would need a queryable local content table, and
//!   a contentless one cannot produce snippets).
//! - Claude transcripts reach 30 MB, so re-extracting everything on every
//!   search is out. Each session carries a cheap fingerprint (file size and
//!   mtime; the updated timestamp for row-backed sessions) and only
//!   sessions whose fingerprint moved are re-extracted.
//! - Re-extraction is whole-session rather than append-only-from-a-byte-
//!   offset: a session changes rarely, one 30 MB transcript re-extracts in
//!   about two seconds, and byte-offset resumption would have to reason
//!   about the transcript's abandoned branches. Simplicity wins here.
//! - The index is disposable. Anything unreadable, stale, or written by a
//!   different schema version is rebuilt rather than migrated, and a failed
//!   refresh never blocks a search of what is already indexed.

mod schema;
mod search;

pub use search::{
    IndexStats, MATCH_END, MATCH_START, RefreshReport, SearchHit, SearchQuery,
};

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::CoreError;

/// Bumped whenever the schema or the text-extraction rules change; a
/// mismatch rebuilds from scratch.
pub const SCHEMA_VERSION: u32 = 3;

pub struct Index {
    conn: Connection,
    path: PathBuf,
}

impl Index {
    /// Open (creating if needed) the index in asm's data directory.
    pub fn open() -> Result<Index, CoreError> {
        let path = default_path()?;
        Index::open_at(&path)
    }

    pub fn open_at(path: &Path) -> Result<Index, CoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::io(parent, e))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| CoreError::Sqlite { db: path.to_path_buf(), source: Box::new(e) })?;
        let mut index = Index { conn, path: path.to_path_buf() };
        index.prepare()?;
        Ok(index)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn sql_err(&self) -> impl Fn(rusqlite::Error) -> CoreError + '_ {
        |e| CoreError::Sqlite { db: self.path.clone(), source: Box::new(e) }
    }

    /// Create the schema, or wipe and recreate it if the version moved.
    fn prepare(&mut self) -> Result<(), CoreError> {
        // WAL keeps searches readable while a refresh writes; the busy
        // timeout covers a CLI, a TUI and the web server refreshing at once.
        self.conn
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .map_err(self.sql_err())?;

        let existing: Option<u32> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |row| {
                row.get::<_, String>(0)
            })
            .ok()
            .and_then(|v| v.parse().ok());

        if existing != Some(SCHEMA_VERSION) {
            self.conn.execute_batch(schema::DROP).map_err(self.sql_err())?;
            self.conn.execute_batch(schema::CREATE).map_err(self.sql_err())?;
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
                    [SCHEMA_VERSION.to_string()],
                )
                .map_err(self.sql_err())?;
        }
        Ok(())
    }
}

fn default_path() -> Result<PathBuf, CoreError> {
    crate::paths::data_dir()
        .map(|d| d.join("index/sessions.db"))
        .ok_or_else(|| CoreError::Invalid { msg: "cannot determine asm data directory".into() })
}
