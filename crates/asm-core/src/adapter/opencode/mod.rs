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

mod live;
mod export_ir;
mod import_ir;
mod store;
mod write;

use std::path::{Path, PathBuf};

use crate::CoreError;
use crate::model::{AgentKind, Session};

use super::{
    AgentRead, AgentWrite, ArchiveOutcome, Capabilities, DeleteReport, DetectResult,
    RelocateOutcome, SessionFilter,
};

pub struct OpenCodeAdapter {
    db: PathBuf,
    lock_dir: Option<PathBuf>,
}

impl OpenCodeAdapter {
    pub fn with_db(db: impl Into<PathBuf>) -> Self {
        OpenCodeAdapter { db: db.into(), lock_dir: None }
    }

    pub fn with_lock_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.lock_dir = Some(dir.into());
        self
    }

    pub fn detect_default() -> Option<Self> {
        let db = default_db()?;
        let lock_dir = state_dir().map(|d| d.join("opencode/locks"));
        db.is_file().then_some(OpenCodeAdapter { db, lock_dir })
    }

    pub fn db(&self) -> &Path {
        &self.db
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
    let same_host = match (&hostname, this_hostname()) {
        (Some(lock_host), Some(here)) => *lock_host == here,
        _ => hostname.is_none(),
    };

    match (pid, same_host) {
        (Some(pid), true) => {
            let held = std::path::Path::new(&format!("/proc/{pid}")).exists();
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

fn this_hostname() -> Option<String> {
    if let Ok(name) = std::env::var("HOSTNAME")
        && !name.is_empty()
    {
        return Some(name);
    }
    for path in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
        if let Ok(name) = std::fs::read_to_string(path) {
            let name = name.trim().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
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
        Capabilities {
            list: true,
            liveness: true,
            resume_native: true,
            send_message: true,
            rename: true,
            archive: true,
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
        write::rename(self, session, title)
    }

    fn archive(&self, session: &Session) -> Result<ArchiveOutcome, CoreError> {
        write::archive(self, session)
    }

    fn unarchive(&self, session: &Session) -> Result<(), CoreError> {
        write::unarchive(self, session)
    }

    fn relocate(&self, session: &Session, new_dir: &Path) -> Result<RelocateOutcome, CoreError> {
        write::relocate(self, session, new_dir)
    }

    fn delete(&self, session: &Session) -> Result<DeleteReport, CoreError> {
        write::delete(self, session)
    }

    fn import_ir(
        &self,
        ir: &crate::ir::IrSession,
        opts: &crate::import::ImportOpts,
    ) -> Result<crate::import::ImportOutcome, CoreError> {
        import_ir::import_ir(self, ir, opts)
    }
}
