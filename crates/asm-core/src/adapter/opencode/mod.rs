//! OpenCode adapter.
//!
//! Store layout (verified against OpenCode 1.17.18, storage generation 2):
//! - Everything lives in one SQLite database:
//!   `$XDG_DATA_HOME/opencode/opencode.db` (default `~/.local/share/...`),
//!   with LIVE `-wal`/`-shm` sidecars while OpenCode runs.
//! - WAL rule: open read-only WITHOUT `immutable=1` — immutable silently
//!   skips the WAL and returns stale data. Never copy the db alone.
//! - Write rule: all ingestion goes through the `opencode` CLI
//!   (`opencode import`/`export`); raw writes are the exception, gated on
//!   the lock dir (`$XDG_STATE_HOME/opencode/locks/`) being empty.
//! - Timestamps are epoch MILLISECONDS. `session.parent_id` marks
//!   subagent/child sessions. Rows from older CLI generations (1.2.x) have
//!   NULL path/agent/model — both generations coexist in one table.
//!
//! OpenCode 2.x (verified against 2.0.25) keeps the same file but a different
//! schema: sessions in `session_v2`, messages in `session_message`, both
//! projections of an event log. asm reads that schema (`v2.rs`) and writes it
//! only through the CLI (`cli2.rs`; rename and delete in `write_v2.rs`, the
//! hub in `hub_v2.rs`, import in `import_v2.rs`): never SQL, because a
//! running service owns those rows. A 2.x store has no archive and no move,
//! and those stay unsupported. The schema is detected per call from
//! `sqlite_master`, so one adapter value follows an OpenCode upgrade without
//! being rebuilt.
//!
//! Detection FAILS CLOSED for writes. A database that cannot be inspected
//! right now (`Schema::Unknown`: locked, unreadable) is read as 1.x, as ever,
//! but every write refuses it: the 1.x SQL against a 2.x store destroys rows.
//! A 2.x database also keeps the 1.x tables it migrated from, and OpenCode
//! migrates in the background; while sessions exist only there, they are
//! listed read-only and every write refuses. Each operation decides the
//! schema once (`write_schema`) and acts on that.

mod cli2;
pub(crate) mod hub;
mod hub_v2;
mod import_ir;
mod import_v2;
mod live;
pub(crate) mod export_ir;
mod store;
#[cfg(test)]
mod tests_v2;
pub(crate) mod v2;
mod write;
mod write_v2;

use std::path::{Path, PathBuf};

use crate::CoreError;
use crate::model::{AgentKind, Session};

const AGENT: &str = "opencode";

use super::{
    AgentRead, AgentWrite, ArchiveOutcome, Capabilities, DeleteReport, DetectResult,
    RelocateOutcome, SessionFilter,
};

pub struct OpenCodeAdapter {
    db: PathBuf,
    lock_dir: Option<PathBuf>,
    /// How to run the 2.x CLI; `None` is `opencode` on the PATH.
    cli: Option<cli2::Cli>,
    /// Where backups go; `None` is asm's own backup directory.
    backup_root: Option<PathBuf>,
}

/// Which generation of OpenCode's schema a database holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schema {
    /// 1.x: `session`, `message`, `part`.
    V1,
    /// 2.x: `session_v2`, `session_message` (event-sourced projections).
    V2,
    /// There is no database file yet (a machine that never ran OpenCode).
    Absent,
    /// Neither: not a store asm reads. Skipped like an agent that is not
    /// installed, instead of every listing failing on `no such table`.
    Unsupported,
    /// The file exists but could not be inspected (locked, busy, unreadable,
    /// not a database). Reads treat it as V1, so a moment's contention never
    /// hides the sessions; WRITES refuse it, because running the 1.x SQL
    /// against a 2.x store destroys rows.
    Unknown,
}

/// The schema `db` holds.
///
/// `session_v2` wins over `session`: a 2.x database keeps the 1.x tables it
/// migrated from (see [`unmigrated`] for sessions it has not copied yet).
pub fn detect_schema(db: &Path) -> Schema {
    use rusqlite::{Connection, OpenFlags};
    if !db.is_file() {
        return Schema::Absent;
    }
    let inspect = || -> rusqlite::Result<Schema> {
        let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        // A writer holding the file for a moment is not "unknown".
        conn.busy_timeout(std::time::Duration::from_millis(300))?;
        schema_of(&conn)
    };
    inspect().unwrap_or(Schema::Unknown)
}

