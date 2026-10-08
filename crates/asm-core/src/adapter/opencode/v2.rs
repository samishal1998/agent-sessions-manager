//! OpenCode 2.x (verified against 2.0.25): read side and the exporter.
//!
//! Sessions live in `session_v2` (listed by `store.rs`, which shares its
//! query with 1.x) and messages in `session_message`, one row per message,
//! ordered by `seq`. Both are PROJECTIONS of the `event` log: the running
//! service folds events into these rows (a streamed tool call becomes the
//! same `assistant` row updated in place). `data` is the message JSON with
//! `id` and `type` stripped out into their own columns.
//!
//! The message union (`packages/schema/src/session-message.ts`):
//! `user{text,files,agents,skills}`, `assistant{agent,model,content[],finish,
//! cost,tokens,error,snapshot,time}`, `shell{shellID,command,status,exit,
//! output{output,cursor,size,truncated}}`, `synthetic{text}`, `system{text}`,
//! `skill{skill,name,text}`, `compaction{status,reason,summary,recent}`, plus
//! the markers `idle`, `agent-switched`, `model-switched`,
//! `location-switched`. An assistant's `content` holds `text`, `reasoning`
//! and `tool` items; a tool's `state` is `streaming{input: string}`,
//! `running`, `completed{input,content,metadata}` or `error{error:{type,
//! message}}`, its `content` a list of `{type:"text",text}` /
//! `{type:"file",uri,mime,name}`.
//!
//! ## The exporter
//!
//! [`transfer_tree`] builds, from read-only queries, the document
//! `opencode session export` prints and `opencode session import` takes
//! (`SessionTransfer.Data` = `{info, messages}`): `info` the way OpenCode's
//! own `Session.fromRow` shapes a row, `messages` the stored JSON with `id`
//! and `type` put back, in `seq` order, minus the messages still in flight
//! (OpenCode's `isSettled`). It never starts `opencode`, so it is safe on a
//! live store, and it is what a backup, a hub push and the hub's transcript
//! view are all made from.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::CoreError;
use crate::ir::{ExtBag, IrMessage, IrPart, IrRole, IrSession, PortablePath};
use crate::model::{AgentKind, Session, SessionLocation, SessionRef, SessionStatus};

use super::OpenCodeAdapter;
use super::export_ir::ir_session;

/// Marks a hub bundle as OpenCode 2.x documents (as opposed to 1.x rows).
pub(crate) const GENERATION: &str = "opencode-v2";

/// The pid of the running OpenCode background service, if there is one.
///
/// 2.x runs sessions inside a long-lived service that records itself in
/// `$XDG_STATE_HOME/opencode/service.json` (`{id, version, url, pid}`). A
/// session is mid-turn when `session_v2.time_suspended` is set (the service
/// claims it when execution starts and releases it when the turn settles)
/// AND that service is up: a service that died leaves its claims behind for
/// the next start to resume, and those sessions are not live. "Up" is proven
/// the way OpenCode's own probe does it (`cli2::service_pid`): the pid is
/// alive and the registered url answers with that pid, so a stale file whose
/// pid another process now owns does not count.
///
/// Known blind spots: `opencode --standalone` runs a private server that
/// writes no service file, so a turn running there reads as idle; and a
/// non-release channel registers in `service-<channel>.json`, which is not
/// read, so its turns read as idle too.
pub(super) fn service_pid(adapter: &OpenCodeAdapter) -> Option<u32> {
    super::cli2::service_pid(adapter.state_dir()?)
}

pub(super) fn export_ir(adapter: &OpenCodeAdapter, session: &Session) -> Result<IrSession, CoreError> {
    let sql_err = |e: rusqlite::Error| CoreError::Sqlite { db: adapter.db().to_path_buf(), source: Box::new(e) };
    let conn = super::super::open_ro(adapter.db())?;
    // Everything stored, settled or not: a reader wants to see the turn that
    // is streaming right now. (The transfer format below leaves it out.)
    let messages = messages_of(&conn, &session.handle.native_id, false).map_err(sql_err)?;
    Ok(ir_session(session, messages.iter().filter_map(convert_message).collect()))
}

// -- the exporter ------------------------------------------------------

/// `Session.Info` for `id`, shaped as `fromRow` does; `None` when there is
/// no such session. Every optional that is unset is left out, as OpenCode
/// prints it.
pub(crate) fn info_of(conn: &Connection, id: &str) -> rusqlite::Result<Option<Value>> {
    let json_col = |text: Option<String>| text.and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let row = conn.query_row(
        "SELECT parent_id, fork_session_id, fork_boundary, project_id, workspace_id, directory, path,
                title, metadata, cost, tokens_input, tokens_output, tokens_reasoning,
                tokens_cache_read, tokens_cache_write, revert, permission, agent, model,
                time_created, time_updated, time_idle, time_viewed, idle_outcome, time_archived
         FROM session_v2 WHERE id = ?1",
        [id],
        |r| {
            let mut info = Map::new();
            info.insert("id".into(), json!(id));
            let mut put = |key: &str, value: Option<Value>| {
                if let Some(value) = value {
                    info.insert(key.into(), value);
                }
            };
            put("parentID", r.get::<_, Option<String>>(0)?.map(Value::from));
            let fork = (r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?);
            put("fork", match fork {
                (Some(session), Some(boundary)) => {
                    Some(json!({ "sessionID": session, "boundary": json_col(Some(boundary.clone())).unwrap_or(json!(boundary)) }))
                }
                _ => None,
            });
            put("projectID", Some(json!(r.get::<_, String>(3)?)));
            let workspace = r.get::<_, Option<String>>(4)?;
            let mut location = json!({ "directory": r.get::<_, String>(5)? });
            if let Some(workspace) = workspace {
                location["workspaceID"] = json!(workspace);
            }
            put("location", Some(location));
            put("subpath", r.get::<_, Option<String>>(6)?.filter(|p| !p.is_empty()).map(Value::from));
            put("title", r.get::<_, Option<String>>(7)?.map(Value::from));
            put("metadata", json_col(r.get(8)?));
            put("cost", Some(number(r.get::<_, f64>(9)?)));
            put("tokens", Some(json!({
                "input": r.get::<_, i64>(10)?,
                "output": r.get::<_, i64>(11)?,
                "reasoning": r.get::<_, i64>(12)?,
                "cache": { "read": r.get::<_, i64>(13)?, "write": r.get::<_, i64>(14)? },
            })));
            put("revert", json_col(r.get(15)?));
            put("permissions", json_col(r.get(16)?));
            put("agent", r.get::<_, Option<String>>(17)?.map(Value::from));
            put("model", json_col(r.get(18)?).map(|mut model| {
                // `fromRow` fills the variant in when the column has none.
                if let Some(m) = model.as_object_mut() {
                    m.entry("variant").or_insert(json!("default"));
                }
                model
            }));
            let mut time = Map::new();
            time.insert("created".into(), json!(r.get::<_, i64>(19)?));
            time.insert("updated".into(), json!(r.get::<_, i64>(20)?));
            for (key, col) in [("idle", 21), ("viewed", 22)] {
                if let Some(ms) = r.get::<_, Option<i64>>(col)? {
                    time.insert(key.into(), json!(ms));
                }
            }
            // `fromRow` treats an archived time of 0 as unset.
            if let Some(ms) = r.get::<_, Option<i64>>(24)?.filter(|ms| *ms != 0) {
                time.insert("archived".into(), json!(ms));
            }
            put("time", Some(Value::Object(time)));
            put("outcome", r.get::<_, Option<String>>(23)?.map(Value::from));
            Ok(Value::Object(info))
        },
    );
    match row {
        Ok(info) => Ok(Some(info)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// A number the way JavaScript prints it: a whole value without a fraction,
/// so `0.0` compares equal to the `0` OpenCode's own export gives.
fn number(f: f64) -> Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9e15 { json!(f as i64) } else { json!(f) }
}

/// The messages of a session as stored, `id` and `type` back in, in `seq`
/// order. `settled_only` drops what OpenCode's export drops: an assistant
/// message that has not completed, a shell or compaction still running.
pub(crate) fn messages_of(conn: &Connection, id: &str, settled_only: bool) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare("SELECT id, type, data FROM session_message WHERE session_id = ?1 ORDER BY seq")?;
    let rows = stmt.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, kind, data) = row?;
        let mut message = match serde_json::from_str::<Value>(&data) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        };
        message.insert("id".into(), json!(id));
        message.insert("type".into(), json!(kind));
        let message = Value::Object(message);
        if !settled_only || is_settled(&message) {
            out.push(message);
        }
    }
    Ok(out)
}

/// `tree` with the messages still in flight left out: what an import
/// brings back.
pub(crate) fn settled(tree: &[Value]) -> Vec<Value> {
    tree.iter()
        .map(|t| {
            let mut t = t.clone();
            if let Some(messages) = t.get_mut("messages").and_then(Value::as_array_mut) {
                messages.retain(is_settled);
            }
            t
        })
        .collect()
}

fn is_settled(message: &Value) -> bool {
    match text(message, "type") {
        Some("assistant") => message.pointer("/time/completed").is_some_and(|c| !c.is_null()),
        Some("shell" | "compaction") => text(message, "status") != Some("running"),
        _ => true,
    }
}

/// `id` and every session below it, parents before children (what an import
/// needs), each level in id order.
pub(crate) fn tree_ids(conn: &Connection, root: &str) -> rusqlite::Result<Vec<String>> {
    let mut all = vec![root.to_string()];
    let mut level = vec![root.to_string()];
    let mut stmt = conn.prepare("SELECT id FROM session_v2 WHERE parent_id = ?1 ORDER BY id")?;
    while !level.is_empty() {
        let mut next = Vec::new();
        for parent in &level {
            for child in stmt.query_map([parent], |r| r.get::<_, String>(0))? {
                let child = child?;
                if !all.contains(&child) {
                    all.push(child.clone());
                    next.push(child);
                }
            }
        }
        level = next;
    }
    Ok(all)
}

/// `{info, messages}` for one session; `None` when it does not exist.
/// `settled_only` leaves out what OpenCode's own export leaves out (see
/// [`messages_of`]): the document a hub push or an import carries. A backup
/// asks for everything stored, so a turn abandoned mid-way is not lost with
/// the session (an import drops those messages again, as it does for the
/// export).
pub(crate) fn transfer(conn: &Connection, id: &str, settled_only: bool) -> rusqlite::Result<Option<Value>> {
    let Some(info) = info_of(conn, id)? else { return Ok(None) };
    Ok(Some(json!({ "info": info, "messages": messages_of(conn, id, settled_only)? })))
}

