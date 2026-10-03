//! OpenCode message/part rows → Session IR.

use jiff::Timestamp;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

use crate::CoreError;
use crate::ir::{ExtBag, IR_VERSION, IrMessage, IrPart, IrProvenance, IrRole, IrSession, PortablePath};
use crate::model::Session;

use super::OpenCodeAdapter;

pub(super) fn export_ir(
    adapter: &OpenCodeAdapter,
    session: &Session,
) -> Result<IrSession, CoreError> {
    let conn = Connection::open_with_flags(
        adapter.db(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| CoreError::Sqlite { db: adapter.db().to_path_buf(), source: Box::new(e) })?;
    let sql_err =
        |e: rusqlite::Error| CoreError::Sqlite { db: adapter.db().to_path_buf(), source: Box::new(e) };

    let mut messages_stmt = conn
        .prepare(
            "SELECT id, time_created, data FROM message
             WHERE session_id = ?1 ORDER BY time_created, id",
        )
        .map_err(sql_err)?;
    let mut parts_stmt = conn
        .prepare("SELECT data FROM part WHERE message_id = ?1 ORDER BY id")
        .map_err(sql_err)?;
    let rows = messages_stmt
        .query_map([&session.handle.native_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(sql_err)?;
    let mut raw = Vec::new();
    for row in rows {
        let (message_id, time_created, data_json) = row.map_err(sql_err)?;
        let part_rows = parts_stmt
            .query_map([&message_id], |row| row.get::<_, Option<String>>(0))
            .map_err(sql_err)?;
        let mut parts = Vec::new();
        for part_json in part_rows {
            parts.push(part_json.map_err(sql_err)?);
        }
        raw.push((message_id, time_created, data_json, parts));
    }
    Ok(build(session, raw))
}

/// A message row as stored: id, time_created, data JSON, and its parts' data JSON in order.
type RawMessage = (String, Option<i64>, Option<String>, Vec<Option<String>>);

/// The IR of a session's root from its row and its tree's dump as bundled
/// by a push (`rows.json`), without a database or an `opencode` process.
pub(crate) fn from_dump(id: &str, dump: &Value) -> IrSession {
    use std::path::PathBuf;
    let rows = |table: &str| dump.get(table).and_then(Value::as_array).cloned().unwrap_or_default();
    let session_row = rows("session").into_iter().next().unwrap_or(Value::Null);
    let text = |k: &str| session_row.get(k).and_then(Value::as_str).map(str::to_string);
    let int = |row: &Value, k: &str| row.get(k).and_then(Value::as_i64);
    let ts = |k: &str| int(&session_row, k).and_then(|ms| Timestamp::from_millisecond(ms).ok());
    let session = Session {
        handle: crate::model::SessionRef {
            agent: crate::model::AgentKind::OpenCode,
            native_id: id.to_string(),
            location: crate::model::SessionLocation::SqliteRow { db: PathBuf::new(), table: "session".into() },
        },
        title: text("title"),
        slug: text("slug"),
        project_root: PathBuf::from(text("directory").unwrap_or_default()),
        git_branch: None,
        created: ts("time_created"),
        updated: ts("time_updated"),
        model: text("model")
            .and_then(|m| serde_json::from_str::<Value>(&m).ok())
            .and_then(|m| m.get("id").and_then(Value::as_str).map(str::to_string)),
        usage: Default::default(),
        status: crate::model::SessionStatus::Idle,
        parent: None,
        agent_version: text("version"),
        size_bytes: None,
    };
    let mut messages: Vec<Value> = rows("message");
    messages.sort_by_key(|m| (int(m, "time_created"), m["id"].as_str().map(str::to_string)));
    let parts = rows("part");
    let raw = messages
        .iter()
        .map(|m| {
            let mid = m["id"].as_str().unwrap_or_default().to_string();
            let mut mine: Vec<&Value> = parts.iter().filter(|p| p["message_id"].as_str() == Some(&mid)).collect();
            mine.sort_by_key(|p| p["id"].as_str().map(str::to_string));
            let data = |v: &Value| v.get("data").and_then(Value::as_str).map(str::to_string);
            (mid, int(m, "time_created"), data(m), mine.into_iter().map(data).collect())
        })
        .collect();
    build(&session, raw)
}

fn build(session: &Session, raw: Vec<RawMessage>) -> IrSession {
    let mut messages = Vec::new();
    for (message_id, time_created, data_json, part_rows) in raw {
        let data: Value = data_json
            .as_deref()
            .and_then(|d| serde_json::from_str(d).ok())
            .unwrap_or(Value::Null);

        let role = match data.get("role").and_then(Value::as_str) {
            Some("assistant") => IrRole::Assistant,
            Some("user") => IrRole::User,
            _ => IrRole::System,
        };
        let timestamp = data
            .get("time")
            .and_then(|t| t.get("created"))
            .and_then(Value::as_i64)
            .or(time_created)
            .and_then(|ms| Timestamp::from_millisecond(ms).ok());

        let mut parts = Vec::new();
        let mut raw_extras = Vec::new();
        for part_json in part_rows {
            let Some(part) = part_json.as_deref().and_then(|p| serde_json::from_str::<Value>(p).ok())
            else {
                continue;
            };
            match convert_part(&part) {
                Converted::Parts(mut converted) => parts.append(&mut converted),
                Converted::Raw => raw_extras.push(part),
            }
        }

        let mut extensions = ExtBag::new();
        if !raw_extras.is_empty() {
            extensions.insert("opencode".into(), json!({ "raw_parts": raw_extras }));
        }
        if parts.is_empty() && extensions.is_empty() {
            continue;
        }
        messages.push(IrMessage { role, timestamp, parts, source_id: Some(message_id), extensions });
    }

    IrSession {
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
        extensions: ExtBag::new(),
    }
}

enum Converted {
    Parts(Vec<IrPart>),
    /// Not modeled by the IR — preserved raw in the message extensions.
    Raw,
}

fn convert_part(part: &Value) -> Converted {
    match part.get("type").and_then(Value::as_str) {
        Some("text") => Converted::Parts(
            part.get("text")
                .and_then(Value::as_str)
                .map(|t| vec![IrPart::Text { text: t.to_string() }])
                .unwrap_or_default(),
        ),
        Some("reasoning") => {
            let summary =
                part.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
            // Encrypted/signed provider payloads live under metadata
            // (e.g. openai.reasoningEncryptedContent, anthropic signatures).
            let metadata = part.get("metadata").map(|m| m.to_string()).unwrap_or_default();
            let opaque =
                metadata.contains("ncryptedContent") || metadata.contains("signature");
            Converted::Parts(vec![IrPart::Reasoning { summary, opaque }])
        }
        Some("tool") => {
            let call_id =
                part.get("callID").and_then(Value::as_str).unwrap_or_default().to_string();
            let name =
                part.get("tool").and_then(Value::as_str).unwrap_or_default().to_string();
            let state = part.get("state").cloned().unwrap_or(Value::Null);
            let input = state.get("input").cloned().unwrap_or(Value::Null);
            let mut parts = vec![IrPart::ToolCall { call_id: call_id.clone(), name, input }];
            // The error member of the tool-state union carries its message
            // in `error`, not `output` — reading only `output` silently
            // drops every failed tool call's result.
            let is_error = state.get("status").and_then(Value::as_str) == Some("error");
            let result = if is_error {
                state.get("error").and_then(Value::as_str)
            } else {
                state.get("output").and_then(Value::as_str)
            };
            if let Some(output) = result {
                parts.push(IrPart::ToolResult {
                    call_id,
                    output: output.to_string(),
                    is_error,
                    truncated: state
                        .get("metadata")
                        .and_then(|m| m.get("truncated"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                });
            }
            Converted::Parts(parts)
        }
        Some("file") => Converted::Parts(vec![IrPart::File {
            path: part
                .get("url")
                .and_then(Value::as_str)
                .and_then(|u| u.strip_prefix("file://"))
                .map(|p| PortablePath::from_path(std::path::Path::new(p))),
            mime: part.get("mime").and_then(Value::as_str).map(str::to_string),
            content: None,
        }]),
        // step-start/step-finish/patch/snapshot/compaction/retry/agent/
        // subtask and anything newer: keep raw for round-trips.
        _ => Converted::Raw,
    }
}
