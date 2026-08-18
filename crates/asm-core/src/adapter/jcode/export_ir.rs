//! jcode snapshot → Session IR.
//!
//! jcode keeps the conversation as a flat `messages` array of
//! `{id, role, content: ContentBlock[]}`, where `ContentBlock` is an
//! internally tagged enum (`{"type": "text", …}`). There are no abandoned
//! branches to walk around — unlike Claude Code's parentUuid tree, the
//! array is the conversation.

use std::fs::File;
use std::io::BufReader;

use jiff::Timestamp;
use serde_json::{Value, json};

use crate::CoreError;
use crate::ir::{
    ExtBag, IR_VERSION, IrMessage, IrPart, IrProvenance, IrRole, IrSession, PortablePath,
};
use crate::model::{Session, SessionLocation};

use super::JCodeAdapter;

pub(super) fn export_ir(
    _adapter: &JCodeAdapter,
    session: &Session,
) -> Result<IrSession, CoreError> {
    let SessionLocation::JsonlFile { path } = &session.handle.location else {
        return Err(CoreError::Invalid { msg: "session has no snapshot file".into() });
    };
    let file = File::open(path).map_err(|e| CoreError::io(path, e))?;
    let snapshot: Value = serde_json::from_reader(BufReader::with_capacity(64 * 1024, file))
        .map_err(|e| CoreError::Invalid { msg: format!("{}: {e}", path.display()) })?;

    let mut skipped: std::collections::BTreeMap<String, u64> = Default::default();
    let messages: Vec<IrMessage> = snapshot
        .get("messages")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(|m| convert_message(m, &mut skipped)).collect())
        .unwrap_or_default();

    let mut extensions = ExtBag::new();
    let mut carried = serde_json::Map::new();
    // Fields that only mean something to jcode, kept so a round-trip can
    // put them back.
    for key in [
        "short_name",
        "status",
        "provider_key",
        "provider_session_id",
        "reasoning_effort",
        "route_api_method",
        "compaction",
        "is_canary",
        "saved",
    ] {
        if let Some(value) = snapshot.get(key) {
            carried.insert(key.to_string(), value.clone());
        }
    }
    if !skipped.is_empty() {
        carried.insert("skipped_block_counts".into(), json!(skipped));
    }
    extensions.insert("jcode".into(), Value::Object(carried));

    Ok(IrSession {
        ir_version: IR_VERSION,
        source: IrProvenance {
            agent: session.handle.agent,
            native_id: session.handle.native_id.clone(),
            agent_version: session.agent_version.clone(),
            exported_at: Timestamp::now(),
            exporter_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        title: session.title.clone(),
        slug: session.slug.clone(),
        project_path: PortablePath::from_path(&session.project_root),
        created: session.created,
        updated: session.updated,
        model: session.model.clone(),
        usage: session.usage,
        messages,
        extensions,
    })
}

fn convert_message(
    message: &Value,
    skipped: &mut std::collections::BTreeMap<String, u64>,
) -> Option<IrMessage> {
    let role = match message.get("role").and_then(Value::as_str) {
        Some("assistant") => IrRole::Assistant,
        Some("user") => IrRole::User,
        _ => IrRole::System,
    };
    let timestamp = message
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|t| t.parse::<Timestamp>().ok());

    let parts: Vec<IrPart> = message
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| blocks.iter().filter_map(|b| convert_block(b, skipped)).collect())
        .unwrap_or_default();
    if parts.is_empty() {
        return None;
    }

    let mut extensions = ExtBag::new();
    if let Some(display_role) = message.get("display_role").and_then(Value::as_str) {
        extensions.insert("jcode".into(), json!({ "display_role": display_role }));
    }

    Some(IrMessage {
        role,
        timestamp,
        parts,
        source_id: message.get("id").and_then(Value::as_str).map(str::to_string),
        extensions,
    })
}

fn convert_block(
    block: &Value,
    skipped: &mut std::collections::BTreeMap<String, u64>,
) -> Option<IrPart> {
    let text_of = |key: &str| block.get(key).and_then(Value::as_str).unwrap_or("").to_string();

    match block.get("type").and_then(Value::as_str) {
        Some("text") => Some(IrPart::Text { text: text_of("text") }),
        // Plain reasoning is readable and replayable; the signed and
        // encrypted variants are provider-bound, so only their text
        // survives and the IR says so.
        Some("reasoning") | Some("reasoning_trace") => {
            Some(IrPart::Reasoning { summary: text_of("text"), opaque: false })
        }
        Some("anthropic_thinking") => {
            Some(IrPart::Reasoning { summary: text_of("thinking"), opaque: true })
        }
        Some("open_ai_reasoning") => {
            let summary = block
                .get("summary")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            Some(IrPart::Reasoning { summary, opaque: true })
        }
        Some("tool_use") => Some(IrPart::ToolCall {
            call_id: text_of("id"),
            name: text_of("name"),
            input: block.get("input").cloned().unwrap_or(Value::Null),
        }),
        Some("tool_result") => Some(IrPart::ToolResult {
            call_id: text_of("tool_use_id"),
            output: text_of("content"),
            is_error: block.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            truncated: false,
        }),
        Some("image") => Some(IrPart::File {
            path: None,
            mime: block.get("media_type").and_then(Value::as_str).map(str::to_string),
            // The bytes are inline base64; the IR carries the reference, not
            // a copy of the image.
            content: None,
        }),
        Some(other) => {
            *skipped.entry(other.to_string()).or_default() += 1;
            None
        }
        None => None,
    }
}
