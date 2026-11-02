//! A pi/omp session file -> `Session` and Session IR.
//!
//! The file is a tree: every entry names its `parentId`, a branch is a
//! child of an earlier entry, and the conversation shown is the path from
//! the current leaf back to a root. The leaf is not stored; pi and omp both
//! take the last entry appended (omp: unless a trailing `model_usage` names
//! another with `activeLeafId`). Version 1 files have no ids at all and are
//! read in file order.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;

use crate::CoreError;
use crate::ir::{IrMessage, IrPart, IrProvenance, IrRole, IrSession, PortablePath};
use crate::model::{AgentKind, Session, SessionLocation, SessionRef, SessionStatus, Usage};

fn ts(value: Option<&Value>) -> Option<Timestamp> {
    value.and_then(Value::as_str).and_then(|s| s.parse().ok())
}

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Every complete line of the file, parsed. A torn final line (one being
/// written) and lines that are not JSON are skipped, not fatal.
fn read_values(path: &Path, keep: impl Fn(&str) -> bool) -> Result<Vec<Value>, CoreError> {
    let file = std::fs::File::open(path).map_err(|e| CoreError::io(path, e))?;
    let mut values = Vec::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() || !keep(line) {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            values.push(value);
        }
    }
    Ok(values)
}

/// The title slot (omp) and the header: the first two lines at most.
struct Head {
    slot_title: Option<String>,
    header: Value,
}

fn head(values: &[Value]) -> Option<(Head, usize)> {
    let mut slot_title = None;
    for (i, value) in values.iter().enumerate().take(2) {
        match str_of(value, "type") {
            // An empty slot title means "no title", as omp reads it.
            Some("title") => slot_title = Some(str_of(value, "title").unwrap_or_default().trim().to_string()),
            Some("session") => return Some((Head { slot_title, header: value.clone() }, i + 1)),
            _ => return None,
        }
    }
    None
}