/// [`detect_schema`] on a connection that is open already. Errors are the
/// caller's to treat as "cannot tell"; they are never read as a schema.
pub(crate) fn schema_of(conn: &rusqlite::Connection) -> rusqlite::Result<Schema> {
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
    let tables = stmt.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<std::collections::HashSet<_>, _>>()?;
    let has = |all: &[&str]| all.iter().all(|t| tables.contains(*t));
    Ok(if has(&["session_v2", "session_message"]) {
        Schema::V2
    } else if has(&["session", "message", "part"]) {
        Schema::V1
    } else {
        Schema::Unsupported
    })
}

/// How many sessions of a database's 1.x tables OpenCode 2.x has not copied
/// into `session_v2` yet. 2.x migrates in a background job (a cursor in the
/// `kv` row `migration.v1-v2`), never drops the 1.x tables, and a 1.x binary
/// can still add rows to them afterwards. `Ok(0)` when the database does not
/// hold both table sets.
pub fn unmigrated(db: &Path) -> rusqlite::Result<u64> {
    use rusqlite::{Connection, OpenFlags};
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.busy_timeout(std::time::Duration::from_millis(300))?;
    let has = |t: &str| conn.query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1", [t], |_| Ok(())).is_ok();
    if !(has("session") && has("session_v2")) {
        return Ok(0);
    }
    conn.query_row(
        "SELECT count(*) FROM session s WHERE NOT EXISTS (SELECT 1 FROM session_v2 v WHERE v.id = s.id)",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n.max(0) as u64)
}

/// The sentence for a store whose sessions OpenCode 2.x is still migrating.
pub(crate) fn unmigrated_note(n: u64) -> String {
    format!(
        "{n} session{} in this OpenCode database {} not been migrated by OpenCode 2.x yet; start OpenCode once so it \
         finishes (asm lists them read-only, and changes nothing in this store until then)",
        if n == 1 { "" } else { "s" },
        if n == 1 { "has" } else { "have" },
    )
}


/// Is this session one of the 1.x rows a 2.x database has not migrated yet
/// (listed from `session`, read-only)?
pub(crate) fn is_unmigrated_row(session: &Session) -> bool {
    matches!(&session.handle.location, crate::model::SessionLocation::SqliteRow { table, .. } if table == "session")
}

/// A store still in the 1.x format with a 2.x binary installed: 2.x migrates
/// it on its first start, and until then neither generation's code is safe
/// to run against it (2.x reads a flagless `import <file>` as a directory).
pub(crate) fn v1_store_with_v2_binary() -> CoreError {
    CoreError::Invalid {
        msg: "this OpenCode database is still in the 1.x format but the installed OpenCode is 2.x: start OpenCode 2.x \
              once so it migrates your sessions, then retry. Nothing was changed"
            .into(),
    }
}

/// The refusal for an operation OpenCode 2.x has no way to do.
fn v2_unsupported(op: &str) -> CoreError {
    CoreError::Invalid { msg: format!("OpenCode 2.x has no {op}") }
}

impl OpenCodeAdapter {
    pub fn with_db(db: impl Into<PathBuf>) -> Self {
        OpenCodeAdapter { db: db.into(), lock_dir: None, cli: None, backup_root: None }
    }

    /// Run the 2.x CLI as `cli` says (tests give it a private environment).
    #[cfg(test)]
    fn with_cli(mut self, cli: cli2::Cli) -> Self {
        self.cli = Some(cli);
        self
    }

    #[cfg(test)]
    fn with_backup_root(mut self, dir: impl Into<PathBuf>) -> Self {
        self.backup_root = Some(dir.into());
        self
    }

    /// Use the `opencode` at `bin` instead of the one on the PATH, for the
    /// work that asks the installed OpenCode what it is (its version decides
    /// what a store that does not exist yet will be) and for 2.x changes.
    pub fn with_binary(mut self, bin: impl Into<PathBuf>) -> Self {
        let state = self.state_dir().map(Path::to_path_buf);
        self.cli = Some(cli2::Cli::ambient_at(bin.into(), state));
        self
    }

