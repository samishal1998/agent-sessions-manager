//! Rollout JSONL -> Session IR.
//!
//! A rollout interleaves two vocabularies. `response_item` lines are the
//! model-facing conversation (OpenAI Responses items); `event_msg` lines are
//! the UI's view of the same turn. Reading both would double every message,
//! so the conversation comes from `response_item` alone and `event_msg` is
//! consulted only for the failure reason on a turn that never produced one.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;

use crate::CoreError;
use crate::ir::{IrMessage, IrPart, IrProvenance, IrRole, IrSession, PortablePath};
use crate::model::{AgentKind, Session, SessionLocation, SessionRef, SessionStatus, Usage};

use super::CodexAdapter;

fn rollout_path(session: &Session) -> Result<PathBuf, CoreError> {
    match &session.handle.location {
        SessionLocation::JsonlFile { path } => Ok(path.clone()),
        _ => Err(CoreError::Invalid {
            msg: format!("codex session {} has no rollout file", session.handle.native_id),
        }),
    }
}

/// One parsed rollout line. Codex writes an envelope around every payload.
struct Line {
    timestamp: Option<Timestamp>,
    kind: String,
    payload: Value,
}

fn read_lines(path: &Path) -> Result<Vec<Line>, CoreError> {
    if path.extension().is_some_and(|e| e == "zst") {
        return Err(CoreError::Invalid {
            msg: format!("{} is zstd-compressed; asm cannot read it yet", path.display()),
        });
    }
    let file = std::fs::File::open(path).map_err(|e| CoreError::io(path, e))?;
    let mut lines = Vec::new();
    for line in BufReader::new(file).lines() {
        // A torn final line is normal while codex is writing; skip it
        // rather than failing the whole read.
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(mut value) = serde_json::from_str::<Value>(line) else { continue };
        lines.push(Line {
            timestamp: value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<Timestamp>().ok()),
            kind: value.get("type").and_then(Value::as_str).unwrap_or_default().to_string(),
            payload: value.get_mut("payload").map(Value::take).unwrap_or(Value::Null),
        });
    }
    Ok(lines)
}

/// Text out of a Responses `content` array, which mixes `input_text` (what
/// was sent) and `output_text` (what came back) under the same key.
fn content_text(content: Option<&Value>) -> String {
    let Some(items) = content.and_then(Value::as_array) else {
        return content.and_then(Value::as_str).unwrap_or_default().to_string();
    };
    items
        .iter()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}

fn role_of(payload: &Value) -> IrRole {
    match payload.get("role").and_then(Value::as_str) {
        Some("assistant") => IrRole::Assistant,
        // Codex injects its instructions as `developer` turns; they are
        // system context, not something the user typed.
        Some("developer") | Some("system") => IrRole::System,
        _ => IrRole::User,
    }
}

/// Turn one `response_item` payload into a message, or `None` when the item
/// carries no conversation content.
fn message_from_item(payload: &Value, timestamp: Option<Timestamp>) -> Option<IrMessage> {
    let item_type = payload.get("type").and_then(Value::as_str)?;
    let source_id = payload.get("id").and_then(Value::as_str).map(str::to_string);

    let (role, parts) = match item_type {
        "message" => {
            let text = content_text(payload.get("content"));
            if text.is_empty() {
                return None;
            }
            (role_of(payload), vec![IrPart::Text { text }])
        }
        "reasoning" => {
            let summary = payload
                .get("summary")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|i| i.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            // `encrypted_content` is provider-bound and cannot cross to
            // another agent; record that the reasoning existed and was lost.
            let opaque = payload.get("encrypted_content").is_some();
            if summary.is_empty() && !opaque {
                return None;
            }
            (IrRole::Assistant, vec![IrPart::Reasoning { summary, opaque }])
        }
        "function_call" | "custom_tool_call" | "local_shell_call" => {
            let call_id = payload
                .get("call_id")
                .and_then(Value::as_str)
                .or_else(|| payload.get("id").and_then(Value::as_str))
                .unwrap_or_default()
                .to_string();
            let name =
                payload.get("name").and_then(Value::as_str).unwrap_or(item_type).to_string();
            // `arguments` is JSON-in-a-string for function calls; `input` is
            // a plain script for custom tools. Keep whichever is there.
            let input = payload
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .or_else(|| payload.get("input").cloned())
                .or_else(|| payload.get("arguments").cloned())
                .unwrap_or(Value::Null);
            (IrRole::Assistant, vec![IrPart::ToolCall { call_id, name, input }])
        }
        "function_call_output" | "custom_tool_call_output" | "local_shell_call_output" => {
            let call_id =
                payload.get("call_id").and_then(Value::as_str).unwrap_or_default().to_string();
            let output = match payload.get("output") {
                Some(Value::String(s)) => s.clone(),
                other => content_text(other),
            };
            (
                IrRole::User,
                vec![IrPart::ToolResult {
                    call_id,
                    output,
                    is_error: false,
                    truncated: false,
                }],
            )
        }
        _ => (IrRole::System, vec![IrPart::Unknown]),
    };

    Some(IrMessage { role, timestamp, parts, source_id, extensions: Default::default() })
}

