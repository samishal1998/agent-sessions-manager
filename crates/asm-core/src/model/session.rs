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

#[derive(Debug, Clone, Deserialize)]
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

impl SessionStatus {
    /// `live`, `idle` or `archived`: the flat spelling of the state, for
    /// `--status`, the `state` field and the search index.
    pub fn state(&self) -> &'static str {
        match self {
            SessionStatus::Live { .. } => "live",
            SessionStatus::Idle => "idle",
            SessionStatus::Archived => "archived",
        }
    }
}

/// The JSON shape is the fields below, in order, plus two flat conveniences
/// for readers that should not have to dig: `id` (the native id) and
/// `state` (see [`SessionStatus::state`]). Written by hand rather than
/// derived so the struct, which is built literally all over, stays as it is.
/// Deserializing ignores both: they are derived from `ref` and `status`.
impl Serialize for Session {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("Session", 15)?;
        s.serialize_field("ref", &self.handle)?;
        s.serialize_field("id", &self.handle.native_id)?;
        s.serialize_field("title", &self.title)?;
        s.serialize_field("slug", &self.slug)?;
        s.serialize_field("project_root", &self.project_root)?;
        s.serialize_field("git_branch", &self.git_branch)?;
        s.serialize_field("created", &self.created)?;
        s.serialize_field("updated", &self.updated)?;
        s.serialize_field("model", &self.model)?;
        s.serialize_field("usage", &self.usage)?;
        s.serialize_field("status", &self.status)?;
        s.serialize_field("state", self.status.state())?;
        s.serialize_field("parent", &self.parent)?;
        s.serialize_field("agent_version", &self.agent_version)?;
        s.serialize_field("size_bytes", &self.size_bytes)?;
        s.end()
    }
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
        short_id_of(self.handle.agent, &self.handle.native_id, self.slug.as_deref())
    }
}

/// `Session::short_id` for a session known only by its parts — one listed
/// by a hub, say, that is not on this machine.
pub fn short_id_of<'a>(agent: AgentKind, native_id: &'a str, slug: Option<&'a str>) -> &'a str {
    if agent == AgentKind::JCode
        && let Some(name) = slug
        && !name.is_empty()
    {
        return name;
    }
    let id = native_id.strip_prefix("session_").unwrap_or(native_id);
    let width = if agent == AgentKind::Codex { 13 } else { 8 };
    &id[..id.len().min(width)]
}
