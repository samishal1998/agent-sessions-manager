use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::AgentKind;

/// Globally unique handle for a session; the address every operation takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRef {
    pub agent: AgentKind,
    /// Agent-native session id (Claude: transcript UUID == filename stem;
    /// OpenCode: `ses_...`).
    pub native_id: String,
    pub location: SessionLocation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionLocation {
    JsonlFile { path: PathBuf },
    SqliteRow { db: PathBuf, table: String },
    Archive { dir: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SessionStatus {
    /// A live agent process currently owns this session; never mutate it.
    Live { pid: Option<u32> },
    Idle,
    Archived,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(rename = "ref")]
    pub handle: SessionRef,
    pub title: Option<String>,
    pub slug: Option<String>,
    /// Project directory this session belongs to. For Claude Code this honors
    /// a trailing `relocated` marker over historical per-record `cwd` values.
    pub project_root: PathBuf,
    pub git_branch: Option<String>,
    pub created: Option<Timestamp>,
    pub updated: Option<Timestamp>,
    pub model: Option<String>,
    pub usage: Usage,
    pub status: SessionStatus,
    /// Native id of the parent session for subagent/child sessions.
    pub parent: Option<String>,
    /// Version of the agent CLI that (last) wrote this session; drives the
    /// tested-versions matrix for import.
    pub agent_version: Option<String>,
    /// Bytes this session's own data occupies in its agent's store: the
    /// transcript file, the snapshot and its journal, or the message and
    /// part rows. Sidecars that are cheap to find are included; anything
    /// that would need a directory walk per session is not, because this is
    /// computed for every session on every listing.
    #[serde(default)]
    pub size_bytes: Option<u64>,
}

impl Session {
    /// A short, human-usable handle for the session.
    ///
    /// Truncating the raw id is not enough, and it is not enough in two
    /// different ways.
    ///
    /// Every jcode id begins `session_`, so eight characters of one is
    /// eight characters of all of them; jcode's own handle is the memorable
    /// name it embeds in that id ("hog"), so use that instead.
    ///
    /// Codex ids are UUIDv7, whose leading twelve hex digits are a
    /// millisecond timestamp: eight characters of one is a ~65-second
    /// bucket, and two sessions started in the same minute display
    /// identically. Thirteen characters covers the whole timestamp, which
    /// is unique in practice and still a valid id prefix, so it round-trips
    /// back through `resolve_ref`.
    pub fn short_id(&self) -> &str {
        if self.handle.agent == AgentKind::JCode
            && let Some(name) = self.slug.as_deref()
            && !name.is_empty()
        {
            return name;
        }
        let id = self.handle.native_id.strip_prefix("session_").unwrap_or(&self.handle.native_id);
        let width = if self.handle.agent == AgentKind::Codex { 13 } else { 8 };
        &id[..id.len().min(width)]
    }
}