/// The transfer document of `root` and of every session below it, parents
/// first, from one read snapshot.
pub(crate) fn transfer_tree(conn: &Connection, root: &str, settled_only: bool) -> rusqlite::Result<Vec<Value>> {
    let _ = conn.execute_batch("BEGIN");
    let tree = tree_ids(conn, root).and_then(|ids| {
        ids.iter().filter_map(|id| transfer(conn, id, settled_only).transpose()).collect::<rusqlite::Result<Vec<_>>>()
    });
    let _ = conn.execute_batch("COMMIT");
    tree
}

/// How many messages each of `ids` holds and the newest `seq` among them: a
/// cheap way to see whether a session was continued between two looks.
pub(crate) fn signature(conn: &Connection, ids: &[String]) -> rusqlite::Result<Vec<(String, i64, i64)>> {
    ids.iter()
        .map(|id| {
            conn.query_row(
                "SELECT COUNT(*), COALESCE(MAX(seq), 0) FROM session_message WHERE session_id = ?1",
                [id],
                |r| Ok((id.clone(), r.get(0)?, r.get(1)?)),
            )
        })
        .collect()
}

/// What the hub compares two copies by: the conversation and what OpenCode
/// keeps about it that import brings back (titles, agent, model, metadata,
/// permissions), not where it is filed or when it was last touched. The
/// import rewrites `time.updated`, the project, the slug and clamps
/// `time.viewed`, so none of those can be part of it.
pub(crate) fn canonical_of(tree: &[Value]) -> String {
    let mut sessions: Vec<&Value> = tree.iter().collect();
    sessions.sort_by_key(|t| t.pointer("/info/id").and_then(Value::as_str).unwrap_or_default().to_string());
    // Fed to the hash as it is written: a tree can be large, and the text
    // of it is needed by nobody.
    let mut hash = Hasher(Sha256::new());
    for t in sessions {
        let mut info = Map::new();
        for key in ["id", "parentID", "title", "agent", "model", "metadata", "permissions", "cost", "tokens"] {
            if let Some(v) = t.pointer(&format!("/info/{key}")) {
                info.insert(key.into(), v.clone());
            }
        }
        write_sorted(&Value::Object(info), &mut hash);
        let _ = hash.write_char('\n');
        for message in t.get("messages").and_then(Value::as_array).into_iter().flatten() {
            write_sorted(message, &mut hash);
            let _ = hash.write_char('\n');
        }
    }
    format!("tree2:{:x}", hash.0.finalize())
}

/// The hash of one value, keys in order.
pub(crate) fn digest(value: &Value) -> String {
    let mut hash = Hasher(Sha256::new());
    write_sorted(value, &mut hash);
    format!("{:x}", hash.0.finalize())
}

/// A [`std::fmt::Write`] that hashes what is written to it.
struct Hasher(Sha256);

impl std::fmt::Write for Hasher {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.update(s.as_bytes());
        Ok(())
    }
}

/// JSON with object keys in order, whatever order the map keeps them in.
fn write_sorted<W: std::fmt::Write>(value: &Value, out: &mut W) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let _ = out.write_char('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    let _ = out.write_char(',');
                }
                let _ = write!(out, "{}", Value::String(key.clone()));
                let _ = out.write_char(':');
                write_sorted(&map[key], out);
            }
            let _ = out.write_char('}');
        }
        Value::Array(items) => {
            let _ = out.write_char('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    let _ = out.write_char(',');
                }
                write_sorted(item, out);
            }
            let _ = out.write_char(']');
        }
        other => {
            let _ = write!(out, "{other}");
        }
    }
}

/// The IR of one session of a transfer document, with no database or
/// `opencode` process: what the hub shows for a pushed 2.x session.
pub(crate) fn ir_from_transfer(transfer: &Value) -> IrSession {
    let info = transfer.get("info").unwrap_or(&Value::Null);
    let ms = |key: &str| info.pointer(&format!("/time/{key}")).and_then(Value::as_i64).and_then(|ms| Timestamp::from_millisecond(ms).ok());
    let session = Session {
        handle: SessionRef {
            agent: AgentKind::OpenCode,
            native_id: text(info, "id").unwrap_or_default().to_string(),
            location: SessionLocation::SqliteRow { db: PathBuf::new(), table: "session_v2".into() },
        },
        title: text(info, "title").map(str::to_string),
        slug: None,
        project_root: PathBuf::from(info.pointer("/location/directory").and_then(Value::as_str).unwrap_or_default()),
        git_branch: None,
        created: ms("created"),
        updated: ms("updated"),
        model: info.pointer("/model/id").and_then(Value::as_str).map(str::to_string),
        usage: Default::default(),
        status: SessionStatus::Idle,
        parent: text(info, "parentID").map(str::to_string),
        agent_version: None,
        size_bytes: None,
    };
    let messages = transfer.get("messages").and_then(Value::as_array).into_iter().flatten();
    ir_session(&session, messages.filter_map(convert_message).collect())
}

// -- the transcript ----------------------------------------------------

fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// One message (`id` and `type` inside it, as in a transfer document) as an
/// IR message; `None` for those that carry nothing to read (the markers, an
/// assistant step that produced no content).
pub(crate) fn convert_message(message: &Value) -> Option<IrMessage> {
    let created = message.pointer("/time/created").and_then(Value::as_i64);
    convert(text(message, "id").unwrap_or_default(), text(message, "type").unwrap_or_default(), created, message)
}

fn convert(id: &str, kind: &str, created: Option<i64>, data: &Value) -> Option<IrMessage> {
    let mut ext = Map::new();
    let mut parts = Vec::new();
    let role = match kind {
        "user" => {
            if let Some(t) = text(data, "text").filter(|t| !t.is_empty()) {
                parts.push(IrPart::Text { text: t.to_string() });
            }
            for file in data.get("files").and_then(Value::as_array).into_iter().flatten() {
                parts.push(file_part(file));
            }
            for key in ["agents", "skills"] {
                if let Some(v) = data.get(key).filter(|a| a.as_array().is_some_and(|a| !a.is_empty())) {
                    ext.insert(key.into(), v.clone());
                }
            }
            IrRole::User
        }
        "assistant" => {
            for item in data.get("content").and_then(Value::as_array).into_iter().flatten() {
                parts.extend(content_item(item));
            }
            for key in ["agent", "model", "finish", "cost", "tokens", "error", "snapshot"] {
                if let Some(v) = data.get(key).filter(|v| !v.is_null()) {
                    ext.insert(key.into(), v.clone());
                }
            }
            // A step that failed before saying anything is still worth
            // showing; one that was merely opened is not.
            if parts.is_empty() && !ext.contains_key("error") {
                return None;
            }
            IrRole::Assistant
        }
        // A command the user ran with `!`: a tool call and its output, like
        // 1.x records it.
        "shell" => {
            let call_id = text(data, "shellID").unwrap_or(id).to_string();
            parts.push(IrPart::ToolCall {
                call_id: call_id.clone(),
                name: "shell".into(),
                input: json!({ "command": text(data, "command").unwrap_or_default() }),
            });
            let status = text(data, "status");
            if status != Some("running") {
                let output = data.get("output");
                parts.push(IrPart::ToolResult {
                    call_id,
                    // A page of output: `{output, cursor, size, truncated}`.
                    output: output
                        .and_then(|o| o.as_str().or_else(|| text(o, "output")))
                        .unwrap_or_default()
                        .to_string(),
                    is_error: matches!(status, Some("timeout" | "killed"))
                        || data.get("exit").and_then(Value::as_i64).is_some_and(|code| code != 0),
                    truncated: output.and_then(|o| o.get("truncated")).and_then(Value::as_bool).unwrap_or(false),
                });
            }
            IrRole::Assistant
        }
        "system" | "synthetic" => {
            parts.push(IrPart::Text { text: text(data, "text").unwrap_or_default().to_string() });
            if let Some(d) = text(data, "description") {
                ext.insert("description".into(), json!(d));
            }
            IrRole::System
        }
        "skill" => {
            parts.push(IrPart::Text { text: text(data, "text").unwrap_or_default().to_string() });
            for key in ["skill", "name"] {
                if let Some(v) = data.get(key) {
                    ext.insert(key.into(), v.clone());
                }
            }
            IrRole::System
        }
        "compaction" => {
            let body = match text(data, "status") {
                Some("failed") => format!(
                    "Compaction failed: {}",
                    data.pointer("/error/message").and_then(Value::as_str).unwrap_or("unknown error")
                ),
                _ => text(data, "summary").unwrap_or_default().to_string(),
            };
            parts.push(IrPart::Text { text: body });
            for key in ["reason", "recent", "status"] {
                if let Some(v) = data.get(key) {
                    ext.insert(key.into(), v.clone());
                }
            }
            IrRole::System
        }
        // Bookkeeping: a turn ended, the agent, model or place changed.
        "idle" | "agent-switched" | "model-switched" | "location-switched" => return None,
        // A kind newer than this reader: keep it raw rather than lose it.
        other => {
            ext.insert("type".into(), json!(other));
            ext.insert("raw".into(), data.clone());
            IrRole::System
        }
    };
    if let Some(metadata) = data.get("metadata").filter(|m| m.as_object().is_some_and(|m| !m.is_empty())) {
        ext.insert("metadata".into(), metadata.clone());
    }
    let mut extensions = ExtBag::new();
    if !ext.is_empty() {
        extensions.insert("opencode".into(), Value::Object(ext));
    }
    Some(IrMessage {
        role,
        timestamp: created.and_then(|ms| Timestamp::from_millisecond(ms).ok()),
        parts,
        source_id: Some(id.to_string()),
        extensions,
    })
}

/// A prompt attachment (`{data, mime, source: {type, uri?}, name?}`) or a
/// tool's file output (`{uri, mime, name?}`). Inline bytes are not carried.
fn file_part(file: &Value) -> IrPart {
    let uri = file.pointer("/source/uri").and_then(Value::as_str).or_else(|| text(file, "uri"));
    IrPart::File {
        path: uri.and_then(|u| u.strip_prefix("file://")).map(|p| PortablePath::from_path(Path::new(p))),
        mime: text(file, "mime").map(str::to_string),
        content: None,
    }
}

