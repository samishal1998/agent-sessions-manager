//! Claude Code transcript → Session IR.
//!
//! Ordering: transcripts are append-only logs that can contain abandoned
//! branches (forks, resume-at-point). The faithful conversation is the
//! parentUuid chain walked back from the resume leaf (`last-prompt`'s
//! leafUuid), bridging `compact_boundary` records via `logicalParentUuid`.
//! If the walk looks implausible (missing links), we fall back to file
//! order rather than dropping content.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use jiff::Timestamp;
use serde_json::{Value, json};

use crate::CoreError;
use crate::ir::{ExtBag, IR_VERSION, IrMessage, IrPart, IrProvenance, IrRole, IrSession, PortablePath};
use crate::model::{Session, SessionLocation};

use super::ClaudeAdapter;

pub(super) fn export_ir(
    _adapter: &ClaudeAdapter,
    session: &Session,
) -> Result<IrSession, CoreError> {
    let SessionLocation::JsonlFile { path: transcript } = &session.handle.location else {
        return Err(CoreError::Invalid { msg: "session has no transcript file".into() });
    };

    let mut records: Vec<Value> = Vec::new();
    let file = File::open(transcript).map_err(|e| CoreError::io(transcript, e))?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    records.push(v);
                }
            }
            Err(_) => break, // torn tail
        }
    }

    let ordered = order_conversation(&records);
    let mut messages: Vec<IrMessage> = ordered
        .iter()
        .filter_map(|record| convert_record(record))
        .collect();

    attach_subagents(transcript, &session.handle.native_id, &mut messages);

    // Compact state records + a census of what was left behind.
    let mut state_records = Vec::new();
    let mut skipped: HashMap<String, u64> = HashMap::new();
    for record in &records {
        match record.get("type").and_then(Value::as_str) {
            Some(
                "ai-title" | "custom-title" | "last-prompt" | "mode" | "permission-mode"
                | "agent-name",
            ) => {
                state_records.push(record.clone());
            }
            Some("user" | "assistant") => {}
            Some(other) => *skipped.entry(other.to_string()).or_default() += 1,
            None => *skipped.entry("(untyped)".to_string()).or_default() += 1,
        }
    }
    let mut extensions = ExtBag::new();
    extensions.insert(
        "claude-code".into(),
        json!({ "state_records": state_records, "skipped_record_counts": skipped }),
    );

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

fn is_conversation(record: &Value) -> bool {
    matches!(record.get("type").and_then(Value::as_str), Some("user" | "assistant"))
}

/// Walk the parentUuid chain from the leaf; fall back to file order.
fn order_conversation(records: &[Value]) -> Vec<&Value> {
    let by_uuid: HashMap<&str, &Value> = records
        .iter()
        .filter_map(|r| r.get("uuid").and_then(Value::as_str).map(|u| (u, r)))
        .collect();

    let leaf = records
        .iter()
        .rev()
        .find_map(|r| {
            (r.get("type").and_then(Value::as_str) == Some("last-prompt"))
                .then(|| r.get("leafUuid").and_then(Value::as_str))
                .flatten()
        })
        .or_else(|| {
            records
                .iter()
                .rev()
                .find(|r| is_conversation(r))
                .and_then(|r| r.get("uuid").and_then(Value::as_str))
        });

    let conversation_count = records.iter().filter(|r| is_conversation(r)).count();
    if let Some(leaf) = leaf {
        let mut chain: Vec<&Value> = Vec::new();
        let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut current = by_uuid.get(leaf).copied();
        while let Some(record) = current {
            // Cycle guard: dirty chains must degrade, not hang.
            if let Some(uuid) = record.get("uuid").and_then(Value::as_str)
                && !visited.insert(uuid)
            {
                break;
            }
            if is_conversation(record) {
                chain.push(record);
            }
            let parent = record
                .get("parentUuid")
                .and_then(Value::as_str)
                // compact boundaries: parentUuid is null, the logical chain
                // continues through logicalParentUuid.
                .or_else(|| record.get("logicalParentUuid").and_then(Value::as_str));
            current = parent.and_then(|p| by_uuid.get(p).copied());
        }
        chain.reverse();
        // Plausibility: a broken chain would silently drop history.
        if chain.len() * 2 >= conversation_count {
            return chain;
        }
    }
    records.iter().filter(|r| is_conversation(r)).collect()
}

