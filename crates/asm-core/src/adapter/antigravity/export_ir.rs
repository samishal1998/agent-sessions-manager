//! Antigravity transcript -> Session IR.
//!
//! Read from `brain/<id>/.system_generated/logs/transcript.jsonl`, not from
//! the protobuf blobs in the conversation database. Both are written by
//! antigravity from the same steps; the JSONL is a documented-by-inspection
//! shape whose absence asm can report, while the blobs would require
//! pinning field numbers of a schema Google does not publish.
//!
//! Every line: `{step_index, source, type, status, created_at, content}`.

use std::io::{BufRead, BufReader};

use jiff::Timestamp;
use serde_json::Value;

use crate::CoreError;
use crate::ir::{IrMessage, IrPart, IrProvenance, IrRole, IrSession, PortablePath};
use crate::model::{AgentKind, Session, Usage};

use super::AntigravityAdapter;

/// Antigravity wraps the user's words in envelopes meant for the model —
/// the request itself, then the local time, then any settings that changed.
/// Only the request is what the user said.
fn user_request(content: &str) -> String {
    match (content.find("<USER_REQUEST>"), content.find("</USER_REQUEST>")) {
        (Some(start), Some(end)) if end > start => {
            content[start + "<USER_REQUEST>".len()..end].trim().to_string()
        }
        _ => content.trim().to_string(),
    }
}

pub(super) fn export_ir(
    adapter: &AntigravityAdapter,
    session: &Session,
) -> Result<IrSession, CoreError> {
    let path = adapter.transcript_of(&session.handle.native_id);
    let file = std::fs::File::open(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CoreError::Invalid {
                msg: format!(
                    "antigravity kept no readable transcript for {} (expected {}); its \
                     conversation database holds the steps as protobuf blobs asm cannot decode",
                    session.handle.native_id,
                    path.display()
                ),
            }
        } else {
            CoreError::io(&path, e)
        }
    })?;

    let mut messages = Vec::new();
    for line in BufReader::new(file).lines() {
        // A torn last line is normal while antigravity is writing.
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else { continue };
        let step_type = value.get("type").and_then(Value::as_str).unwrap_or_default();
        let content = value.get("content").and_then(Value::as_str).unwrap_or_default();
        if content.is_empty() {
            continue;
        }
        let timestamp = value
            .get("created_at")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<Timestamp>().ok());
        let source_id =
            value.get("step_index").and_then(Value::as_i64).map(|i| i.to_string());

        let (role, text) = match step_type {
            "USER_INPUT" => (IrRole::User, user_request(content)),
            "PLANNER_RESPONSE" | "AGENT_RESPONSE" => (IrRole::Assistant, content.to_string()),
            // A checkpoint is antigravity's own compaction summary, and a
            // system message is the harness talking. Both are context the
            // model saw, neither is something a person said.
            "CHECKPOINT" | "SYSTEM_MESSAGE" => (IrRole::System, content.to_string()),
            // Unknown step types are kept as system context rather than
            // dropped: this vocabulary will grow.
            _ => (IrRole::System, content.to_string()),
        };
        if text.is_empty() {
            continue;
        }
        messages.push(IrMessage {
            role,
            timestamp,
            parts: vec![IrPart::Text { text }],
            source_id,
            extensions: [(
                AgentKind::Antigravity.as_str().to_string(),
                serde_json::json!({ "step_type": step_type }),
            )]
            .into_iter()
            .collect(),
        });
    }

    Ok(IrSession {
        ir_version: crate::ir::IR_VERSION,
        source: IrProvenance {
            agent: AgentKind::Antigravity,
            native_id: session.handle.native_id.clone(),
            agent_version: session.agent_version.clone(),
            exported_at: Timestamp::now(),
            exporter_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        title: session.title.clone(),
        slug: None,
        project_path: PortablePath::from_path(&session.project_root),
        created: session.created,
        updated: session.updated,
        model: session.model.clone(),
        usage: Usage::default(),
        messages,
        extensions: [(
            AgentKind::Antigravity.as_str().to_string(),
            serde_json::json!({ "transcript_log": path.display().to_string() }),
        )]
        .into_iter()
        .collect(),
    })
}
