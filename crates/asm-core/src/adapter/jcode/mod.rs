//! jcode adapter (https://github.com/1jehuang/jcode).
//!
//! Store layout, read from the project's own source:
//! - Base directory is `$JCODE_HOME`, else `~/.jcode`.
//! - Each session is a **whole-snapshot** `sessions/<id>.json` plus an
//!   append-only `sessions/<id>.journal.jsonl`. That is a third storage
//!   shape: Claude Code appends one JSONL per session, OpenCode keeps rows
//!   in SQLite, jcode rewrites a single JSON document.
//! - Liveness is a file per running session under `active_pids/`, whose
//!   contents are the owning PID (`streaming_pids/` marks one that is
//!   mid-response, `internal_pids/` marks spawned/debug sessions).
//!
//! The snapshot shape matters for listing: because the whole conversation
//! lives in the same document as the metadata, and `working_dir` is
//! serialized *after* `messages`, there is no cheap head-scan. Listing
//! therefore streams the document through serde with the message array
//! deserialized into `IgnoredAny`, which walks it without building it.

mod live;
mod export_ir;
mod store;
mod write;

use std::path::{Path, PathBuf};

use crate::CoreError;
use crate::model::{AgentKind, Session};

use super::{AgentRead, AgentWrite, Capabilities, DetectResult, SessionFilter};

pub struct JCodeAdapter {
    root: PathBuf,
}

impl JCodeAdapter {
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        JCodeAdapter { root: root.into() }
    }

    pub fn detect_default() -> Option<Self> {
        let root = default_root()?;
        root.join("sessions").is_dir().then_some(JCodeAdapter { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }
}

fn default_root() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("JCODE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Some(home);
    }
    etcetera::home_dir().ok().map(|h| h.join(".jcode"))
}

impl AgentRead for JCodeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::JCode
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            list: true,
            liveness: true,
            resume_native: true,
            export_ir: true,
            rename: true,
            archive: true,
            delete: true,
            ..Capabilities::default()
        }
    }

    fn detect(&self) -> DetectResult {
        DetectResult {
            agent: AgentKind::JCode,
            store_found: self.sessions_dir().is_dir(),
            store_root: self.root.clone(),
        }
    }

    fn sessions(&self, filter: &SessionFilter) -> Result<Vec<Session>, CoreError> {
        store::sessions(self, filter)
    }

    fn resume_command(&self, session: &Session) -> Option<std::process::Command> {
        let mut cmd = std::process::Command::new("jcode");
        // jcode resolves `--resume` by memorable short name or by id; the
        // short name is what its own UI shows, so prefer it when present.
        let target = session.slug.clone().unwrap_or_else(|| session.handle.native_id.clone());
        cmd.arg("--resume").arg(target).current_dir(&session.project_root);
        Some(cmd)
    }

    fn export_ir(&self, session: &Session) -> Result<crate::ir::IrSession, CoreError> {
        export_ir::export_ir(self, session)
    }
}

/// What jcode can be asked to do, and what it cannot.
///
/// Rename goes through `jcode session rename` — jcode's own command, which
/// writes the `custom_title` its picker reads. Archive and delete move or
/// remove whole files. What stays unsupported is anything needing the
/// snapshot itself rewritten: the whole conversation lives in that one
/// document, and there is no sanctioned command for moving a session to a
/// different directory or materializing an imported one.
impl AgentWrite for JCodeAdapter {
    fn rename(&self, session: &Session, title: &str) -> Result<(), CoreError> {
        write::rename(self, session, title)
    }

    fn archive(&self, session: &Session) -> Result<super::ArchiveOutcome, CoreError> {
        write::archive(self, session)
    }

    fn unarchive(&self, _session: &Session) -> Result<(), CoreError> {
        // Archived sessions leave jcode's store entirely, so they no longer
        // resolve as sessions; restoring goes through the archive by id.
        Err(unsupported("unarchive-by-ref"))
    }

    fn relocate(
        &self,
        _session: &Session,
        _new_dir: &Path,
    ) -> Result<super::RelocateOutcome, CoreError> {
        Err(unsupported("move (the working directory lives inside the snapshot)"))
    }

    fn delete(&self, session: &Session) -> Result<super::DeleteReport, CoreError> {
        write::delete(self, session)
    }

    fn import_ir(
        &self,
        _ir: &crate::ir::IrSession,
        _opts: &crate::import::ImportOpts,
    ) -> Result<crate::import::ImportOutcome, CoreError> {
        Err(unsupported("import into jcode (no sanctioned import command)"))
    }
}

fn unsupported(op: &'static str) -> CoreError {
    CoreError::NotSupported { agent: "jcode", op }
}
