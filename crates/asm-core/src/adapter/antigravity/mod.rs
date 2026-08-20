//! Google Antigravity adapter (the `agy` CLI).
//!
//! Antigravity is what Gemini CLI became: Google now refuses Gemini Code
//! Assist for individuals with `IneligibleTierError: migrate to the
//! Antigravity suite`, and the replacement keeps a completely different
//! store.
//!
//! Layout, read from a real install:
//! - Root is `~/.gemini/antigravity-cli` — inside the old `.gemini`
//!   directory, but sharing nothing else with it.
//! - Each conversation is **its own SQLite database**,
//!   `conversations/<uuid>.db`, holding a `steps` table whose payloads are
//!   protobuf blobs with no published schema. That is a fourth storage
//!   shape again: one file per conversation, binary inside.
//! - `cache/conversation_metadata.json` carries the title, the step count
//!   and the last-modified time for every conversation.
//! - `cache/last_conversations.json` maps a workspace directory to the last
//!   conversation opened there. It is the *only* place a conversation is
//!   tied to a project: neither the per-conversation database nor
//!   `conversation_summaries.db` records a working directory
//!   (`workspace_uris` is empty for CLI conversations).
//! - `brain/<uuid>/.system_generated/logs/transcript.jsonl` is a plain
//!   JSONL rendering of the same steps, written by antigravity itself.
//!
//! The transcript is read from that JSONL rather than from the protobuf.
//! Both come from the same code and carry the same steps, and reading the
//! blobs would mean pinning field numbers of an unpublished schema — a
//! guess that breaks silently on the next release, whereas a missing JSONL
//! file is a gap asm can report.

mod export_ir;
mod live;
mod store;

use std::path::{Path, PathBuf};

use crate::CoreError;
use crate::model::{AgentKind, Session};

use super::{AgentRead, AgentWrite, Capabilities, DetectResult, SessionFilter};

pub struct AntigravityAdapter {
    root: PathBuf,
}

impl AntigravityAdapter {
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        AntigravityAdapter { root: root.into() }
    }

    pub fn detect_default() -> Option<Self> {
        let root = default_root()?;
        root.join("conversations").is_dir().then_some(AntigravityAdapter { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn conversations_dir(&self) -> PathBuf {
        self.root.join("conversations")
    }

    /// Title, step count and last-modified time per conversation.
    pub fn metadata_file(&self) -> PathBuf {
        self.root.join("cache/conversation_metadata.json")
    }

    /// Workspace directory -> the last conversation opened there.
    pub fn workspaces_file(&self) -> PathBuf {
        self.root.join("cache/last_conversations.json")
    }

    /// Where antigravity writes its own JSONL rendering of a conversation.
    pub fn transcript_of(&self, id: &str) -> PathBuf {
        self.root.join("brain").join(id).join(".system_generated/logs/transcript.jsonl")
    }
}

/// `agy` exposes no environment override for its data directory, so asm
/// provides one of its own. Tests need to point at a fixture store, and
/// hard-coding `~/.gemini/antigravity-cli` would make them untestable.
fn default_root() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("ASM_ANTIGRAVITY_ROOT")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Some(root);
    }
    etcetera::home_dir().ok().map(|h| h.join(".gemini/antigravity-cli"))
}

impl AgentRead for AntigravityAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Antigravity
    }

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
            agent: AgentKind::Antigravity,
            store_found: self.conversations_dir().is_dir(),
            store_root: self.root.clone(),
        }
    }

    fn sessions(&self, filter: &SessionFilter) -> Result<Vec<Session>, CoreError> {
        store::sessions(self, filter)
    }

    fn resume_command(&self, session: &Session) -> Option<std::process::Command> {
        let mut cmd = std::process::Command::new("agy");
        cmd.arg("--conversation").arg(&session.handle.native_id);
        // A conversation whose workspace was never recorded has to resume
        // wherever the user is; sending it to `/` would be worse.
        if session.project_root.is_dir() {
            cmd.current_dir(&session.project_root);
        }
        Some(cmd)
    }

    fn export_ir(&self, session: &Session) -> Result<crate::ir::IrSession, CoreError> {
        export_ir::export_ir(self, session)
    }
}

/// Read-only. Every step lives in a protobuf blob inside a per-conversation
/// database, and `agy` ships no rename, archive or import command to write
/// through — so there is nothing to write with except guessed field numbers.
impl AgentWrite for AntigravityAdapter {
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
        Err(unsupported("import into antigravity"))
    }
}

fn unsupported(op: &'static str) -> CoreError {
    CoreError::NotSupported { agent: "antigravity", op }
}