    /// `<state>/opencode`, where a 2.x service records itself.
    fn state_dir(&self) -> Option<&Path> {
        self.cli.as_ref().and_then(|c| c.state_dir()).or_else(|| self.lock_dir.as_deref()?.parent())
    }

    fn cli(&self) -> cli2::Cli {
        self.cli.clone().unwrap_or_else(|| cli2::Cli::ambient(self.state_dir().map(Path::to_path_buf)))
    }

    /// A fresh backup directory for session `id`.
    fn backup_dir(&self, id: &str) -> Option<PathBuf> {
        match &self.backup_root {
            Some(root) => Some(root.join(id).join(jiff::Timestamp::now().as_millisecond().to_string())),
            None => crate::paths::backup_dir("opencode", id),
        }
    }

    pub fn with_lock_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.lock_dir = Some(dir.into());
        self
    }

    /// The default store, whether or not OpenCode has created it yet — a
    /// pull onto a machine that never ran it lets `opencode import` do so.
    pub fn default_store() -> Option<Self> {
        let lock_dir = state_dir().map(|d| d.join("opencode/locks"));
        default_db().map(|db| OpenCodeAdapter { db, lock_dir, cli: None, backup_root: None })
    }

    pub fn detect_default() -> Option<Self> {
        let db = default_db()?;
        let lock_dir = state_dir().map(|d| d.join("opencode/locks"));
        (db.is_file() && detect_schema(&db) != Schema::Unsupported)
            .then_some(OpenCodeAdapter { db, lock_dir, cli: None, backup_root: None })
    }

    pub fn db(&self) -> &Path {
        &self.db
    }

    /// The schema the store holds right now; see [`detect_schema`]. An
    /// adapter with no database at all (hub planning asks what an agent
    /// supports) is V1, the full set.
    pub fn schema(&self) -> Schema {
        if self.db.as_os_str().is_empty() { Schema::V1 } else { detect_schema(&self.db) }
    }

    /// What `asm doctor` and `asm list` should say about this store beyond
    /// its sessions, if anything: sessions OpenCode 2.x has not migrated yet.
    pub fn store_note(&self) -> Option<String> {
        if self.schema() != Schema::V2 {
            return None;
        }
        match unmigrated(&self.db) {
            Ok(0) | Err(_) => None,
            Ok(n) => Some(unmigrated_note(n)),
        }
    }

    /// For reads: is this a 2.x store? An unreadable one is not (see
    /// [`Schema::Unknown`]).
    pub(crate) fn is_v2(&self) -> bool {
        self.schema() == Schema::V2
    }

    /// The schema a WRITE may act on, decided once per operation: V1 or V2,
    /// or `Absent` (nothing there yet; the caller follows the installed
    /// binary). Refuses what it cannot be sure of: a store it could not
    /// inspect (the 1.x SQL against a 2.x store destroys rows), one that is
    /// not an OpenCode store, and a 2.x store still migrating its 1.x
    /// sessions (a change now would be to a store only half of which asm
    /// can see).
    pub(crate) fn write_schema(&self) -> Result<Schema, CoreError> {
        match self.schema() {
            schema @ (Schema::V1 | Schema::Absent) => Ok(schema),
            Schema::V2 => match unmigrated(&self.db) {
                Ok(0) => Ok(Schema::V2),
                Ok(n) => Err(CoreError::Invalid { msg: format!("{}. Nothing was changed", unmigrated_note(n)) }),
                Err(_) => Err(Self::uninspectable()),
            },
            Schema::Unsupported => Err(CoreError::Invalid {
                msg: format!("{} is not an OpenCode store asm can change. Nothing was changed", self.db.display()),
            }),
            Schema::Unknown => Err(Self::uninspectable()),
        }
    }

    fn uninspectable() -> CoreError {
        CoreError::StoreBusy {
            agent: AGENT,
            detail: "its database could not be inspected just now (locked or unreadable), and changing it blind could \
                     damage it; nothing was changed"
                .into(),
        }
    }

    /// `write_schema` for a change to `session`: the store must also still
    /// be the generation the session was listed from.
    pub(crate) fn write_schema_for(&self, session: &Session) -> Result<Schema, CoreError> {
        let schema = self.write_schema()?;
        let listed_from_v2 = matches!(&session.handle.location, crate::model::SessionLocation::SqliteRow { table, .. } if table == "session_v2");
        let listed_from_v1 = matches!(&session.handle.location, crate::model::SessionLocation::SqliteRow { table, .. } if table == "session");
        if (listed_from_v2 && schema == Schema::V1) || (listed_from_v1 && schema == Schema::V2) {
            return Err(CoreError::Invalid {
                msg: "OpenCode changed this store's format since the session was listed; list again. Nothing was changed".into(),
            });
        }
        Ok(schema)
    }

    /// True when a LIVE OpenCode instance holds the store.
    ///
    /// Presence of a lock is not enough: OpenCode leaves its lock directory
    /// behind when it exits uncleanly, and treating those as "busy" would
    /// block every mutation forever. Each lock carries a pid and hostname,
    /// so liveness is checked the same way the Claude adapter checks its
    /// PID records.
    pub fn store_busy(&self) -> bool {
        self.locks().iter().any(|lock| lock.held)
    }

    /// Locks that exist but whose owner is gone — safe for the user to
    /// delete. Reported by `doctor`, never removed automatically: they
    /// belong to another program.
    pub fn stale_locks(&self) -> Vec<LockHolder> {
        self.locks().into_iter().filter(|lock| !lock.held).collect()
    }

    pub fn locks(&self) -> Vec<LockHolder> {
        let Some(lock_dir) = &self.lock_dir else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(lock_dir) else {
            return Vec::new();
        };
        entries.filter_map(Result::ok).map(|e| inspect_lock(&e.path())).collect()
    }
}