pub(super) fn export_ir(
    _adapter: &CodexAdapter,
    session: &Session,
) -> Result<IrSession, CoreError> {
    let path = rollout_path(session)?;
    let lines = read_lines(&path)?;

    let mut messages = Vec::new();
    let mut usage = session.usage;
    for line in &lines {
        match line.kind.as_str() {
            "response_item" => {
                if let Some(message) = message_from_item(&line.payload, line.timestamp) {
                    messages.push(message);
                }
            }
            // The only event worth reading: totals codex reports per turn,
            // which the `threads` row rounds into a single number.
            "event_msg" if line.payload.get("type").and_then(Value::as_str) == Some("token_count") => {
                if let Some(info) = line.payload.get("info") {
                    apply_token_count(&mut usage, info);
                }
            }
            _ => {}
        }
    }

    Ok(IrSession {
        ir_version: crate::ir::IR_VERSION,
        source: IrProvenance {
            agent: AgentKind::Codex,
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
        usage,
        messages,
        extensions: [(
            AgentKind::Codex.as_str().to_string(),
            serde_json::json!({ "rollout_path": path.display().to_string() }),
        )]
        .into_iter()
        .collect(),
        }
    )
}

fn apply_token_count(usage: &mut Usage, info: &Value) {
    let total = info.get("total_token_usage").unwrap_or(info);
    let read = |key: &str| total.get(key).and_then(Value::as_u64);
    if let Some(v) = read("input_tokens") {
        usage.input_tokens = Some(v);
    }
    if let Some(v) = read("output_tokens") {
        usage.output_tokens = Some(v);
    }
    if let Some(v) = read("cached_input_tokens") {
        usage.cache_read_tokens = Some(v);
    }
}

/// Build a `Session` from the rollout head alone, for rollouts the state
/// database has not adopted. Only `session_meta` is read: these files reach
/// tens of megabytes and a listing must not stream all of one.
pub(super) fn session_from_rollout(path: &Path) -> Option<Session> {
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut meta = None;
    // `session_meta` is the first record codex writes, but tolerate a few
    // lines of preamble rather than assuming the offset.
    for line in reader.lines().take(8) {
        let Ok(line) = line else { break };
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            meta = value.get("payload").cloned();
            break;
        }
    }
    let meta = meta?;
    let id = meta
        .get("session_id")
        .or_else(|| meta.get("id"))
        .and_then(Value::as_str)?
        .to_string();
    let created = meta
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<Timestamp>().ok());
    let modified = std::fs::metadata(path).ok();

    Some(Session {
        handle: SessionRef {
            agent: AgentKind::Codex,
            native_id: id,
            location: SessionLocation::JsonlFile { path: path.to_path_buf() },
        },
        title: None,
        slug: None,
        project_root: meta
            .get("cwd")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_default(),
        git_branch: None,
        created,
        updated: modified
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| Timestamp::try_from(t).ok())
            .or(created),
        model: meta.get("model").and_then(Value::as_str).map(str::to_string),
        usage: Usage::default(),
        status: SessionStatus::Idle,
        parent: None,
        agent_version: meta.get("cli_version").and_then(Value::as_str).map(str::to_string),
        size_bytes: modified.map(|m| m.len()),
    })
}
