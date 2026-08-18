use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::AgentKind;

/// A project directory an agent has sessions for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub agent: AgentKind,
    /// Authoritative absolute path (read from session records / store rows,
    /// never derived from encoded directory names).
    pub root: PathBuf,
    /// Agent-native project identifier, treated as opaque:
    /// Claude Code's encoded directory name, OpenCode's 40-hex project id.
    pub native_id: Option<String>,
    pub session_count: usize,
    pub last_updated: Option<Timestamp>,
}