/// A lock in OpenCode's lock directory, and whether its owner still exists.
#[derive(Debug, Clone)]
pub struct LockHolder {
    pub path: PathBuf,
    pub pid: Option<u32>,
    pub hostname: Option<String>,
    /// True when we believe a live process still holds this lock.
    pub held: bool,
    pub reason: String,
}

/// How long an un-attributable lock (no readable metadata, or another
/// host's) is trusted after its `heartbeat` file was last touched.
///
/// This path is a fallback only. OpenCode writes a `heartbeat` file next to
/// the lock's `meta.json`, but whether it refreshes that file periodically
/// has NOT been verified — so this grace period is deliberately generous
/// and errs toward treating a lock as held. Locks owned by a pid on this
/// host never reach here; they are decided by whether the process exists.
const HEARTBEAT_GRACE: std::time::Duration = std::time::Duration::from_secs(300);

fn inspect_lock(path: &Path) -> LockHolder {
    let meta: Option<serde_json::Value> = std::fs::read(path.join("meta.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let pid = meta
        .as_ref()
        .and_then(|m| m.get("pid"))
        .and_then(serde_json::Value::as_u64)
        .map(|p| p as u32);
    let hostname = meta
        .as_ref()
        .and_then(|m| m.get("hostname"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);

    // Another machine's lock (a shared home directory): we cannot inspect
    // its process table, so fall back to the heartbeat.
    let same_host = match (&hostname, crate::process::hostname()) {
        (Some(lock_host), Some(here)) => *lock_host == here,
        _ => hostname.is_none(),
    };

    match (pid, same_host) {
        (Some(pid), true) => {
            let held = crate::process::alive(pid);
            let reason = if held {
                format!("held by pid {pid}")
            } else {
                format!("stale: pid {pid} is no longer running")
            };
            LockHolder { path: path.to_path_buf(), pid: Some(pid), hostname, held, reason }
        }
        _ => {
            // No usable pid: trust a recent heartbeat, distrust an old one.
            let fresh = heartbeat_age(path).is_some_and(|age| age < HEARTBEAT_GRACE);
            let reason = match (&hostname, fresh) {
                (Some(host), true) => format!("held on another host ({host}), heartbeat is recent"),
                (Some(host), false) => format!("stale: lock from host {host}, heartbeat is old"),
                (None, true) => "held: unreadable lock metadata, heartbeat is recent".to_string(),
                (None, false) => "stale: unreadable lock metadata and an old heartbeat".to_string(),
            };
            LockHolder { path: path.to_path_buf(), pid, hostname, held: fresh, reason }
        }
    }
}

fn heartbeat_age(lock: &Path) -> Option<std::time::Duration> {
    // Directory locks keep a `heartbeat` file; a bare file lock is its own
    // heartbeat.
    let candidate = lock.join("heartbeat");
    let target = if candidate.exists() { candidate } else { lock.to_path_buf() };
    std::fs::metadata(target).ok()?.modified().ok()?.elapsed().ok()
}


fn default_db() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| etcetera::home_dir().ok().map(|h| h.join(".local/share")));
    base.map(|b| b.join("opencode/opencode.db"))
}

fn state_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| etcetera::home_dir().ok().map(|h| h.join(".local/state")))
}