fn content_item(item: &Value) -> Vec<IrPart> {
    match text(item, "type") {
        Some("text") => text(item, "text").map(|t| vec![IrPart::Text { text: t.to_string() }]).unwrap_or_default(),
        Some("reasoning") => {
            // Encrypted/signed provider payloads live under `state`.
            let state = item.get("state").or_else(|| item.get("providerMetadata")).map(Value::to_string).unwrap_or_default();
            vec![IrPart::Reasoning {
                summary: text(item, "text").unwrap_or_default().to_string(),
                opaque: state.contains("ncryptedContent") || state.contains("signature"),
            }]
        }
        Some("tool") => tool(item),
        _ => Vec::new(),
    }
}

fn tool(item: &Value) -> Vec<IrPart> {
    let call_id = text(item, "id").unwrap_or_default().to_string();
    let state = item.get("state").unwrap_or(&Value::Null);
    // A call still being typed holds its input as the raw JSON text so far.
    let input = match state.get("input") {
        Some(Value::String(raw)) => serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.clone())),
        Some(v) => v.clone(),
        None => Value::Null,
    };
    let mut parts = vec![IrPart::ToolCall {
        call_id: call_id.clone(),
        name: text(item, "name").unwrap_or_default().to_string(),
        input,
    }];
    let (output, is_error) = match text(state, "status") {
        Some("completed") => (Some(tool_text(state)), false),
        Some("error") => {
            let message = state.pointer("/error/message").and_then(Value::as_str).unwrap_or("the tool failed");
            let partial = tool_text(state);
            (Some(if partial.is_empty() { message.to_string() } else { format!("{message}\n{partial}") }), true)
        }
        // streaming/running: no result yet (or ever, if the turn was cut off).
        _ => (None, false),
    };
    if let Some(output) = output {
        let truncated = state.pointer("/metadata/truncated").and_then(Value::as_bool).unwrap_or(false);
        parts.push(IrPart::ToolResult { call_id, output, is_error, truncated });
    }
    parts
}

