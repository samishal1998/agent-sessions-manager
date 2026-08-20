//! Codex CLI adapter (https://github.com/openai/codex).
//!
//! Store layout, read from a real 0.148.0 install:
//! - Base directory is `$CODEX_HOME`, else `~/.codex`.
//! - The transcript is an append-only JSONL "rollout" under
//!   `sessions/<YYYY>/<MM>/<DD>/rollout-<local-time>-<uuid>.jsonl`, every
//!   line `{timestamp, ordinal, type, payload}`.
//! - `state_5.sqlite` holds a `threads` row per session, and that row — not
//!   the filesystem — is what `codex resume`'s picker reads. It carries the
//!   `rollout_path`, the `cwd`, the title and the archived flag, so listing
//!   reads the database and only touches the JSONL when a transcript is
//!   actually asked for.
//!
//! Two traps the code has to respect:
//! - **The filename timestamp is local time with no offset.** It agrees
//!   with the record timestamps only on a UTC machine. Times come from the
//!   database or from `session_meta`, never from the filename.
//! - **A rollout can exist with no `threads` row** (the database is newer
//!   than the on-disk history, and codex adopts orphans lazily on resume).
//!   Listing therefore unions the two, because a session asm cannot see is
//!   the problem asm exists to solve.

mod export_ir;
mod store;

use std::path::{Path, PathBuf};

use crate::CoreError;
use crate::model::{AgentKind, Session};

use super::{AgentRead, AgentWrite, Capabilities, DetectResult, SessionFilter};

pub struct CodexAdapter {
    root: PathBuf,
}

impl CodexAdapter {
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        CodexAdapter { root: root.into() }
    }

    pub fn detect_default() -> Option<Self> {
        let root = default_root()?;
        // Either half is enough: a fresh install has the database before it
        // has any rollout, and a copied history has rollouts before it has
        // a database.
        (root.join("sessions").is_dir() || root.join("state_5.sqlite").is_file())
            .then_some(CodexAdapter { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// The metadata database backing `codex resume`'s picker.
    pub fn state_db(&self) -> PathBuf {
        self.root.join("state_5.sqlite")
    }
}

fn default_root() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Some(home);
    }
    etcetera::home_dir().ok().map(|h| h.join(".codex"))
}

impl AgentRead for CodexAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    /// Read-only for now. Codex owns its `threads` table through sqlx
    /// migrations and offers no import/rename/archive command of its own,
    /// so every write would be a raw write to a schema that has already
    /// moved five versions; that is not a promise worth making yet.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            list: true,
            read_transcript: true,
            export_ir: true,
            resume_native: true,
            send_message: true,
            ..Capabilities::default()
        }
    }

    fn detect(&self) -> DetectResult {
        DetectResult {
            agent: AgentKind::Codex,
            store_found: self.sessions_dir().is_dir() || self.state_db().is_file(),
            store_root: self.root.clone(),
        }
    }

    fn sessions(&self, filter: &SessionFilter) -> Result<Vec<Session>, CoreError> {
        store::sessions(self, filter)
    }

    fn resume_command(&self, session: &Session) -> Option<std::process::Command> {
        let mut cmd = std::process::Command::new("codex");
        cmd.arg("resume")
            .arg(&session.handle.native_id)
            .current_dir(&session.project_root);
        Some(cmd)
    }

    fn export_ir(&self, session: &Session) -> Result<crate::ir::IrSession, CoreError> {
        export_ir::export_ir(self, session)
    }
}

/// Codex is read-only in asm today.
///
/// Every mutation would have to be a raw write to `state_5.sqlite`, whose
/// schema is 48 sqlx migrations deep and still gaining columns, and codex
/// ships no `import`/`rename`/`archive` command to write through instead.
/// Reporting that honestly is better than a write that silently diverges
/// from what codex's own picker believes.
impl AgentWrite for CodexAdapter {
    fn rename(&self, _session: &Session, _title: &str) -> Result<(), CoreError> {
        Err(unsupported("rename"))
    }

    fn archive(&self, _session: &Session) -> Result<super::ArchiveOutcome, CoreError> {
        Err(unsupported("archive"))
    }

    fn unarchive(&self, _session: &Session) -> Result<(), CoreError> {
        Err(unsupported("unarchive"))
    }

    fn relocate(
        &self,
        _session: &Session,
        _new_dir: &Path,
    ) -> Result<super::RelocateOutcome, CoreError> {
        Err(unsupported("move"))
    }

    fn delete(&self, _session: &Session) -> Result<super::DeleteReport, CoreError> {
        Err(unsupported("delete"))
    }

    fn import_ir(
        &self,
        _ir: &crate::ir::IrSession,
        _opts: &crate::import::ImportOpts,
    ) -> Result<crate::import::ImportOutcome, CoreError> {
        Err(unsupported("import into codex"))
    }
}

fn unsupported(op: &'static str) -> CoreError {
    CoreError::NotSupported { agent: "codex", op }
}