impl AgentRead for OpenCodeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::OpenCode
    }

    fn capabilities(&self) -> Capabilities {
        // 2.x: rename, delete and import work through the CLI; there is no
        // archive or move to offer. `send_message` stays off too: `run
        // --format json`'s event vocabulary has not been verified against
        // 2.x, and parsing it wrongly would show a broken chat.
        // Only a store known to be 1.x offers them: one that cannot be
        // inspected right now is not assumed to be (see `Schema::Unknown`).
        let v1 = self.schema() == Schema::V1;
        Capabilities {
            list: true,
            read_transcript: true,
            liveness: true,
            resume_native: true,
            send_message: v1,
            rename: true,
            archive: v1,
            delete: true,
            export_ir: true,
            import_ir: true,
            ..Capabilities::default()
        }
    }

    fn detect(&self) -> DetectResult {
        DetectResult {
            agent: AgentKind::OpenCode,
            store_found: self.db.is_file(),
            store_root: self.db.clone(),
        }
    }

    fn sessions(&self, filter: &SessionFilter) -> Result<Vec<Session>, CoreError> {
        store::sessions(self, filter)
    }

    fn resume_command(&self, session: &Session) -> Option<std::process::Command> {
        let mut cmd = std::process::Command::new("opencode");
        cmd.arg("-s")
            .arg(&session.handle.native_id)
            .current_dir(&session.project_root);
        Some(cmd)
    }

    fn export_ir(&self, session: &Session) -> Result<crate::ir::IrSession, CoreError> {
        export_ir::export_ir(self, session)
    }
}

impl AgentWrite for OpenCodeAdapter {
    fn rename(&self, session: &Session, title: &str) -> Result<(), CoreError> {
        match self.write_schema_for(session)? {
            Schema::V2 => write_v2::rename(self, session, title),
            _ => write::rename(self, session, title),
        }
    }

    fn archive(&self, session: &Session) -> Result<ArchiveOutcome, CoreError> {
        match self.write_schema_for(session)? {
            Schema::V2 => Err(v2_unsupported("archive")),
            _ => write::archive(self, session),
        }
    }

    fn unarchive(&self, session: &Session) -> Result<(), CoreError> {
        match self.write_schema_for(session)? {
            Schema::V2 => Err(v2_unsupported("archive")),
            _ => write::unarchive(self, session),
        }
    }

    fn relocate(&self, session: &Session, new_dir: &Path) -> Result<RelocateOutcome, CoreError> {
        match self.write_schema_for(session)? {
            Schema::V2 => Err(CoreError::Invalid {
                msg: "OpenCode 2.x cannot move a session: it only queues a move for a persistent server, and a one-shot \
                      run would leave the session claimed"
                    .into(),
            }),
            _ => write::relocate(self, session, new_dir),
        }
    }

    fn delete(&self, session: &Session) -> Result<DeleteReport, CoreError> {
        match self.write_schema_for(session)? {
            Schema::V2 => write_v2::delete(self, session),
            _ => write::delete(self, session),
        }
    }

    fn import_ir(
        &self,
        ir: &crate::ir::IrSession,
        opts: &crate::import::ImportOpts,
    ) -> Result<crate::import::ImportOutcome, CoreError> {
        let v2 = match self.write_schema()? {
            Schema::V2 => true,
            // No store yet: the installed binary decides what will create it.
            // (Never a flagless command: with no 2.x binary there is nothing
            // here to create it with, and the 1.x path says so.)
            Schema::Absent => self.cli().is_v2(),
            _ if !opts.dry_run && self.cli().is_v2() => return Err(v1_store_with_v2_binary()),
            _ => false,
        };
        if v2 {
            return import_v2::import_ir(self, ir, opts);
        }
        import_ir::import_ir(self, ir, opts)
    }
}
