//! Agent adapters. Read and write capabilities are split so read-only
//! adapters (e.g. a future Cursor IDE adapter) can be honest about what
//! they support; UIs query `Capabilities` instead of guessing.
//!
//! In-tree adapters are dispatched through the closed `Adapter` enum —
//! adding an agent makes the compiler list every place that must handle it.

pub mod claude;
pub mod opencode;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::CoreError;
use crate::model::{AgentKind, Session};

/// What an adapter supports. Flags flip on as milestones land.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Capabilities {
    // read side
    pub list: bool,
    pub read_transcript: bool,
    pub liveness: bool,
    // write side
    pub rename: bool,
    pub archive: bool,
    pub relocate: bool,
    pub delete: bool,
    pub import_ir: bool,
    pub export_ir: bool,
    /// Can we hand back a command that resumes the session in its own agent?
    pub resume_native: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectResult {
    pub agent: AgentKind,
    pub store_root: PathBuf,
    pub store_found: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SessionFilter {
    pub agent: Option<AgentKind>,
    /// Only sessions whose project root equals this path.
    pub project: Option<PathBuf>,
    /// Include subagent/child sessions (hidden by default, like the agents'
    /// own pickers).
    pub include_children: bool,
}

impl SessionFilter {
    pub fn matches(&self, session: &Session) -> bool {
        if let Some(agent) = self.agent
            && session.handle.agent != agent
        {
            return false;
        }
        if let Some(project) = &self.project
            && &session.project_root != project
        {
            return false;
        }
        true
    }
}

/// Read-side adapter surface (milestone M0/M1 subset; transcript streaming
/// and IR export join in M1).
pub trait AgentRead {
    fn kind(&self) -> AgentKind;
    fn capabilities(&self) -> Capabilities;
    fn detect(&self) -> DetectResult;
    fn sessions(&self, filter: &SessionFilter) -> Result<Vec<Session>, CoreError>;
    /// Command that resumes this session interactively in its own agent,
    /// with the working directory already set to the session's project.
    fn resume_command(&self, session: &Session) -> Option<std::process::Command>;
    /// Export the full session (metadata + conversation) as Session IR.
    fn export_ir(&self, session: &Session) -> Result<crate::ir::IrSession, CoreError>;
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ArchiveOutcome {
    /// Where the session's native files went (None when the agent has a
    /// native archived flag instead, like OpenCode's `time_archived`).
    pub archived_to: Option<PathBuf>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RelocateOutcome {
    pub new_transcript: Option<PathBuf>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DeleteReport {
    pub backup_dir: Option<PathBuf>,
    pub removed: Vec<PathBuf>,
}

/// Write-side adapter surface. Every implementation must refuse to touch a
/// live session / busy store and back up before destroying data.
pub trait AgentWrite: AgentRead {
    fn rename(&self, session: &Session, title: &str) -> Result<(), CoreError>;
    fn archive(&self, session: &Session) -> Result<ArchiveOutcome, CoreError>;
    fn unarchive(&self, session: &Session) -> Result<(), CoreError>;
    fn relocate(&self, session: &Session, new_dir: &Path) -> Result<RelocateOutcome, CoreError>;
    fn delete(&self, session: &Session) -> Result<DeleteReport, CoreError>;
    /// Materialize an IR session as a native session of this agent.
    fn import_ir(
        &self,
        ir: &crate::ir::IrSession,
        opts: &crate::import::ImportOpts,
    ) -> Result<crate::import::ImportOutcome, CoreError>;
}

/// Closed set of in-tree adapters (enum dispatch).
pub enum Adapter {
    ClaudeCode(claude::ClaudeAdapter),
    OpenCode(opencode::OpenCodeAdapter),
}

impl Adapter {
    /// All adapters whose store exists on this machine.
    pub fn available() -> Vec<Adapter> {
        let mut adapters = Vec::new();
        if let Some(claude) = claude::ClaudeAdapter::detect_default() {
            adapters.push(Adapter::ClaudeCode(claude));
        }
        if let Some(opencode) = opencode::OpenCodeAdapter::detect_default() {
            adapters.push(Adapter::OpenCode(opencode));
        }
        adapters
    }
}

macro_rules! dispatch {
    ($self:ident, $a:ident => $body:expr) => {
        match $self {
            Adapter::ClaudeCode($a) => $body,
            Adapter::OpenCode($a) => $body,
        }
    };
}

impl AgentRead for Adapter {
    fn kind(&self) -> AgentKind {
        dispatch!(self, a => a.kind())
    }

    fn capabilities(&self) -> Capabilities {
        dispatch!(self, a => a.capabilities())
    }

    fn detect(&self) -> DetectResult {
        dispatch!(self, a => a.detect())
    }

    fn sessions(&self, filter: &SessionFilter) -> Result<Vec<Session>, CoreError> {
        dispatch!(self, a => a.sessions(filter))
    }

    fn resume_command(&self, session: &Session) -> Option<std::process::Command> {
        dispatch!(self, a => a.resume_command(session))
    }

    fn export_ir(&self, session: &Session) -> Result<crate::ir::IrSession, CoreError> {
        dispatch!(self, a => a.export_ir(session))
    }
}

impl AgentWrite for Adapter {
    fn rename(&self, session: &Session, title: &str) -> Result<(), CoreError> {
        dispatch!(self, a => a.rename(session, title))
    }

    fn archive(&self, session: &Session) -> Result<ArchiveOutcome, CoreError> {
        dispatch!(self, a => a.archive(session))
    }

    fn unarchive(&self, session: &Session) -> Result<(), CoreError> {
        dispatch!(self, a => a.unarchive(session))
    }

    fn relocate(&self, session: &Session, new_dir: &Path) -> Result<RelocateOutcome, CoreError> {
        dispatch!(self, a => a.relocate(session, new_dir))
    }

    fn delete(&self, session: &Session) -> Result<DeleteReport, CoreError> {
        dispatch!(self, a => a.delete(session))
    }

    fn import_ir(
        &self,
        ir: &crate::ir::IrSession,
        opts: &crate::import::ImportOpts,
    ) -> Result<crate::import::ImportOutcome, CoreError> {
        dispatch!(self, a => a.import_ir(ir, opts))
    }
}
