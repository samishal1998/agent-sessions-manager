use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::model::{AgentKind, Usage};

pub const IR_VERSION: u32 = 1;

/// Per-agent passthrough for anything the IR does not model, keyed by agent
/// name ("claude-code", "opencode", ...).
pub type ExtBag = BTreeMap<String, serde_json::Value>;

/// A filesystem path with `${HOME}` tokenized out, so archives and synced
/// stores stay portable across machines (path identity is the core
/// portability problem — different homes, same layout).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PortablePath(pub String);

impl PortablePath {
    pub fn from_path(path: &Path) -> Self {
        let s = path.display().to_string();
        if let Ok(home) = etcetera::home_dir() {
            let home = home.display().to_string();
            if let Some(rest) = s.strip_prefix(&home) {
                return PortablePath(format!("${{HOME}}{rest}"));
            }
        }
        PortablePath(s)
    }

    pub fn resolve(&self) -> PathBuf {
        if let Some(rest) = self.0.strip_prefix("${HOME}")
            && let Ok(home) = etcetera::home_dir()
        {
            return home.join(rest.trim_start_matches('/'));
        }
        PathBuf::from(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrProvenance {
    pub agent: AgentKind,
    pub native_id: String,
    pub agent_version: Option<String>,
    pub exported_at: Timestamp,
    /// Version of asm that produced this document.
    pub exporter_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrSession {
    pub ir_version: u32,
    pub source: IrProvenance,
    pub title: Option<String>,
    pub slug: Option<String>,
    pub project_path: PortablePath,
    pub created: Option<Timestamp>,
    pub updated: Option<Timestamp>,
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Usage,
    pub messages: Vec<IrMessage>,
    #[serde(default)]
    pub extensions: ExtBag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IrRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrMessage {
    pub role: IrRole,
    pub timestamp: Option<Timestamp>,
    pub parts: Vec<IrPart>,
    /// Native id of the record/message this came from (drives idempotent
    /// import via deterministic target ids).
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "ExtBag::is_empty")]
    pub extensions: ExtBag,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IrPart {
    Text {
        text: String,
    },
    /// Reasoning content. `opaque: true` means the original was a signed or
    /// encrypted provider payload and only its readable text survives.
    Reasoning {
        summary: String,
        opaque: bool,
    },
    ToolCall {
        call_id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        call_id: String,
        output: String,
        #[serde(default)]
        is_error: bool,
        #[serde(default)]
        truncated: bool,
    },
    File {
        path: Option<PortablePath>,
        mime: Option<String>,
        content: Option<String>,
    },
    /// A nested subagent run attached to the turn that spawned it.
    Agent {
        name: String,
        description: Option<String>,
        transcript: Vec<IrMessage>,
    },
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_path_tokenizes_home_and_resolves_back() {
        let home = etcetera::home_dir().unwrap();
        let original = home.join("projects/demo");
        let portable = PortablePath::from_path(&original);
        assert!(portable.0.starts_with("${HOME}/"), "{}", portable.0);
        assert_eq!(portable.resolve(), original);

        let foreign = PortablePath::from_path(Path::new("/opt/other"));
        assert_eq!(foreign.0, "/opt/other");
        assert_eq!(foreign.resolve(), PathBuf::from("/opt/other"));
    }

    #[test]
    fn ir_round_trips_through_json() {
        let session = IrSession {
            ir_version: IR_VERSION,
            source: IrProvenance {
                agent: AgentKind::ClaudeCode,
                native_id: "abc".into(),
                agent_version: Some("2.1.233".into()),
                exported_at: Timestamp::UNIX_EPOCH,
                exporter_version: "0.1.0".into(),
            },
            title: Some("t".into()),
            slug: None,
            project_path: PortablePath("${HOME}/p".into()),
            created: None,
            updated: None,
            model: None,
            usage: Usage::default(),
            messages: vec![IrMessage {
                role: IrRole::Assistant,
                timestamp: None,
                parts: vec![
                    IrPart::Text { text: "hi".into() },
                    IrPart::ToolCall {
                        call_id: "c1".into(),
                        name: "bash".into(),
                        input: serde_json::json!({"command": "ls"}),
                    },
                ],
                source_id: Some("u1".into()),
                extensions: ExtBag::new(),
            }],
            extensions: ExtBag::new(),
        };
        let json = serde_json::to_string(&session).unwrap();
        let back: IrSession = serde_json::from_str(&json).unwrap();
        assert_eq!(back.messages.len(), 1);
        assert!(matches!(back.messages[0].parts[1], IrPart::ToolCall { .. }));
    }

    #[test]
    fn unknown_part_types_deserialize_leniently() {
        let json = r#"{"type": "hologram", "data": 1}"#;
        let part: IrPart = serde_json::from_str(json).unwrap();
        assert!(matches!(part, IrPart::Unknown));
    }
}