fn convert_record(record: &Value) -> Option<IrMessage> {
    let record_type = record.get("type").and_then(Value::as_str)?;
    let message = record.get("message")?;
    let timestamp = record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|t| t.parse::<Timestamp>().ok());
    let source_id = record.get("uuid").and_then(Value::as_str).map(str::to_string);

    let mut extensions = ExtBag::new();
    if record.get("isMeta").and_then(Value::as_bool) == Some(true) {
        extensions.insert("claude-code".into(), json!({ "isMeta": true }));
    }

    let (role, parts) = match record_type {
        "user" => (IrRole::User, user_parts(message.get("content")?)),
        "assistant" => (IrRole::Assistant, assistant_parts(message.get("content")?)),
        _ => return None,
    };
    if parts.is_empty() {
        return None;
    }
    Some(IrMessage { role, timestamp, parts, source_id, extensions })
}

fn user_parts(content: &Value) -> Vec<IrPart> {
    match content {
        Value::String(text) => vec![IrPart::Text { text: text.clone() }],
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|t| IrPart::Text { text: t.to_string() }),
                Some("tool_result") => Some(IrPart::ToolResult {
                    call_id: block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    output: stringify_content(block.get("content")),
                    is_error: block.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    truncated: false,
                }),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn assistant_parts(content: &Value) -> Vec<IrPart> {
    let Value::Array(blocks) = content else { return Vec::new() };
    blocks
        .iter()
        .filter_map(|block| match block.get("type").and_then(Value::as_str) {
            Some("text") => block
                .get("text")
                .and_then(Value::as_str)
                .map(|t| IrPart::Text { text: t.to_string() }),
            // Thinking blocks are signed provider payloads — only the
            // readable text crosses agents.
            Some("thinking") => block
                .get("thinking")
                .and_then(Value::as_str)
                .map(|t| IrPart::Reasoning { summary: t.to_string(), opaque: true }),
            Some("tool_use") => Some(IrPart::ToolCall {
                call_id: block.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
                name: block.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                input: block.get("input").cloned().unwrap_or(Value::Null),
            }),
            _ => None,
        })
        .collect()
}

fn stringify_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Fold `<project>/<uuid>/subagents/**/agent-*.jsonl` transcripts into the
/// assistant turn that spawned them (matched by the meta.json toolUseId).
fn attach_subagents(transcript: &Path, session_id: &str, messages: &mut [IrMessage]) {
    let Some(project_dir) = transcript.parent() else { return };
    let subagents_root = project_dir.join(session_id).join("subagents");
    let mut agent_files = Vec::new();
    collect_agent_files(&subagents_root, &mut agent_files);

    for agent_file in agent_files {
        let meta_path = agent_file.with_extension("").with_extension("meta.json");
        let meta: Value = std::fs::read(&meta_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        let tool_use_id = meta.get("toolUseId").and_then(Value::as_str);

        let mut sub_messages = Vec::new();
        if let Ok(file) = File::open(&agent_file) {
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if let Ok(record) = serde_json::from_str::<Value>(&line)
                    && let Some(msg) = convert_record(&record)
                {
                    sub_messages.push(msg);
                }
            }
        }
        if sub_messages.is_empty() {
            continue;
        }

        let part = IrPart::Agent {
            name: meta
                .get("agentType")
                .and_then(Value::as_str)
                .unwrap_or("subagent")
                .to_string(),
            description: meta.get("description").and_then(Value::as_str).map(str::to_string),
            transcript: sub_messages,
        };

        // Attach to the assistant message holding the spawning tool_use.
        let target = tool_use_id.and_then(|id| {
            messages.iter_mut().find(|m| {
                m.parts
                    .iter()
                    .any(|p| matches!(p, IrPart::ToolCall { call_id, .. } if call_id == id))
            })
        });
        match target {
            Some(message) => message.parts.push(part),
            None => {
                if let Some(last) = messages.last_mut() {
                    last.parts.push(part);
                }
            }
        }
    }
}

fn collect_agent_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_agent_files(&path, out);
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.starts_with("agent-")
            && name.ends_with(".jsonl")
        {
            out.push(path);
        }
    }
    out.sort();
}
