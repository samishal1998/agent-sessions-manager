//! Claude Code adapter.
//!
//! Store layout (verified against Claude Code 2.1.233):
//! - Root: `$CLAUDE_CONFIG_DIR` if set, else `~/.claude`.
//! - Transcripts: `<root>/projects/<encoded-cwd>/<sessionUuid>.jsonl`,
//!   append-only JSONL. The encoded directory name is LOSSY — the absolute
//!   `cwd` field inside the records is the only authoritative project path.
//! - Session identity is the transcript filename stem, which always equals
//!   the camelCase `sessionId` field. The snake_case `session_id` field is a
//!   DIFFERENT, sparse field that may hold unrelated ids — never read it.
//! - Live sessions are announced by PID-keyed records in `<root>/sessions/`.

mod live;
mod export_ir;
mod import_ir;
mod liveness;
mod path_encode;
mod store;
mod write;

pub use path_encode::encode_project_dir;

use std::path::{Path, PathBuf};

use crate::CoreError;
use crate::model::{AgentKind, Session};

use super::{
    AgentRead, AgentWrite, ArchiveOutcome, Capabilities, DeleteReport, DetectResult,
    RelocateOutcome, SessionFilter,
};

pub struct ClaudeAdapter {
    root: PathBuf,
    /// Path to the global `~/.claude.json` (cost/usage rollups). Optional so
    /// tests can run without one.
    global_config: Option<PathBuf>,
}

impl ClaudeAdapter {
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        ClaudeAdapter { root: root.into(), global_config: None }
    }

    pub fn with_global_config(mut self, path: impl Into<PathBuf>) -> Self {
        self.global_config = Some(path.into());
        self
    }

    /// Adapter over the default store, if one exists on this machine.
    pub fn detect_default() -> Option<Self> {
        let root = default_root()?;
        if !root.join("projects").is_dir() {
            return None;
        }
        let global_config = etcetera::home_dir().ok().map(|home| home.join(".claude.json"));
        Some(ClaudeAdapter { root, global_config: global_config.filter(|p| p.is_file()) })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Session ids present in more than one project directory — each one
    /// breaks Claude Code's cross-project `--resume` for that id.
    pub fn duplicate_session_ids(&self) -> Result<Vec<(String, Vec<PathBuf>)>, CoreError> {
        store::duplicate_session_ids(self)
    }
}

fn default_root() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    etcetera::home_dir().ok().map(|home| home.join(".claude"))
}

impl AgentRead for ClaudeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            list: true,
            read_transcript: true,
            liveness: true,
            resume_native: true,
            send_message: true,
            rename: true,
            archive: true,
            relocate: true,
            delete: true,
            export_ir: true,
            import_ir: true,
        }
    }

    fn detect(&self) -> DetectResult {
        DetectResult {
            agent: AgentKind::ClaudeCode,
            store_found: self.root.join("projects").is_dir(),
            store_root: self.root.clone(),
        }
    }

    fn sessions(&self, filter: &SessionFilter) -> Result<Vec<Session>, CoreError> {
        store::sessions(self, filter)
    }

    fn resume_command(&self, session: &Session) -> Option<std::process::Command> {
        let mut cmd = std::process::Command::new("claude");
        cmd.arg("--resume")
            .arg(&session.handle.native_id)
            .current_dir(&session.project_root);
        Some(cmd)
    }

    fn export_ir(&self, session: &Session) -> Result<crate::ir::IrSession, CoreError> {
        export_ir::export_ir(self, session)
    }
}

impl AgentWrite for ClaudeAdapter {
    fn rename(&self, session: &Session, title: &str) -> Result<(), CoreError> {
        write::rename(self, session, title)
    }

    fn archive(&self, session: &Session) -> Result<ArchiveOutcome, CoreError> {
        write::archive(self, session)
    }

    fn unarchive(&self, _session: &Session) -> Result<(), CoreError> {
        // Archived Claude sessions live in our archive store and no longer
        // resolve as sessions; restoration goes through `unarchive_by_id`.
        Err(CoreError::NotSupported { agent: "claude-code", op: "unarchive-by-ref" })
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