/// The plain text of a pi `content`: a string, or blocks of which the text
/// ones count.
fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| str_of(b, "type") == Some("text"))
            .filter_map(|b| str_of(b, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn add_usage(total: &mut Usage, usage: &Value) {
    let field = |key: &str| usage.get(key).and_then(Value::as_u64);
    let add = |slot: &mut Option<u64>, v: Option<u64>| {
        if let Some(v) = v {
            *slot = Some(slot.unwrap_or(0) + v);
        }
    };
    add(&mut total.input_tokens, field("input"));
    add(&mut total.output_tokens, field("output"));
    add(&mut total.cache_read_tokens, field("cacheRead"));
    add(&mut total.cache_write_tokens, field("cacheWrite"));
    if let Some(cost) = usage.pointer("/cost/total").and_then(Value::as_f64) {
        total.cost_usd = Some(total.cost_usd.unwrap_or(0.0) + cost);
    }
}

/// The first line of a user's text, short enough to be a title.
fn first_line(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.chars().take(120).collect())
}

/// A `Session` from its file. Only the lines a listing needs are parsed:
/// tool results, which carry the bulk of a long session, are skipped.
pub(crate) fn session_from_file(kind: AgentKind, path: &Path, parent: Option<String>) -> Option<Session> {
    let values = read_values(path, |line| !line.contains("\"role\":\"toolResult\"")).ok()?;
    let (head, body) = head(&values)?;
    let header = &head.header;
    let id = str_of(header, "id")?.to_string();

    let mut name: Option<String> = None;
    let mut first_user = None;
    let mut model = None;
    let mut usage = Usage::default();
    let mut last = None;
    let mut agent_version = None;
    for entry in &values[body..] {
        last = ts(entry.get("timestamp")).or(last);
        if let Some(version) = entry.pointer("/runtime/version").and_then(Value::as_str) {
            agent_version = Some(version.to_string());
        }
        match str_of(entry, "type") {
            // Pi's name: the latest wins, and an empty one clears it.
            Some("session_info") => name = str_of(entry, "name").map(|n| n.trim().to_string()),
            Some("title_change") => name = str_of(entry, "title").map(|n| n.trim().to_string()),
            Some("usage") | Some("model_usage") => {
                if let Some(u) = entry.get("usage") {
                    add_usage(&mut usage, u);
                }
            }
            Some("message") => {
                let Some(message) = entry.get("message") else { continue };
                match str_of(message, "role") {
                    Some("user") if first_user.is_none() => first_user = first_line(&text_of(message.get("content"))),
                    Some("assistant") => {
                        if let Some(m) = str_of(message, "model") {
                            model = Some(m.to_string());
                        }
                        // Usage entries (pi `usage`, omp `model_usage`) are
                        // the side calls; the turns themselves count here.
                        if let Some(u) = message.get("usage") {
                            add_usage(&mut usage, u);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // omp's slot is the title it shows; then its header copy; then pi's name.
    let title = head
        .slot_title
        .or_else(|| str_of(header, "title").map(str::to_string))
        .or(name)
        .filter(|t| !t.is_empty())
        .or(first_user);
    let created = ts(header.get("timestamp"));
    let meta = std::fs::metadata(path).ok();
    let modified = meta.as_ref().and_then(|m| m.modified().ok()).and_then(|t| Timestamp::try_from(t).ok());
    Some(Session {
        handle: SessionRef { agent: kind, native_id: id, location: SessionLocation::JsonlFile { path: path.to_path_buf() } },
        title,
        slug: None,
        project_root: str_of(header, "cwd").map(PathBuf::from).unwrap_or_default(),
        git_branch: None,
        created,
        updated: modified.or(last).or(created),
        model,
        usage,
        status: SessionStatus::Idle,
        parent,
        agent_version,
        size_bytes: meta.map(|m| m.len()),
    })
}

/// The entries on the current branch, root first.
fn current_branch(entries: &[Value]) -> Vec<&Value> {
    let by_id: HashMap<&str, &Value> = entries.iter().filter_map(|e| Some((str_of(e, "id")?, e))).collect();
    // Version 1: no ids, so no tree; the file is the conversation.
    if by_id.is_empty() {
        return entries.iter().collect();
    }
    let Some(mut leaf) = entries.iter().rev().find(|e| str_of(e, "id").is_some()) else { return Vec::new() };
    if str_of(leaf, "type") == Some("model_usage")
        && let Some(active) = str_of(leaf, "activeLeafId").and_then(|id| by_id.get(id))
    {
        leaf = active;
    }
    let mut path = vec![leaf];
    let mut seen = HashSet::from([str_of(leaf, "id")]);
    while let Some(parent) = str_of(path[path.len() - 1], "parentId").and_then(|id| by_id.get(id)) {
        // A cycle is a damaged file; stop rather than loop.
        if !seen.insert(str_of(parent, "id")) {
            break;
        }
        path.push(parent);
    }
    path.reverse();
    path
}

fn blocks(content: Option<&Value>) -> Vec<&Value> {
    match content {
        Some(Value::Array(items)) => items.iter().collect(),
        _ => Vec::new(),
    }
}

fn image(block: &Value) -> IrPart {
    IrPart::File { path: None, mime: str_of(block, "mimeType").map(str::to_string), content: None }
}

/// User-side content: a string, or text and image blocks.
fn user_parts(content: Option<&Value>) -> Vec<IrPart> {
    if let Some(Value::String(text)) = content {
        return vec![IrPart::Text { text: text.clone() }];
    }
    blocks(content)
        .into_iter()
        .map(|b| match str_of(b, "type") {
            Some("text") => IrPart::Text { text: str_of(b, "text").unwrap_or_default().to_string() },
            Some("image") => image(b),
            _ => IrPart::Unknown,
        })
        .collect()
}

fn assistant_parts(content: Option<&Value>) -> Vec<IrPart> {
    blocks(content)
        .into_iter()
        .map(|b| match str_of(b, "type") {
            Some("text") => IrPart::Text { text: str_of(b, "text").unwrap_or_default().to_string() },
            // A signed thought is provider-bound: its text survives, the
            // signature cannot travel.
            Some("thinking") => IrPart::Reasoning {
                summary: str_of(b, "thinking").unwrap_or_default().to_string(),
                opaque: b.get("thinkingSignature").is_some(),
            },
            Some("redactedThinking") => IrPart::Reasoning { summary: String::new(), opaque: true },
            Some("toolCall") => IrPart::ToolCall {
                call_id: str_of(b, "id").unwrap_or_default().to_string(),
                name: str_of(b, "name").unwrap_or_default().to_string(),
                input: b.get("arguments").cloned().unwrap_or(Value::Null),
            },
            Some("image") => image(b),
            _ => IrPart::Unknown,
        })
        .collect()
}

/// One entry of the branch as a message, or `None` for bookkeeping entries
/// (model and thinking changes, labels, usage, titles) and empty ones.
fn message_of(entry: &Value) -> Option<IrMessage> {
    let timestamp = ts(entry.get("timestamp"));
    let source_id = str_of(entry, "id").map(str::to_string);
    let system = |text: String| (!text.trim().is_empty()).then(|| (IrRole::System, vec![IrPart::Text { text }]));
    let (role, parts) = match str_of(entry, "type")? {
        "message" => {
            let message = entry.get("message")?;
            let content = message.get("content");
            match str_of(message, "role")? {
                "user" => (IrRole::User, user_parts(content)),
                "assistant" => (IrRole::Assistant, assistant_parts(content)),
                "toolResult" => (
                    IrRole::User,
                    vec![IrPart::ToolResult {
                        call_id: str_of(message, "toolCallId").unwrap_or_default().to_string(),
                        output: text_of(content),
                        is_error: message.get("isError").and_then(Value::as_bool).unwrap_or(false),
                        truncated: false,
                    }],
                ),
                // The system prompt and its patches are carried as sections,
                // with `content` usually empty: not part of the conversation.
                "system" | "developer" => system(text_of(content))?,
                "bashExecution" => system(format!(
                    "$ {}\n{}",
                    str_of(message, "command").unwrap_or_default(),
                    str_of(message, "output").unwrap_or_default()
                ))?,
                "branchSummary" | "compactionSummary" => system(str_of(message, "summary").map(str::to_string).unwrap_or_else(|| text_of(content)))?,
                // Extension messages (`custom`, legacy `hookMessage`).
                _ => system(text_of(content))?,
            }
        }
        "compaction" => system(format!("Earlier conversation, summarized: {}", str_of(entry, "summary")?))?,
        "branch_summary" => system(format!("Abandoned branch, summarized: {}", str_of(entry, "summary")?))?,
        "custom_message" if entry.get("display").and_then(Value::as_bool) != Some(false) => {
            system(text_of(entry.get("content")))?
        }
        _ => return None,
    };
    if parts.is_empty() {
        return None;
    }
    Some(IrMessage { role, timestamp, parts, source_id, extensions: Default::default() })
}

/// The session in the file at `path`, wherever that is: a hub's blob store
/// keeps it under its hash.
pub(crate) fn ir_of_file(kind: AgentKind, path: &Path) -> Result<IrSession, CoreError> {
    let session = session_from_file(kind, path, None)
        .ok_or_else(|| CoreError::Invalid { msg: format!("{} is not a {kind} session", path.display()) })?;
    export_ir(&session)
}

/// The messages on the current branch of the file at `path`.
fn messages_of(path: &Path) -> Result<Vec<IrMessage>, CoreError> {
    let values = read_values(path, |_| true)?;
    let body = head(&values).map_or(0, |(_, body)| body);
    Ok(current_branch(&values[body..]).into_iter().filter_map(message_of).collect())
}

pub(crate) fn export_ir(session: &Session) -> Result<IrSession, CoreError> {
    let SessionLocation::JsonlFile { path } = &session.handle.location else {
        return Err(CoreError::Invalid { msg: format!("{} session has no file", session.handle.agent) });
    };
    let kind = session.handle.agent;
    Ok(IrSession {
        ir_version: crate::ir::IR_VERSION,
        source: IrProvenance {
            agent: kind,
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
        usage: session.usage,
        messages: messages_of(path)?,
        extensions: [(kind.as_str().to_string(), serde_json::json!({ "path": path.display().to_string() }))]
            .into_iter()
            .collect(),
    })
}