/// A tool's `content`: text as is, files as a one-line reference.
fn tool_text(state: &Value) -> String {
    state
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| match text(c, "type") {
            Some("text") => text(c, "text").map(str::to_string),
            Some("file") => {
                let name = text(c, "name").or_else(|| text(c, "uri")).unwrap_or("file");
                Some(format!("[file: {name} ({})]", text(c, "mime").unwrap_or("unknown type")))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};
    use serde_json::json;

    use super::super::{OpenCodeAdapter, Schema, detect_schema, hub};
    use crate::adapter::{AgentRead, AgentWrite, SessionFilter};
    use crate::ir::{IrPart, IrRole};
    use crate::model::{SessionLocation, SessionStatus};

    /// `project`, `session_v2` and `session_message` with their indexes,
    /// copied verbatim from `sqlite_master` of an OpenCode 2.0.25 database.
    const DDL: &str = r#"CREATE TABLE `project` (
          `id` text PRIMARY KEY,
          `worktree` text NOT NULL,
          `vcs` text,
          `name` text,
          `icon_url` text,
          `icon_url_override` text,
          `icon_color` text,
          `time_created` integer NOT NULL,
          `time_updated` integer NOT NULL,
          `time_initialized` integer,
          `time_active` integer DEFAULT 0 NOT NULL,
          `sandboxes` text NOT NULL,
          `commands` text
        );
CREATE TABLE `session_v2` (
          `id` text PRIMARY KEY,
          `project_id` text NOT NULL,
          `workspace_id` text,
          `parent_id` text,
          `fork_session_id` text,
          `fork_boundary` text,
          `slug` text NOT NULL,
          `directory` text NOT NULL,
          `path` text,
          `title` text,
          `version` text NOT NULL,
          `share_url` text,
          `summary_additions` integer,
          `summary_deletions` integer,
          `summary_files` integer,
          `summary_diffs` text,
          `metadata` text,
          `cost` real DEFAULT 0 NOT NULL,
          `tokens_input` integer DEFAULT 0 NOT NULL,
          `tokens_output` integer DEFAULT 0 NOT NULL,
          `tokens_reasoning` integer DEFAULT 0 NOT NULL,
          `tokens_cache_read` integer DEFAULT 0 NOT NULL,
          `tokens_cache_write` integer DEFAULT 0 NOT NULL,
          `revert` text,
          `permission` text,
          `agent` text,
          `model` text,
          `time_created` integer NOT NULL,
          `time_updated` integer NOT NULL,
          `time_idle` integer,
          `time_viewed` integer,
          `idle_outcome` text,
          `time_compacting` integer,
          `time_archived` integer,
          `time_suspended` integer,
          `resume_attempts` integer DEFAULT 0 NOT NULL,
          CONSTRAINT `fk_session_v2_project_id_project_id_fk` FOREIGN KEY (`project_id`) REFERENCES `project`(`id`) ON DELETE CASCADE
        );
CREATE INDEX `session_v2_project_idx` ON `session_v2` (`project_id`);
CREATE INDEX `session_v2_workspace_idx` ON `session_v2` (`workspace_id`);
CREATE INDEX `session_v2_parent_idx` ON `session_v2` (`parent_id`);
CREATE INDEX `session_v2_time_suspended_idx` ON `session_v2` (`time_suspended`) WHERE "session_v2"."time_suspended" is not null;
CREATE TABLE `session_message` (
          `id` text PRIMARY KEY,
          `session_id` text NOT NULL,
          `type` text NOT NULL,
          `seq` integer NOT NULL,
          `time_created` integer NOT NULL,
          `time_updated` integer NOT NULL,
          `data` text NOT NULL,
          CONSTRAINT `fk_session_message_session_id_session_v2_id_fk` FOREIGN KEY (`session_id`) REFERENCES `session_v2`(`id`) ON DELETE CASCADE
        );
CREATE UNIQUE INDEX `session_message_session_seq_idx` ON `session_message` (`session_id`,`seq`);
CREATE INDEX `session_message_session_type_seq_idx` ON `session_message` (`session_id`,`type`,`seq`);
CREATE INDEX `session_message_session_time_created_id_idx` ON `session_message` (`session_id`,`time_created`,`id`);
CREATE INDEX `session_message_time_created_idx` ON `session_message` (`time_created`);
CREATE TABLE `session_inbox` (
          `id` text PRIMARY KEY,
          `session_id` text NOT NULL,
          `type` text NOT NULL,
          `payload` text NOT NULL,
          `delivery` text NOT NULL,
          `enqueued_seq` integer NOT NULL,
          `time_created` integer NOT NULL,
          CONSTRAINT `fk_session_inbox_session_id_session_v2_id_fk` FOREIGN KEY (`session_id`) REFERENCES `session_v2`(`id`) ON DELETE CASCADE
        );
CREATE UNIQUE INDEX `session_inbox_session_enqueued_seq_idx` ON `session_inbox` (`session_id`,`enqueued_seq`);
CREATE TABLE `session_pending` (
          `id` text PRIMARY KEY,
          `session_id` text NOT NULL,
          `type` text NOT NULL,
          `data` text NOT NULL,
          `delivery` text,
          `admitted_seq` integer NOT NULL,
          `time_created` integer NOT NULL,
          CONSTRAINT `fk_session_pending_session_id_session_v2_id_fk` FOREIGN KEY (`session_id`) REFERENCES `session_v2`(`id`) ON DELETE CASCADE
        );
CREATE UNIQUE INDEX `session_pending_session_admitted_seq_idx` ON `session_pending` (`session_id`,`admitted_seq`);"#;

    const ROOT: &str = "/home/user/projects/openrpc";

    struct Store {
        _dir: tempfile::TempDir,
        db: std::path::PathBuf,
        conn: Connection,
    }

    impl Store {
        fn adapter(&self) -> OpenCodeAdapter {
            OpenCodeAdapter::with_db(&self.db)
        }

        fn session(&self, id: &str, parent: Option<&str>, title: Option<&str>, created: i64, archived: Option<i64>, suspended: Option<i64>) {
            self.conn
                .execute(
                    "INSERT INTO session_v2 (id, project_id, parent_id, slug, directory, title, version, agent, model,
                        cost, tokens_input, tokens_output, tokens_cache_read, tokens_cache_write,
                        time_created, time_updated, time_archived, time_suspended)
                     VALUES (?1, 'prj_1', ?2, ?3, ?4, ?5, '2.0.25', 'build',
                        '{\"id\":\"claude-sonnet-5-5\",\"providerID\":\"anthropic\",\"variant\":\"default\"}',
                        0.5, 100, 200, 30, 40, ?6, ?6 + 1000, ?7, ?8)",
                    params![id, parent, format!("slug-{id}"), ROOT, title, created, archived, suspended],
                )
                .unwrap();
        }

        fn message(&self, session: &str, seq: i64, id: &str, kind: &str, data: serde_json::Value) {
            self.conn
                .execute(
                    "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
                    params![id, session, kind, seq, 1_800_000_000_000_i64 + seq * 1000, data.to_string()],
                )
                .unwrap();
        }
    }

    /// Plain exchange, a busy session (every message kind), a subagent
    /// child, an archived session and one claimed by a running turn.
    fn store() -> Store {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(DDL).unwrap();
        conn.execute(
            "INSERT INTO project (id, worktree, sandboxes, time_created, time_updated)
             VALUES ('prj_1', ?1, '[]', 1, 1)",
            [ROOT],
        )
        .unwrap();
        let store = Store { _dir: dir, db, conn };

        store.session("ses_plain", None, Some("Plain exchange"), 1_800_000_000_000, None, None);
        store.message("ses_plain", 1, "msg_1", "user", json!({"text": "Hello there", "time": {"created": 1}}));
        store.message(
            "ses_plain",
            2,
            "msg_2",
            "assistant",
            json!({"agent": "build", "model": {"id": "claude-sonnet-5-5", "providerID": "anthropic"},
                   "content": [{"type": "text", "id": "t1", "text": "Hi, how can I help?"}],
                   "finish": "stop", "cost": 0.01,
                   "tokens": {"input": 10, "output": 5, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                   "time": {"created": 2, "completed": 3}}),
        );

        store.session("ses_busy", None, Some("Busy session"), 1_800_000_100_000, None, None);
        store.message(
            "ses_busy",
            1,
            "msg_b1",
            "user",
            json!({"text": "Fix the build", "files": [{"data": "", "source": {"type": "uri", "uri": "file:///home/user/projects/openrpc/Cargo.toml"}, "mime": "text/plain"}],
                   "agents": [{"name": "general"}], "time": {"created": 1}}),
        );
        store.message("ses_busy", 2, "msg_b2", "agent-switched", json!({"agent": "plan", "time": {"created": 2}}));
        store.message("ses_busy", 3, "msg_b3", "system", json!({"text": "You are in plan mode", "time": {"created": 3}}));
        store.message(
            "ses_busy",
            4,
            "msg_b4",
            "assistant",
            json!({"agent": "plan", "model": {"id": "claude-sonnet-5-5", "providerID": "anthropic"},
                   "content": [
                     {"type": "reasoning", "id": "r1", "text": "Let me look first.",
                      "providerMetadata": {"anthropic": {"signature": "abc"}}},
                     {"type": "text", "id": "t1", "text": "Running the build."},
                     {"type": "tool", "id": "call_ok", "name": "bash",
                      "state": {"status": "completed", "input": {"command": "cargo build"},
                                "content": [{"type": "text", "text": "Finished dev"}, {"type": "text", "text": "0 warnings"}],
                                "metadata": {"truncated": true}},
                      "time": {"created": 4}},
                     {"type": "tool", "id": "call_bad", "name": "read",
                      "state": {"status": "error", "input": {"filePath": "/nope"},
                                "error": {"type": "unknown", "message": "ENOENT: no such file"}},
                      "time": {"created": 4}},
                     {"type": "tool", "id": "call_typing", "name": "write",
                      "state": {"status": "streaming", "input": "{\"filePath\": \"/x\""}, "time": {"created": 4}}
                   ],
                   "finish": "tool-calls", "cost": 0.02,
                   "tokens": {"input": 1, "output": 2, "reasoning": 3, "cache": {"read": 4, "write": 5}},
                   "time": {"created": 4}}),
        );
        store.message(
            "ses_busy",
            5,
            "msg_b5",
            "shell",
            json!({"shellID": "sh_1", "command": "ls -la", "status": "exited", "exit": 0,
                   "output": {"output": "total 0", "cursor": 7, "size": 7, "truncated": false},
                   "time": {"created": 5, "completed": 6}}),
        );
        store.message(
            "ses_busy",
            6,
            "msg_b6",
            "compaction",
            json!({"status": "completed", "reason": "auto", "summary": "We fixed the build.", "recent": "msg_b5", "time": {"created": 6}}),
        );
        store.message("ses_busy", 7, "msg_b7", "synthetic", json!({"sessionID": "ses_busy", "text": "background job done", "time": {"created": 7}}));
        store.message("ses_busy", 8, "msg_b8", "model-switched", json!({"model": {"id": "x", "providerID": "y"}, "time": {"created": 8}}));
        store.message(
            "ses_busy",
            9,
            "msg_b9",
            "assistant",
            json!({"agent": "plan", "model": {"id": "m", "providerID": "p"}, "content": [], "time": {"created": 9}}),
        );

        store.message("ses_busy", 10, "msg_b10", "idle", json!({"outcome": "succeeded", "time": {"created": 10}}));
        store.message("ses_busy", 11, "msg_b11", "location-switched", json!({"location": {"directory": "/x"}, "time": {"created": 11}}));

        store.session("ses_child", Some("ses_busy"), Some("Review (@general subagent)"), 1_800_000_200_000, None, None);
        store.session("ses_old", None, None, 1_700_000_000_000, Some(1_700_000_500_000), None);
        store.session("ses_running", None, Some("Mid-turn"), 1_800_000_300_000, None, Some(1_800_000_310_000));
        store
    }

    fn find<'a>(sessions: &'a [crate::model::Session], id: &str) -> &'a crate::model::Session {
        sessions.iter().find(|s| s.handle.native_id == id).unwrap_or_else(|| panic!("{id} not listed"))
    }

    #[test]
    fn lists_sessions_hiding_children() {
        let store = store();
        let sessions = store.adapter().sessions(&SessionFilter::default()).unwrap();
        assert_eq!(sessions.len(), 4, "the subagent child is hidden");
        let plain = find(&sessions, "ses_plain");
        assert_eq!(plain.title.as_deref(), Some("Plain exchange"));
        assert_eq!(plain.slug.as_deref(), Some("slug-ses_plain"));
        assert_eq!(plain.project_root, std::path::PathBuf::from(ROOT));
        assert_eq!(plain.agent_version.as_deref(), Some("2.0.25"));
        assert_eq!(plain.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(plain.usage.input_tokens, Some(100));
        assert_eq!(plain.usage.output_tokens, Some(200));
        assert_eq!(plain.usage.cache_read_tokens, Some(30));
        assert_eq!(plain.usage.cache_write_tokens, Some(40));
        assert_eq!(plain.usage.cost_usd, Some(0.5));
        assert_eq!(plain.created.unwrap().as_millisecond(), 1_800_000_000_000);
        assert_eq!(plain.updated.unwrap().as_millisecond(), 1_800_000_001_000);
        assert!(plain.size_bytes.unwrap() > 0, "size is the stored message JSON");
        assert!(plain.parent.is_none() && plain.git_branch.is_none());
        assert_eq!(plain.status, SessionStatus::Idle);
        assert!(matches!(&plain.handle.location, SessionLocation::SqliteRow { table, .. } if table == "session_v2"));
        // Newest first.
        assert_eq!(sessions[0].handle.native_id, "ses_running");
    }

    #[test]
    fn children_and_archived_sessions() {
        let store = store();
        let all = store
            .adapter()
            .sessions(&SessionFilter { include_children: true, ..SessionFilter::default() })
            .unwrap();
        assert_eq!(all.len(), 5);
        assert_eq!(find(&all, "ses_child").parent.as_deref(), Some("ses_busy"));
        // 2.0.25 ignores `time_archived`: a session archived in 1.x is an
        // ordinary one here (it could never be unarchived otherwise).
        let old = find(&all, "ses_old");
        assert_eq!(old.status, SessionStatus::Idle);
        assert_eq!(old.title, None);
        let by_project = store
            .adapter()
            .sessions(&SessionFilter { project: Some(ROOT.into()), ..SessionFilter::default() })
            .unwrap();
        assert_eq!(by_project.len(), 4);
    }

    /// `time_suspended` means a turn is claimed; only a service that is up
    /// makes it live. "Up" is proven by the service answering, not by a pid
    /// that happens to be alive.
    #[test]
    fn a_claimed_session_is_live_only_while_the_service_is() {
        use super::super::cli2::testenv::{json_reply, register, serve};
        let store = store();
        let state = store._dir.path().join("state/opencode");
        std::fs::create_dir_all(&state).unwrap();
        let locks = state.join("locks");
        let adapter = || OpenCodeAdapter::with_db(&store.db).with_lock_dir(&locks);
        let status = |a: &OpenCodeAdapter| find(&a.sessions(&SessionFilter::default()).unwrap(), "ses_running").status;
        let me = std::process::id();

        assert_eq!(status(&adapter()), SessionStatus::Idle, "no service file: cannot tell, so idle");
        let (port, _) = serve(json_reply(&format!(r#"{{"version":"2.0.25","pid":{me}}}"#)));
        register(&state, me, port, json!({}));
        assert_eq!(status(&adapter()), SessionStatus::Live { pid: Some(me) });
        // A live pid that is not the service (the pid was reused): nobody answers as it.
        let mut stranger = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        register(&state, stranger.id(), free, json!({}));
        assert_eq!(status(&adapter()), SessionStatus::Idle, "a stranger's pid is not a running service");
        stranger.kill().unwrap();
        let _ = stranger.wait();
        std::fs::write(state.join("service.json"), json!({"pid": 4_000_000_000_u64}).to_string()).unwrap();
        assert_eq!(status(&adapter()), SessionStatus::Idle, "its service is gone: an interrupted turn, not a live one");
        // An unclaimed session is never live.
        register(&state, me, port, json!({}));
        let sessions = adapter().sessions(&SessionFilter::default()).unwrap();
        assert_eq!(find(&sessions, "ses_plain").status, SessionStatus::Idle);
    }

    #[test]
    fn exports_a_plain_exchange() {
        let store = store();
        let adapter = store.adapter();
        let session = find(&adapter.sessions(&SessionFilter::default()).unwrap(), "ses_plain").clone();
        let ir = adapter.export_ir(&session).unwrap();
        assert_eq!(ir.title.as_deref(), Some("Plain exchange"));
        assert_eq!(ir.messages.len(), 2);
        assert_eq!(ir.messages[0].role, IrRole::User);
        assert!(matches!(&ir.messages[0].parts[..], [IrPart::Text { text }] if text == "Hello there"));
        assert_eq!(ir.messages[1].role, IrRole::Assistant);
        assert!(matches!(&ir.messages[1].parts[..], [IrPart::Text { text }] if text == "Hi, how can I help?"));
        let meta = &ir.messages[1].extensions["opencode"];
        assert_eq!(meta["finish"], "stop");
        assert_eq!(meta["tokens"]["output"], 5);
        assert_eq!(meta["model"]["id"], "claude-sonnet-5-5");
        assert_eq!(ir.messages[0].source_id.as_deref(), Some("msg_1"));
    }

    #[test]
    fn exports_every_message_kind_in_seq_order() {
        let store = store();
        let adapter = store.adapter();
        let session = find(&adapter.sessions(&SessionFilter::default()).unwrap(), "ses_busy").clone();
        let ir = adapter.export_ir(&session).unwrap();
        let ids: Vec<_> = ir.messages.iter().map(|m| m.source_id.clone().unwrap()).collect();
        // The agent/model switches and the contentless assistant step are dropped.
        assert_eq!(ids, ["msg_b1", "msg_b3", "msg_b4", "msg_b5", "msg_b6", "msg_b7"]);

        let user = &ir.messages[0];
        assert_eq!(user.role, IrRole::User);
        assert!(matches!(&user.parts[0], IrPart::Text { text } if text == "Fix the build"));
        assert!(matches!(&user.parts[1], IrPart::File { path: Some(p), mime: Some(m), .. }
            if p.0.ends_with("projects/openrpc/Cargo.toml") && m == "text/plain"));
        assert_eq!(user.extensions["opencode"]["agents"][0]["name"], "general");

        assert_eq!(ir.messages[1].role, IrRole::System);

        let assistant = &ir.messages[2];
        assert_eq!(assistant.role, IrRole::Assistant);
        let p = &assistant.parts;
        assert!(matches!(&p[0], IrPart::Reasoning { summary, opaque: true } if summary == "Let me look first."));
        assert!(matches!(&p[1], IrPart::Text { text } if text == "Running the build."));
        assert!(matches!(&p[2], IrPart::ToolCall { call_id, name, input }
            if call_id == "call_ok" && name == "bash" && input["command"] == "cargo build"));
        assert!(matches!(&p[3], IrPart::ToolResult { call_id, output, is_error: false, truncated: true }
            if call_id == "call_ok" && output == "Finished dev\n0 warnings"));
        assert!(matches!(&p[4], IrPart::ToolCall { name, .. } if name == "read"));
        assert!(matches!(&p[5], IrPart::ToolResult { call_id, output, is_error: true, .. }
            if call_id == "call_bad" && output == "ENOENT: no such file"));
        // Still being typed: the call, parsed as far as it goes, no result.
        assert!(matches!(&p[6], IrPart::ToolCall { call_id, input: serde_json::Value::String(raw), .. }
            if call_id == "call_typing" && raw.contains("filePath")));
        assert_eq!(p.len(), 7);
        assert_eq!(assistant.extensions["opencode"]["tokens"]["reasoning"], 3);
        assert_eq!(assistant.extensions["opencode"]["agent"], "plan");

        let shell = &ir.messages[3];
        assert!(matches!(&shell.parts[0], IrPart::ToolCall { call_id, name, input }
            if call_id == "sh_1" && name == "shell" && input["command"] == "ls -la"));
        assert!(matches!(&shell.parts[1], IrPart::ToolResult { output, is_error: false, .. } if output == "total 0"));

        let compaction = &ir.messages[4];
        assert_eq!(compaction.role, IrRole::System);
        assert!(matches!(&compaction.parts[..], [IrPart::Text { text }] if text == "We fixed the build."));
        assert_eq!(compaction.extensions["opencode"]["reason"], "auto");

        assert!(matches!(&ir.messages[5].parts[..], [IrPart::Text { text }] if text == "background job done"));
        // seq order, not insertion or id order.
        assert!(ir.messages.windows(2).all(|w| w[0].timestamp <= w[1].timestamp));
    }

    #[test]
    fn a_session_with_no_messages_exports_empty() {
        let store = store();
        let adapter = store.adapter();
        let session = find(&adapter.sessions(&SessionFilter::default()).unwrap(), "ses_running").clone();
        assert!(adapter.export_ir(&session).unwrap().messages.is_empty());
    }

    /// What a push compares: it must move when a message is appended or
    /// changed in place (a streamed tool call), and not for a viewing.
    #[test]
    fn the_fingerprint_moves_with_the_conversation() {
        let store = store();
        let adapter = store.adapter();
        let session = find(&adapter.sessions(&SessionFilter::default()).unwrap(), "ses_plain").clone();
        let fp = || hub::fingerprint(&adapter, &session).unwrap();
        let before = fp();
        assert_eq!(before, fp(), "stable while nothing changes");

        store.conn.execute("UPDATE session_v2 SET time_viewed = 5, time_suspended = 7 WHERE id = 'ses_plain'", []).unwrap();
        assert_eq!(before, fp(), "viewing or claiming is not a change to the conversation");

        store.conn.execute("UPDATE session_message SET time_updated = time_updated + 1 WHERE id = 'msg_2'", []).unwrap();
        let updated = fp();
        assert_ne!(before, updated, "a message rewritten in place");

        store.message("ses_plain", 3, "msg_3", "user", json!({"text": "more", "time": {"created": 3}}));
        assert_ne!(updated, fp(), "a message appended");

        // A subagent's messages count towards its parent's tree.
        let parent = find(&adapter.sessions(&SessionFilter::default()).unwrap(), "ses_busy").clone();
        let tree = hub::fingerprint(&adapter, &parent).unwrap();
        store.message("ses_child", 1, "msg_c1", "user", json!({"text": "child", "time": {"created": 1}}));
        assert_ne!(tree, hub::fingerprint(&adapter, &parent).unwrap());
    }

    #[test]
    fn what_opencode_2_cannot_do_says_why() {
        let store = store();
        let adapter = store.adapter();
        let session = find(&adapter.sessions(&SessionFilter::default()).unwrap(), "ses_plain").clone();
        let say = |r: Result<(), crate::CoreError>| r.unwrap_err().to_string();
        assert!(say(adapter.archive(&session).map(drop)).contains("OpenCode 2.x has no archive"));
        assert!(say(adapter.unarchive(&session)).contains("OpenCode 2.x has no archive"));
        assert!(say(adapter.relocate(&session, ROOT.as_ref()).map(drop)).contains("cannot move"));
        let caps = adapter.capabilities();
        assert!(caps.list && caps.read_transcript && caps.export_ir && caps.resume_native);
        assert!(caps.rename && caps.delete && caps.import_ir);
        assert!(!caps.archive && !caps.relocate && !caps.send_message);
        // `send` is refused before any process exists, and has no command to run.
        assert!(crate::live::AgentLive::send_command(&adapter, &session, "hi").is_none());
        assert!(OpenCodeAdapter::with_db("").capabilities().archive, "hub planning sees the full set");
        // Nothing was touched.
        let still = adapter.sessions(&SessionFilter::default()).unwrap();
        assert_eq!(find(&still, "ses_plain").title.as_deref(), Some("Plain exchange"));
    }

    // -- the exporter ------------------------------------------------------

    fn transfer(store: &Store, id: &str) -> serde_json::Value {
        super::super::v2::transfer(&store.conn, id, true).unwrap().unwrap()
    }

    /// `info` the way `Session.fromRow` shapes it: what is unset is absent,
    /// the model gets its default variant, the cost is a plain number.
    #[test]
    fn the_exporter_shapes_info_like_opencode_does() {
        let store = store();
        let plain = transfer(&store, "ses_plain");
        assert_eq!(plain["info"], json!({
            "id": "ses_plain", "projectID": "prj_1", "agent": "build",
            "model": {"id": "claude-sonnet-5-5", "providerID": "anthropic", "variant": "default"},
            "cost": 0.5,
            "tokens": {"input": 100, "output": 200, "reasoning": 0, "cache": {"read": 30, "write": 40}},
            "time": {"created": 1_800_000_000_000_i64, "updated": 1_800_000_001_000_i64},
            "title": "Plain exchange", "location": {"directory": ROOT},
        }));
        let child = transfer(&store, "ses_child");
        assert_eq!(child["info"]["parentID"], "ses_busy");
        let old = transfer(&store, "ses_old");
        assert!(old["info"].get("title").is_none(), "no title: no key");
        assert_eq!(old["info"]["time"]["archived"], 1_700_000_500_000_i64);
        store.conn.execute("UPDATE session_v2 SET cost = 0.0, metadata = '{\"k\":1}', permission = '[{\"p\":1}]', idle_outcome = 'failed', time_idle = 9, time_viewed = 8 WHERE id = 'ses_plain'", []).unwrap();
        let again = transfer(&store, "ses_plain");
        assert_eq!(again["info"]["cost"], json!(0), "0.0 is the 0 OpenCode prints");
        assert!(again["info"]["cost"].is_i64());
        assert_eq!(again["info"]["metadata"], json!({"k": 1}));
        assert_eq!(again["info"]["permissions"], json!([{"p": 1}]));
        assert_eq!(again["info"]["outcome"], "failed");
        assert_eq!(again["info"]["time"]["idle"], 9);
        assert_eq!(again["info"]["time"]["viewed"], 8);
        assert!(super::super::v2::transfer(&store.conn, "ses_nope", true).unwrap().is_none());
    }

    #[test]
    fn the_exporter_puts_id_and_type_back_and_leaves_out_what_is_in_flight() {
        let store = store();
        let plain = transfer(&store, "ses_plain");
        assert_eq!(plain["messages"][0], json!({"id": "msg_1", "type": "user", "text": "Hello there", "time": {"created": 1}}));
        assert_eq!(plain["messages"].as_array().unwrap().len(), 2);

        // ses_busy's msg_b4 and msg_b9 are assistants that never completed.
        let busy = transfer(&store, "ses_busy");
        let ids: Vec<&str> = busy["messages"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["msg_b1", "msg_b2", "msg_b3", "msg_b5", "msg_b6", "msg_b7", "msg_b8", "msg_b10", "msg_b11"]);
        store.message("ses_busy", 12, "msg_b12", "shell", json!({"status": "running", "command": "sleep 9", "time": {"created": 12}}));
        store.message("ses_busy", 13, "msg_b13", "compaction", json!({"status": "running", "time": {"created": 13}}));
        store.message("ses_busy", 14, "msg_b14", "assistant", json!({"time": {"created": 14, "completed": 15}, "content": []}));
        let after = transfer(&store, "ses_busy");
        let ids: Vec<&str> = after["messages"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
        assert_eq!(&ids[9..], ["msg_b14"], "running shell and compaction are left out, a completed assistant is not");
        // The reading side still sees the turn in flight.
        let live = super::super::v2::messages_of(&store.conn, "ses_busy", false).unwrap();
        assert_eq!(live.len(), 14);
    }

    #[test]
    fn a_tree_is_exported_parents_first() {
        let store = store();
        store.session("ses_grand", Some("ses_child"), Some("Grandchild"), 1_800_000_400_000, None, None);
        store.session("ses_aaa", Some("ses_busy"), Some("Second child"), 1_800_000_500_000, None, None);
        let tree = super::super::v2::transfer_tree(&store.conn, "ses_busy", true).unwrap();
        let ids: Vec<&str> = tree.iter().map(|t| t["info"]["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["ses_busy", "ses_aaa", "ses_child", "ses_grand"]);
        assert_eq!(super::super::v2::tree_ids(&store.conn, "ses_plain").unwrap(), ["ses_plain"]);
        // Nothing is left open: a write right after works.
        store.session("ses_after", None, None, 1, None, None);
    }

    #[test]
    fn the_hub_transcript_reads_a_transfer_document_like_the_database() {
        let store = store();
        let adapter = store.adapter();
        let session = find(&adapter.sessions(&SessionFilter::default()).unwrap(), "ses_plain").clone();
        let from_db = adapter.export_ir(&session).unwrap();
        let from_doc = super::super::v2::ir_from_transfer(&transfer(&store, "ses_plain"));
        assert_eq!(from_doc.messages.len(), from_db.messages.len());
        for (a, b) in from_doc.messages.iter().zip(&from_db.messages) {
            assert_eq!(serde_json::to_value(a).unwrap(), serde_json::to_value(b).unwrap());
        }
        assert_eq!(from_doc.title.as_deref(), Some("Plain exchange"));
        assert_eq!(from_doc.project_path.0, ROOT);
    }

    // -- busy sessions, backups, rename and delete without a binary ---------------

    /// An `opencode` that records how it was called and changes nothing.
    fn fake_cli(store: &Store) -> (super::super::cli2::Cli, std::path::PathBuf) {
        let dir = store._dir.path();
        let log = dir.join("calls.log");
        let script = dir.join("opencode");
        std::fs::write(&script, format!("#!/bin/sh\necho \"$*\" >> {}\n", log.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (super::super::cli2::Cli::isolated(script, vec![], dir.join("state"), dir), log)
    }

    #[test]
    fn a_busy_session_is_refused_before_anything_runs() {
        let store = store();
        let (cli, log) = fake_cli(&store);
        let adapter = store.adapter().with_cli(cli).with_backup_root(store._dir.path().join("backups"));
        let sessions = adapter.sessions(&SessionFilter { include_children: true, ..SessionFilter::default() }).unwrap();
        let running = find(&sessions, "ses_running").clone();

        // A claim whose service is gone is an unfinished turn from an OpenCode
        // that is no longer running; say so, and do not suggest closing it,
        // nor anything that would resume the turn silently.
        let err = adapter.rename(&running, "New").unwrap_err();
        let text = err.to_string();
        assert!(matches!(err, crate::CoreError::Invalid { .. }), "{err}");
        assert!(
            text.contains("ses_running")
                && text.contains("an unfinished turn from an OpenCode that is no longer running")
                && text.contains("start OpenCode once to finish or abandon it, then retry"),
            "{text}"
        );
        assert!(!text.contains("close OpenCode"), "{text}");
        assert!(adapter.delete(&running).is_err());

        // With the service up it is a turn that is running.
        super::super::cli2::testenv::service_up(&store._dir.path().join("state"));
        let err = adapter.rename(&running, "New").unwrap_err();
        assert!(matches!(err, crate::CoreError::StoreBusy { .. }), "{err}");
        assert!(err.to_string().contains("ses_running") && err.to_string().contains("a turn is running"), "{err}");
        std::fs::remove_file(store._dir.path().join("state/service.json")).unwrap();

        // Queued work counts, in either table; and a busy child blocks its parent's delete.
        let plain = find(&sessions, "ses_plain").clone();
        store.conn.execute("INSERT INTO session_inbox VALUES ('inb_1', 'ses_plain', 't', '{}', 'next', 1, 1)", []).unwrap();
        assert!(matches!(adapter.rename(&plain, "New").unwrap_err(), crate::CoreError::StoreBusy { .. }));
        store.conn.execute("DELETE FROM session_inbox", []).unwrap();
        store.conn.execute("INSERT INTO session_pending VALUES ('pen_1', 'ses_plain', 't', '{}', NULL, 1, 1)", []).unwrap();
        assert!(adapter.rename(&plain, "New").is_err());
        store.conn.execute("DELETE FROM session_pending", []).unwrap();
        store.conn.execute("UPDATE session_v2 SET time_suspended = 5 WHERE id = 'ses_child'", []).unwrap();
        let busy = find(&sessions, "ses_busy").clone();
        assert!(adapter.delete(&busy).unwrap_err().to_string().contains("ses_child"));
        assert!(!log.exists(), "opencode was never started");
        assert!(!store._dir.path().join("backups").exists(), "and nothing was backed up for a refusal");
    }

    #[test]
    fn rename_refuses_an_empty_title_and_a_missing_session() {
        let store = store();
        let (cli, log) = fake_cli(&store);
        let adapter = store.adapter().with_cli(cli).with_backup_root(store._dir.path().join("backups"));
        let plain = find(&adapter.sessions(&SessionFilter::default()).unwrap(), "ses_plain").clone();
        assert!(adapter.rename(&plain, "  ").unwrap_err().to_string().contains("cannot be empty"));
        let mut gone = plain.clone();
        gone.handle.native_id = "ses_gone".into();
        assert!(adapter.rename(&gone, "x").unwrap_err().to_string().contains("not found"));
        assert!(!log.exists());
    }

    /// The backup is written, as export documents, before `opencode` is
    /// asked to delete; and a delete that did not happen is reported.
    #[test]
    fn delete_backs_the_whole_tree_up_first() {
        let store = store();
        let (cli, log) = fake_cli(&store);
        let adapter = store.adapter().with_cli(cli).with_backup_root(store._dir.path().join("backups"));
        let busy = find(&adapter.sessions(&SessionFilter::default()).unwrap(), "ses_busy").clone();
        let err = adapter.delete(&busy).unwrap_err().to_string();
        assert!(err.contains("still there") && err.contains("a backup is in"), "{err}");
        assert!(err.contains("opencode session import <file> --directory <directory>"), "how to restore: {err}");
        let called = std::fs::read_to_string(&log).unwrap();
        assert!(called.starts_with("session delete ses_busy --standalone"), "{called}");

        let dir = std::fs::read_dir(store._dir.path().join("backups/ses_busy")).unwrap().next().unwrap().unwrap().path();
        let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(names, ["00-ses_busy.json", "01-ses_child.json", "manifest.json"], "the root first: import in name order");
        let root: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("00-ses_busy.json")).unwrap()).unwrap();
        // Everything stored: the turn that never settled (msg_b4, msg_b9) is in the
        // backup although the export document leaves it out.
        let ids: Vec<&str> = root["messages"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"msg_b4") && ids.contains(&"msg_b9"), "{ids:?}");
        assert_eq!(root["messages"].as_array().unwrap().len(), 11);
        assert_eq!(super::settled(std::slice::from_ref(&root))[0], transfer(&store, "ses_busy"), "settled is the export");

        // Where each goes back, and nothing readable by anyone else.
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["root"], "ses_busy");
        assert_eq!(manifest["sessions"][0]["file"], "00-ses_busy.json");
        assert_eq!(manifest["sessions"][0]["directory"], ROOT);
        assert_eq!(manifest["sessions"][1]["parentID"], "ses_busy");
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        for name in &names {
            assert_eq!(mode(&dir.join(name)), 0o600, "{name}");
        }
    }

    /// A refusal at the last moment keeps the backup and deletes nothing: a
    /// child that appeared, a turn that started, a message that arrived.
    #[test]
    fn nothing_is_deleted_when_the_tree_changed_while_it_was_backed_up() {
        let store = store();
        let (cli, _log) = fake_cli(&store);
        let adapter = store.adapter().with_cli(cli).with_backup_root(store._dir.path().join("backups"));
        let ids = super::tree_ids(&store.conn, "ses_busy").unwrap();
        let signature = super::signature(&store.conn, &ids).unwrap();
        let backup = store._dir.path().join("backups/kept");
        let check = || super::super::write_v2::recheck(&adapter, &store.conn, "ses_busy", &ids, &signature, &backup);
        check().unwrap();

        store.session("ses_late_child", Some("ses_busy"), None, 1, None, None);
        let err = check().unwrap_err().to_string();
        assert!(err.contains("session tree changed") && err.contains("nothing was deleted") && err.contains("kept"), "{err}");
        store.conn.execute("DELETE FROM session_v2 WHERE id = 'ses_late_child'", []).unwrap();
        check().unwrap();

        store.message("ses_busy", 50, "msg_late", "user", json!({"text": "one more", "time": {"created": 50}}));
        assert!(check().unwrap_err().to_string().contains("was continued"));
        store.conn.execute("DELETE FROM session_message WHERE id = 'msg_late'", []).unwrap();
        check().unwrap();

        store.conn.execute("UPDATE session_v2 SET time_suspended = 9 WHERE id = 'ses_child'", []).unwrap();
        let err = check().unwrap_err().to_string();
        assert!(err.contains("ses_child") && err.contains("nothing was deleted"), "{err}");
    }

    /// After a delete nothing may be left of the tree or hang from it.
    #[test]
    fn a_delete_that_left_something_behind_is_reported_with_the_backup() {
        let store = store();
        let (cli, _log) = fake_cli(&store);
        let adapter = store.adapter().with_cli(cli);
        let backup = std::path::Path::new("/somewhere/backup");
        let verify = |ids: &[&str]| {
            let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
            super::super::write_v2::verify_gone(&adapter, &store.conn, &ids, backup)
        };
        verify(&["ses_never_existed"]).unwrap();
        let err = verify(&["ses_plain"]).unwrap_err().to_string();
        assert!(err.contains("ses_plain is still there") && err.contains("/somewhere/backup"), "{err}");
        // The deleted parent's child that was added at the last moment.
        store.session("ses_orphan", Some("ses_gone_parent"), None, 1, None, None);
        assert!(verify(&["ses_gone_parent"]).unwrap_err().to_string().contains("ses_orphan is still there"));
    }

    /// Two backups of one session in one millisecond do not share a directory,
    /// and the directory is private.
    #[test]
    fn backup_directories_are_unique_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let want = dir.path().join("backups/ses_x/1700000000000");
        let first = super::super::write_v2::private_dir_for_test(&want).unwrap();
        let second = super::super::write_v2::private_dir_for_test(&want).unwrap();
        let third = super::super::write_v2::private_dir_for_test(&want).unwrap();
        assert_eq!(first, want);
        assert_ne!(second, first);
        assert_ne!(third, second);
        assert!(second.ends_with("1700000000000-1") && third.ends_with("1700000000000-2"));
    }

    #[test]
    fn schemas_are_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        let make = |name: &str, tables: &[&str]| {
            let path = dir.path().join(name);
            let conn = Connection::open(&path).unwrap();
            for t in tables {
                conn.execute(&format!("CREATE TABLE {t} (id TEXT)"), []).unwrap();
            }
            path
        };
        assert_eq!(detect_schema(&store().db), Schema::V2);
        assert_eq!(detect_schema(&make("v1.db", &["session", "message", "part", "project"])), Schema::V1);
        // 1.18 already has a `session_message` table beside the 1.x ones.
        assert_eq!(detect_schema(&make("v118.db", &["session", "message", "part", "session_message"])), Schema::V1);
        assert_eq!(detect_schema(&make("both.db", &["session", "message", "part", "session_v2", "session_message"])), Schema::V2);
        assert_eq!(detect_schema(&make("half.db", &["session_v2"])), Schema::Unsupported);
        assert_eq!(detect_schema(&make("empty.db", &[])), Schema::Unsupported);
        assert_eq!(detect_schema(&dir.path().join("missing.db")), Schema::Absent, "no store yet: not a failure to look");
        let garbage = dir.path().join("garbage.db");
        std::fs::write(&garbage, b"not a database at all, just text").unwrap();
        assert_eq!(detect_schema(&garbage), Schema::Unknown, "cannot read it: unknown, never assumed 1.x");
    }

    /// The probes of the review: what a 1.x write does to a 2.x store, and a
    /// store that cannot be inspected.
    #[test]
    fn a_store_that_cannot_be_inspected_is_never_written_with_the_one_x_sql() {
        let store = store();
        let (cli, log) = fake_cli(&store);
        let adapter = store.adapter().with_cli(cli).with_backup_root(store._dir.path().join("backups"));
        let session = find(&adapter.sessions(&SessionFilter::default()).unwrap(), "ses_plain").clone();
        let messages = || store.conn.query_row("SELECT COUNT(*) FROM session_message WHERE session_id='ses_plain'", [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(messages(), 2);

        // Locked for the moment: another connection holds the file exclusively.
        let blocker = Connection::open(&store.db).unwrap();
        blocker.execute_batch("PRAGMA locking_mode = EXCLUSIVE; BEGIN EXCLUSIVE;").unwrap();
        assert_eq!(adapter.schema(), Schema::Unknown);
        assert!(!adapter.is_v2(), "reads fall back to 1.x as before");
        let say = |r: Result<(), crate::CoreError>| r.unwrap_err().to_string();
        let refusal = "could not be inspected";
        assert!(say(adapter.rename(&session, "x")).contains(refusal));
        assert!(say(adapter.delete(&session).map(drop)).contains(refusal));
        assert!(say(adapter.archive(&session).map(drop)).contains(refusal));
        assert!(say(adapter.unarchive(&session)).contains(refusal));
        assert!(say(adapter.relocate(&session, ROOT.as_ref()).map(drop)).contains(refusal));
        let opts = crate::import::ImportOpts { mode: crate::import::ImportMode::Full, project: None, dry_run: false };
        let ir = adapter_ir();
        assert!(say(adapter.import_ir(&ir, &opts).map(drop)).contains(refusal));
        let bundle = hub::collect(&adapter, &session);
        assert!(bundle.err().unwrap().to_string().contains(refusal), "a push would otherwise carry empty 1.x queries");
        assert_eq!(hub::fingerprint(&adapter, &session), None, "no fingerprint for a store nobody can read");
        assert!(crate::live::AgentLive::send_command(&adapter, &session, "hi").is_none(), "and `run` is not started blind");
        let caps = adapter.capabilities();
        assert!(!caps.send_message && !caps.archive, "only a store known to be 1.x offers them");
        assert!(!log.exists(), "opencode never started");
        blocker.execute_batch("ROLLBACK").unwrap();
        drop(blocker);
        assert_eq!(messages(), 2, "nothing was touched");
        assert_eq!(adapter.schema(), Schema::V2);

        // The 1.x path itself will not touch a 2.x store, whoever calls it.
        let one_x = super::super::write::delete(&adapter, &session);
        assert!(one_x.unwrap_err().to_string().contains("2.x store"));
        assert_eq!(messages(), 2, "the review's probe: session_message rows survive a 1.x delete");
        assert!(super::super::write::rename(&adapter, &session, "x").is_err());
        // ...and never lists `session_message` among a 2.x store's 1.x tables.
        assert!(!super::super::write::session_tables(&store.conn).contains(&"session_message"));
    }

    fn adapter_ir() -> crate::ir::IrSession {
        crate::ir::IrSession {
            ir_version: 1,
            source: crate::ir::IrProvenance {
                agent: crate::model::AgentKind::ClaudeCode,
                native_id: "x".into(),
                agent_version: None,
                exported_at: jiff::Timestamp::UNIX_EPOCH,
                exporter_version: "t".into(),
            },
            title: None,
            slug: None,
            project_path: crate::ir::PortablePath(".".into()),
            created: None,
            updated: None,
            model: None,
            usage: Default::default(),
            messages: vec![],
            extensions: crate::ir::ExtBag::new(),
        }
    }

    /// 2.x migrates its 1.x sessions in the background and keeps the old
    /// tables. While some are not copied, writes are refused and the user is told.
    #[test]
    fn a_store_still_migrating_is_not_written_and_says_so() {
        let store = store();
        store.conn.execute_batch(V1_DDL).unwrap();
        let v1 = |id: &str| {
            store
                .conn
                .execute("INSERT INTO session VALUES (?1,'prj_1',NULL,'s','/w','t','1.18',NULL,NULL,0,0,0,0,0,10,10,NULL)", [id])
                .unwrap()
        };
        let adapter = store.adapter();
        assert_eq!(adapter.store_note(), None, "no 1.x tables to migrate");
        v1("ses_plain"); // already migrated (it is in session_v2)
        assert_eq!(super::super::unmigrated(&store.db).unwrap(), 0);
        assert_eq!(adapter.store_note(), None);
        v1("ses_only_in_one_x");
        v1("ses_another");
        assert_eq!(super::super::unmigrated(&store.db).unwrap(), 2);
        let note = adapter.store_note().unwrap();
        assert!(
            note.starts_with("2 sessions in this OpenCode database have not been migrated by OpenCode 2.x yet; start OpenCode once"),
            "{note}"
        );

        let (cli, log) = fake_cli(&store);
        let adapter = store.adapter().with_cli(cli).with_backup_root(store._dir.path().join("backups"));
        let session = find(&adapter.sessions(&SessionFilter::default()).unwrap(), "ses_plain").clone();
        let refusal = "have not been migrated by OpenCode 2.x yet";
        assert!(adapter.rename(&session, "x").unwrap_err().to_string().contains(refusal));
        assert!(adapter.delete(&session).unwrap_err().to_string().contains(refusal));
        let opts = crate::import::ImportOpts { mode: crate::import::ImportMode::Full, project: None, dry_run: false };
        assert!(adapter.import_ir(&adapter_ir(), &opts).unwrap_err().to_string().contains(refusal));
        let manifest: crate::hub::manifest::Manifest = serde_json::from_value(json!({
            "schema": 1, "agent": "opencode", "id": "ses_plain", "project_root": ROOT, "project_root_portable": ROOT,
            "canonical": "x", "parent_rev": null, "files": [], "extra": { "generation": "opencode-v2" },
        }))
        .unwrap();
        let installed = hub::install(&adapter, &manifest, &|_| unreachable!(), None, None);
        assert!(installed.unwrap_err().to_string().contains(refusal));
        assert!(!log.exists(), "opencode never started");
        let messages: i64 = store.conn.query_row("SELECT COUNT(*) FROM session_message", [], |r| r.get(0)).unwrap();
        assert_eq!(messages, 13, "nothing was touched");

        // Once OpenCode has copied them, writes are allowed again.
        store.conn.execute("DELETE FROM session WHERE id IN ('ses_only_in_one_x', 'ses_another')", []).unwrap();
        assert_eq!(adapter.store_note(), None);
        assert!(adapter.write_schema().is_ok());
    }

    /// The sessions only the 1.x tables hold are listed, read-only, beside the
    /// migrated ones: nothing is hidden while OpenCode migrates.
    #[test]
    fn sessions_not_migrated_yet_are_listed_and_read_from_the_one_x_tables() {
        let store = store();
        store.conn.execute_batch(V1_DDL).unwrap();
        let v1 = |id: &str, parent: Option<&str>, title: &str, updated: i64| {
            store
                .conn
                .execute(
                    "INSERT INTO session VALUES (?1,'prj_1',?2,'s',?3,?4,'1.18.31',NULL,NULL,0,1,2,0,0,10,?5,NULL)",
                    rusqlite::params![id, parent, ROOT, title, updated],
                )
                .unwrap()
        };
        v1("ses_plain", None, "Stale 1.x copy of a migrated session", 1); // migrated: session_v2 wins
        v1("ses_old_one", None, "Only in 1.x", 1_900_000_000_000);
        v1("ses_old_child", Some("ses_old_one"), "Its subagent", 1_900_000_000_001);
        store.conn.execute("INSERT INTO message VALUES ('m1','ses_old_one',1,1,'{\"role\":\"user\"}')", []).unwrap();
        store
            .conn
            .execute("INSERT INTO part VALUES ('p1','m1','ses_old_one',1,1,'{\"type\":\"text\",\"text\":\"from the old tables\"}')", [])
            .unwrap();
        let adapter = store.adapter();
        let listed = adapter.sessions(&SessionFilter::default()).unwrap();
        assert_eq!(listed.iter().filter(|s| s.handle.native_id == "ses_plain").count(), 1, "never twice");
        assert_eq!(find(&listed, "ses_plain").title.as_deref(), Some("Plain exchange"), "the 2.x row wins");
        let old = find(&listed, "ses_old_one");
        assert_eq!(old.title.as_deref(), Some("Only in 1.x"));
        assert!(matches!(&old.handle.location, SessionLocation::SqliteRow { table, .. } if table == "session"));
        assert!(old.size_bytes.unwrap() > 0, "sized from message and part");
        assert!(listed.iter().all(|s| s.handle.native_id != "ses_old_child"), "children stay hidden by default");
        let all = adapter.sessions(&SessionFilter { include_children: true, ..SessionFilter::default() }).unwrap();
        assert_eq!(find(&all, "ses_old_child").parent.as_deref(), Some("ses_old_one"));
        assert_eq!(listed[0].handle.native_id, "ses_old_one", "newest first across both");
        // Read as 1.x: the transcript comes from message/part.
        let ir = adapter.export_ir(old).unwrap();
        assert!(matches!(&ir.messages[0].parts[..], [IrPart::Text { text }] if text == "from the old tables"));
        // Not pushable (a 2.x machine cannot take 1.x rows) and not indexed as 2.x.
        assert_eq!(hub::fingerprint(&adapter, old), None);
        assert!(hub::collect(&adapter, old).err().unwrap().to_string().contains("has not been migrated"));
        let before = crate::index::fingerprint(old);
        store.conn.execute("INSERT INTO message VALUES ('m2','ses_old_one',1,5000,'{\"role\":\"assistant\"}')", []).unwrap();
        assert_ne!(before, crate::index::fingerprint(old), "the index follows the 1.x tables for it");
        // ...and none of it can be changed.
        assert!(adapter.rename(old, "x").unwrap_err().to_string().contains("have not been migrated"));
    }

    const V1_DDL: &str = "CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT, slug TEXT, directory TEXT,
                    title TEXT, version TEXT, agent TEXT, model TEXT, cost REAL, tokens_input INTEGER,
                    tokens_output INTEGER, tokens_cache_read INTEGER, tokens_cache_write INTEGER,
                    time_created INTEGER, time_updated INTEGER, time_archived INTEGER);
                 CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
                 CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);";

    /// The index fingerprint follows the schema: a migrated database keeps its
    /// old `message` table, frozen, and the new messages are in `session_message`.
    #[test]
    fn the_index_fingerprint_follows_session_message_on_a_migrated_store() {
        let store = store();
        store.conn.execute_batch(V1_DDL).unwrap();
        let session = find(&store.adapter().sessions(&SessionFilter::default()).unwrap(), "ses_plain").clone();
        let before = crate::index::fingerprint(&session);
        // A tool result streamed into the row: session_message moves while
        // session_v2.time_updated lags.
        store.conn.execute("UPDATE session_message SET time_updated = time_updated + 5000 WHERE id = 'msg_2'", []).unwrap();
        let after = crate::index::fingerprint(&session);
        assert_ne!(before, after, "the fingerprint must move when session_message does");
        store.message("ses_plain", 3, "msg_3", "user", json!({"text": "more", "time": {"created": 3}}));
        assert_ne!(after, crate::index::fingerprint(&session));
    }

    /// A failed query must not look like "no sessions": the daemon skips the
    /// pass instead of pushing a changed (empty) session.
    #[test]
    fn a_fingerprint_that_cannot_be_read_is_none_not_empty() {
        let store = store();
        let adapter = store.adapter();
        let session = find(&adapter.sessions(&SessionFilter::default()).unwrap(), "ses_plain").clone();
        assert!(hub::fingerprint(&adapter, &session).is_some());
        let mut missing = session.clone();
        missing.handle.native_id = "ses_nope".into();
        assert_eq!(hub::fingerprint(&adapter, &missing), None, "not a session in this store");
        store.conn.execute_batch("ALTER TABLE session_message RENAME TO session_message_gone").unwrap();
        assert_eq!(adapter.schema(), Schema::Unsupported);
        store.conn.execute_batch("ALTER TABLE session_message_gone RENAME TO session_message").unwrap();
        assert!(hub::fingerprint(&adapter, &session).is_some());
    }

    /// A script standing in for `opencode`: it logs its arguments and runs
    /// `body` for the commands it is given.
    fn script(store: &Store, version: &str, body: &str) -> (super::super::cli2::Cli, std::path::PathBuf) {
        let dir = store._dir.path();
        let log = dir.join("calls.log");
        let path = dir.join("opencode-script");
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho \"$*\" >> {}\n[ \"$1\" = --version ] && echo 'opencode v{version}'\n{body}\nexit 0\n", log.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (super::super::cli2::Cli::isolated(path, vec![], dir.join("state"), dir), log)
    }

    /// After a failed import, what was left of it is removed if the STORE has
    /// it, whatever the CLI said: a timeout after the server committed the
    /// root left it behind and made the restore fail with "here already".
    #[test]
    fn a_half_imported_tree_is_cleaned_up_by_what_the_store_holds() {
        let store = store();
        let dir = store._dir.path();
        let (empty, committed) = (dir.join("empty.db"), dir.join("committed.db"));
        std::fs::remove_file(&store.db).ok();
        {
            let conn = Connection::open(&empty).unwrap();
            conn.execute_batch(DDL).unwrap();
            conn.execute("INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) VALUES ('prj_1', '/', '[]', 1, 1)", []).unwrap();
            std::fs::copy(&empty, &committed).unwrap();
            Connection::open(&committed)
                .unwrap()
                .execute("INSERT INTO session_v2 (id, project_id, slug, directory, version, time_created, time_updated) VALUES ('ses_r', 'prj_1', 's', '/', '2.0.25', 1, 1)", [])
                .unwrap();
        }
        std::fs::copy(&empty, &store.db).unwrap();
        // The server commits the root and then the CLI fails (a timeout, a kill).
        let (cli, log) = script(
            &store,
            "2.0.25",
            &format!(
                "case \"$1 $2\" in\n\"session import\") cp {} {}; exit 1;;\n\"session delete\") cp {} {};;\nesac",
                committed.display(), store.db.display(), empty.display(), store.db.display()
            ),
        );
        let adapter = store.adapter().with_cli(cli);
        let tree = [json!({ "info": { "id": "ses_r", "title": "T", "cost": 0, "location": { "directory": "/" } }, "messages": [] })];
        let err = super::super::hub_v2::import_tree(&adapter, &tree, std::path::Path::new("/")).unwrap_err().to_string();
        assert!(err.contains("opencode session import failed"), "{err}");
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains("session delete ses_r --standalone"), "the root the server committed was removed: {calls}");
        let count = || Connection::open(&store.db).unwrap().query_row("SELECT COUNT(*) FROM session_v2", [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(count(), 0);

        // A root that was here before is not ours to remove.
        std::fs::copy(&committed, &store.db).unwrap();
        std::fs::remove_file(&log).unwrap();
        let err = super::super::hub_v2::import_tree(&adapter, &tree, std::path::Path::new("/")).unwrap_err().to_string();
        assert!(err.contains("session ses_r is here already"), "{err}");
        assert!(!log.exists(), "nothing was run, and nothing deleted");
        assert_eq!(count(), 1);
    }

    /// No database yet: the installed binary decides what will create it,
    /// and it is never run with no flag. The CLI runs in asm's scratch
    /// directory, so the project directory it is given is absolute.
    #[test]
    fn an_import_with_no_store_yet_follows_the_binary_and_hands_over_an_absolute_directory() {
        let store = store();
        std::fs::remove_file(&store.db).unwrap();
        let (cli, log) = script(&store, "2.0.25", "");
        let adapter = store.adapter().with_cli(cli).with_backup_root(store._dir.path().join("backups"));
        assert_eq!(adapter.schema(), Schema::Absent);
        let opts = crate::import::ImportOpts { mode: crate::import::ImportMode::Full, project: None, dry_run: false };
        // A relative project directory means the test's working directory, which is not the CLI's.
        let mut ir = adapter_ir();
        ir.project_path = crate::ir::PortablePath(".".into());
        let err = adapter.import_ir(&ir, &opts);
        assert!(err.is_err(), "the script creates no store, so the import is not confirmed");
        let calls = std::fs::read_to_string(&log).unwrap();
        let import = calls.lines().find(|l| l.starts_with("session import")).unwrap_or_else(|| panic!("no import was run: {calls}"));
        let directory = import.split(" --directory ").nth(1).unwrap().split(' ').next().unwrap();
        assert!(std::path::Path::new(directory).is_absolute(), "{import}");
        assert_eq!(std::fs::canonicalize(directory).unwrap(), std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap(), "{import}");
        assert!(import.ends_with("--standalone"), "{import}");
        for call in calls.lines() {
            assert!(call == "--version" || call.ends_with("--standalone"), "a flagless command was run: {call}");
        }
    }

    /// A repeat import that finds the session there says so; when the source
    /// has grown since, it says that too.
    #[test]
    fn a_repeat_import_says_when_the_source_grew() {
        let store = store();
        let ir = adapter_ir();
        let id = crate::import::idempotency::opencode_session_id(ir.source.agent.as_str(), &ir.source.native_id);
        store.session(&id, None, Some("Imported"), 1_800_000_000_000, None, None);
        store.message(&id, 1, "msg_imp_1", "user", json!({"text": "hello", "time": {"created": 1}}));
        let adapter = store.adapter();
        let opts = crate::import::ImportOpts { mode: crate::import::ImportMode::Full, project: None, dry_run: false };
        // The source has no messages now: the idle marker alone is 1, as stored.
        let done = adapter.import_ir(&ir, &opts).unwrap();
        assert!(done.in_sync && done.warnings.is_empty(), "{:?}", done.warnings);
        let mut grown = ir.clone();
        let message = |text: &str| crate::ir::IrMessage {
            role: crate::ir::IrRole::User,
            timestamp: None,
            parts: vec![crate::ir::IrPart::Text { text: text.into() }],
            source_id: Some(text.into()),
            extensions: crate::ir::ExtBag::new(),
        };
        grown.messages = vec![message("one"), message("two")];
        let done = adapter.import_ir(&grown, &opts).unwrap();
        assert!(done.in_sync);
        assert!(
            done.warnings.iter().any(|w| w.contains("imported before, from 1 messages; the source has 3 now") && w.contains("is not updated")),
            "{:?}",
            done.warnings
        );
    }

    /// A 1.x database is read by the 1.x code and keeps its full set of
    /// capabilities; a 2.x one never reaches the 1.x tables.
    #[test]
    fn a_one_x_database_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT);
                 CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT, slug TEXT, directory TEXT,
                    title TEXT, version TEXT, agent TEXT, model TEXT, cost REAL, tokens_input INTEGER,
                    tokens_output INTEGER, tokens_cache_read INTEGER, tokens_cache_write INTEGER,
                    time_created INTEGER, time_updated INTEGER, time_archived INTEGER);
                 CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
                 CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
                 INSERT INTO project VALUES ('p', '/w');
                 INSERT INTO session VALUES ('ses_1', 'p', NULL, 's', '/w', 'One', '1.17.18', NULL, NULL, NULL, NULL, NULL, NULL, NULL, 1, 2, NULL);
                 INSERT INTO message VALUES ('m1', 'ses_1', 1, 1, '{\"role\":\"user\"}');
                 INSERT INTO part VALUES ('p1', 'm1', 'ses_1', 1, 1, '{\"type\":\"text\",\"text\":\"hello\"}');",
            )
            .unwrap();
        let adapter = OpenCodeAdapter::with_db(&db);
        assert!(!adapter.is_v2());
        let sessions = adapter.sessions(&SessionFilter::default()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(matches!(&sessions[0].handle.location, SessionLocation::SqliteRow { table, .. } if table == "session"));
        let ir = adapter.export_ir(&sessions[0]).unwrap();
        assert!(matches!(&ir.messages[0].parts[..], [IrPart::Text { text }] if text == "hello"));
        let caps = adapter.capabilities();
        assert!(caps.rename && caps.archive && caps.delete && caps.import_ir && caps.send_message);
        assert!(OpenCodeAdapter::with_db("").capabilities().rename, "hub planning sees the full set");
    }
}
