//! Pi (earendil-works/pi, the `pi` command) and its fork oh-my-pi (`omp`).
//!
//! Both keep one JSONL file per session, read from pi 1.5.0's source and
//! docs and from omp 18.11.3's:
//! - `<sessions>/<encoded cwd>/<timestamp>_<id>.jsonl`. The folder name is a
//!   lossy encoding of the working directory, and the two encode it
//!   differently (pi `--home-u-proj--`, omp `-proj` relative to home), so
//!   the directory always comes from the session header, never the folder.
//! - The header line is `{"type":"session","version":3,"id","timestamp","cwd"}`;
//!   every later line is an entry with `id`/`parentId`, forming a tree
//!   whose current branch ends at the last entry appended.
//! - omp puts a fixed 256-byte title slot (`{"type":"title",...}`) before
//!   the header and rewrites it in place on a rename. Pi names a session
//!   with `session_info` entries instead; the latest one wins.
//! - omp's subagents are sessions of their own, in the parent's artifact
//!   directory: `<parent stem>/<agent id>.jsonl`, nested for their children.
//!
//! Read-only for now: omp keeps titles in a database beside the files and
//! restores deleted sessions from `.bak` copies, so writes wait until they
//! can be checked against a real omp.

pub(crate) mod hub;
mod live;
pub(crate) mod export_ir;
mod store;

use std::path::{Path, PathBuf};

use crate::CoreError;
use crate::model::{AgentKind, Session};

use super::{AgentRead, AgentWrite, Capabilities, DetectResult, SessionFilter};

pub struct PiAdapter {
    kind: AgentKind,
    root: PathBuf,
    sessions: PathBuf,
}

impl PiAdapter {
    /// `kind` is `Pi` or `Omp`; `root` is its agent directory (`~/.pi/agent`,
    /// `~/.omp/agent`), with sessions under `root/sessions`.
    pub fn with_root(kind: AgentKind, root: impl Into<PathBuf>) -> Self {
        debug_assert!(matches!(kind, AgentKind::Pi | AgentKind::Omp));
        let root = root.into();
        PiAdapter { kind, sessions: root.join("sessions"), root }
    }

    /// The default store, whether or not the agent has created it yet.
    pub fn default_store(kind: AgentKind) -> Option<Self> {
        let root = default_root(kind)?;
        let mut adapter = Self::with_root(kind, root);
        // omp moves sessions to `$XDG_DATA_HOME/omp/sessions` once that
        // directory exists, dropping the `agent/` level.
        if kind == AgentKind::Omp
            && std::env::var_os("ASM_OMP_ROOT").is_none_or(|v| v.is_empty())
            && let Some(xdg) = xdg_data_home()
            && xdg.join("omp").is_dir()
        {
            adapter.sessions = xdg.join("omp").join("sessions");
        }
        Some(adapter)
    }

    pub fn detect_default(kind: AgentKind) -> Option<Self> {
        Self::default_store(kind).filter(|a| a.sessions.is_dir())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn sessions_dir(&self) -> &Path {
        &self.sessions
    }

    fn binary(&self) -> &'static str {
        if self.kind == AgentKind::Omp { "omp" } else { "pi" }
    }
}

fn non_empty(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

fn xdg_data_home() -> Option<PathBuf> {
    non_empty("XDG_DATA_HOME").or_else(|| etcetera::home_dir().ok().map(|h| h.join(".local/share")))
}

/// Pi honours `PI_CODING_AGENT_DIR`. omp reads that name too, along with
/// profiles and `PI_CONFIG_DIR`, so one variable could mean either store:
/// asm gives omp a name of its own, `ASM_OMP_ROOT`, and honours the
/// profile variable omp prefers.
fn default_root(kind: AgentKind) -> Option<PathBuf> {
    let home = || etcetera::home_dir().ok();
    match kind {
        AgentKind::Omp => non_empty("ASM_OMP_ROOT").or_else(|| {
            let base = home()?.join(".omp");
            Some(match std::env::var("OMP_PROFILE").ok().filter(|p| !p.is_empty() && p != "default") {
                Some(profile) => base.join("profiles").join(profile).join("agent"),
                None => base.join("agent"),
            })
        }),
        _ => non_empty("PI_CODING_AGENT_DIR").or_else(|| Some(home()?.join(".pi").join("agent"))),
    }
}

impl AgentRead for PiAdapter {
    fn kind(&self) -> AgentKind {
        self.kind
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            list: true,
            read_transcript: true,
            export_ir: true,
            resume_native: true,
            ..Capabilities::default()
        }
    }

    fn detect(&self) -> DetectResult {
        DetectResult { agent: self.kind, store_found: self.sessions.is_dir(), store_root: self.root.clone() }
    }

    fn sessions(&self, filter: &SessionFilter) -> Result<Vec<Session>, CoreError> {
        store::sessions(self, filter)
    }

    /// By path: both take a session file, and run it in its own directory.
    fn resume_command(&self, session: &Session) -> Option<std::process::Command> {
        let crate::model::SessionLocation::JsonlFile { path } = &session.handle.location else {
            return None;
        };
        let mut cmd = std::process::Command::new(self.binary());
        cmd.arg(if self.kind == AgentKind::Omp { "--resume" } else { "--session" })
            .arg(path)
            .current_dir(&session.project_root);
        Some(cmd)
    }

    fn export_ir(&self, session: &Session) -> Result<crate::ir::IrSession, CoreError> {
        export_ir::export_ir(session)
    }
}

impl AgentWrite for PiAdapter {
    fn rename(&self, _session: &Session, _title: &str) -> Result<(), CoreError> {
        Err(self.unsupported("rename"))
    }

    fn archive(&self, _session: &Session) -> Result<super::ArchiveOutcome, CoreError> {
        Err(self.unsupported("archive"))
    }

    fn unarchive(&self, _session: &Session) -> Result<(), CoreError> {
        Err(self.unsupported("unarchive"))
    }

    fn relocate(&self, _session: &Session, _new_dir: &Path) -> Result<super::RelocateOutcome, CoreError> {
        Err(self.unsupported("move"))
    }

    fn delete(&self, _session: &Session) -> Result<super::DeleteReport, CoreError> {
        Err(self.unsupported("delete"))
    }

    fn import_ir(
        &self,
        _ir: &crate::ir::IrSession,
        _opts: &crate::import::ImportOpts,
    ) -> Result<crate::import::ImportOutcome, CoreError> {
        Err(self.unsupported(if self.kind == AgentKind::Omp { "import into omp" } else { "import into pi" }))
    }
}

impl PiAdapter {
    fn unsupported(&self, op: &'static str) -> CoreError {
        CoreError::NotSupported { agent: self.binary(), op }
    }
}
