//! IR → OpenCode 2.x, through the sanctioned path: build the document
//! `opencode session export` prints (`SessionTransfer.Data`) and feed it to
//! `opencode session import --directory <project>`.
//!
//! What the 2.0.25 import does with it (verified by running it): it keeps
//! our ids, so the deterministic ids of `import::idempotency` make a second
//! import a no-op; it validates the document against the schema (a
//! completed tool needs non-empty `content`, an errored one an
//! `error{type,message}`, an assistant its `agent`, `model` and
//! `time.completed`), renumbers the messages 1..n, and computes the
//! project from the directory it is given.
//!
//! Faithful enough to read and to continue from: text, reasoning text, tool
//! calls with their results (names mapped into 2.x's vocabulary) and the
//! turn order come across. Opaque reasoning, attachments (2.x holds file
//! bytes inline, which an IR does not carry) and nested subagent runs do
//! not, and the loss report says so.

use serde_json::{Value, json};

use crate::CoreError;
use crate::import::{ImportOpts, ImportOutcome, LossReport, idempotency, toolmap};
use crate::ir::{IrPart, IrRole, IrSession};
use crate::model::{AgentKind, SessionLocation, SessionRef};

use super::{OpenCodeAdapter, v2};

pub(super) fn import_ir(adapter: &OpenCodeAdapter, ir: &IrSession, opts: &ImportOpts) -> Result<ImportOutcome, CoreError> {
    let session_id = idempotency::opencode_session_id(ir.source.agent.as_str(), &ir.source.native_id);
    let target_ref = SessionRef {
        agent: AgentKind::OpenCode,
        native_id: session_id.clone(),
        location: SessionLocation::SqliteRow { db: adapter.db().to_path_buf(), table: "session_v2".to_string() },
    };
    let resume_hint = Some(format!("opencode -s {session_id}   (run in the project dir)"));
    let outcome = |in_sync, artifact, loss| ImportOutcome {
        mode: opts.mode,
        target: Some(target_ref.clone()),
        in_sync,
        dry_run_artifact: artifact,
        resume_hint: resume_hint.clone(),
        warnings: vec![],
        loss,
    };
    let sql = |e: rusqlite::Error| CoreError::Sqlite { db: adapter.db().to_path_buf(), source: Box::new(e) };

    let mut loss = LossReport::default();
    // Resume continues with the session's model, so the messages must name
    // one this install can serve.
    let model = preferred_model(adapter);
    if let Some((provider, model_id)) = &model {
        loss.notes.push(format!(
            "conversation re-attributed to {provider}/{model_id} (the target install's last-used model) so resume \
             works; the source model ({}) is recorded in the provenance",
            ir.model.as_deref().unwrap_or("unknown")
        ));
    }
    // Absolute: the CLI runs with asm's scratch directory as its cwd, so a
    // relative path would be read from there.
    let project_dir = ir.project_path.resolve();
    let project_dir = std::path::absolute(&project_dir).unwrap_or(project_dir);
    let document = build_document(ir, &session_id, &project_dir, model, &mut loss);
    let wanted = document["messages"].as_array().map_or(0, Vec::len) as i64;
    let pretty = serde_json::to_string_pretty(&document).unwrap();
    if opts.dry_run {
        return Ok(outcome(false, Some(pretty), loss));
    }

    // Imported before: the id is there with its messages. (A bare row is
    // not enough; the import is atomic on 2.x, but be as strict as 1.x.)
    if adapter.db().is_file() {
        let conn = super::super::open_ro(adapter.db())?;
        let there = v2::info_of(&conn, &session_id).map_err(sql)?.is_some();
        let messages: i64 = conn
            .query_row("SELECT COUNT(*) FROM session_message WHERE session_id = ?1", [&session_id], |r| r.get(0))
            .map_err(sql)?;
        if there && messages > 0 {
            let mut done = outcome(true, None, LossReport::default());
            if messages != wanted {
                // The same ids are deterministic, so a session that grew
                // since is "imported" and not brought up to date.
                done.warnings.push(format!(
                    "this session was imported before, from {messages} messages; the source has {wanted} now. An imported \
                     copy is not updated: delete {session_id} in OpenCode first to import it again"
                ));
            }
            return Ok(done);
        }
        if there {
            return Err(CoreError::Invalid {
                msg: format!("session {session_id} is in the store without messages; delete it and import again"),
            });
        }
    }

    if !project_dir.is_dir() {
        return Err(CoreError::Invalid {
            msg: format!("target project directory {} does not exist (opencode import binds the session to it)", project_dir.display()),
        });
    }
    let cli = adapter.cli();
    let file = cli.scratch()?.join(format!("{session_id}.import.json"));
    std::fs::write(&file, &pretty).map_err(|e| CoreError::io(&file, e))?;
    let ran = cli.import(&file, &project_dir);
    let _ = std::fs::remove_file(&file);
    ran?;
    // The CLI answers success for an id that exists; the store is the test.
    let conn = super::super::open_ro(adapter.db())?;
    let stored: i64 = conn
        .query_row("SELECT COUNT(*) FROM session_message WHERE session_id = ?1", [&session_id], |r| r.get(0))
        .map_err(sql)?;
    if v2::info_of(&conn, &session_id).map_err(sql)?.is_none() || stored != wanted {
        return Err(CoreError::Invalid {
            msg: format!("opencode import did not complete ({stored} of {wanted} messages of {session_id} are in the store)"),
        });
    }
    Ok(outcome(false, None, loss))
}

/// The (provider, model) of the session last used here: the one model this
/// install is known to serve.
fn preferred_model(adapter: &OpenCodeAdapter) -> Option<(String, String)> {
    if !adapter.db().is_file() {
        return None;
    }
    let conn = super::super::open_ro(adapter.db()).ok()?;
    let model: String = conn
        .query_row(
            "SELECT model FROM session_v2 WHERE model IS NOT NULL AND model != '' ORDER BY time_updated DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok()?;
    let value: Value = serde_json::from_str(&model).ok()?;
    Some((value.get("providerID")?.as_str()?.to_string(), value.get("id")?.as_str()?.to_string()))
}

/// 1.x names (what `toolmap` produces for OpenCode) into 2.x's: it has
/// `shell` for `bash` and `subagent` for `task`; `todowrite` and the rest
/// stay as they are, inert but readable history.
fn tool_name(name: &str, loss: &mut LossReport) -> String {
    let mapped = toolmap::map_tool(name, AgentKind::OpenCode);
    if mapped.is_some() {
        loss.tools_mapped += 1;
    }
    let name_here = match mapped.unwrap_or(name) {
        "bash" => "shell",
        "task" => "subagent",
        other => other,
    };
    if !V2_TOOLS.contains(&name_here) && name_here != "subagent" {
        *loss.tools_kept_unmapped.entry(name.to_string()).or_default() += 1;
    }
    name_here.to_string()
}

const V2_TOOLS: [&str; 11] = ["shell", "read", "edit", "write", "glob", "grep", "webfetch", "websearch", "question", "skill", "patch"];

/// A tool's `input` must be an object.
fn input_object(input: &Value) -> Value {
    match input {
        Value::Object(_) => input.clone(),
        Value::Null => json!({}),
        other => json!({ "value": other }),
    }
}

/// The `{info, messages}` document. `model` overrides the (provider, model)
/// every assistant message is attributed to.
pub(crate) fn build_document(
    ir: &IrSession,
    session_id: &str,
    project_dir: &std::path::Path,
    model: Option<(String, String)>,
    loss: &mut LossReport,
) -> Value {
    use std::collections::HashMap;
    loss.messages_in = ir.messages.len();
    let mut results: HashMap<&str, (&str, bool)> = HashMap::new();
    for message in &ir.messages {
        for part in &message.parts {
            if let IrPart::ToolResult { call_id, output, is_error, .. } = part {
                results.insert(call_id.as_str(), (output.as_str(), *is_error));
            }
        }
    }
    let now = jiff::Timestamp::now().as_millisecond();
    let ms = |ts: Option<jiff::Timestamp>| ts.map(|t| t.as_millisecond()).unwrap_or(now);
    let (provider_id, model_id) = model.unwrap_or_else(|| {
        let provider = if ir.source.agent == AgentKind::ClaudeCode { "anthropic" } else { "unknown" };
        (provider.to_string(), ir.model.clone().unwrap_or_else(|| "imported".to_string()))
    });

    let mut messages = Vec::new();
    let mut last = ms(ir.created);
    for (index, message) in ir.messages.iter().enumerate() {
        let source_id = message.source_id.clone().unwrap_or_else(|| format!("idx:{index}"));
        let id = idempotency::opencode_message_id(session_id, index, &source_id);
        let created = ms(message.timestamp);
        let mut texts = Vec::new();
        let mut content = Vec::new();
        for part in &message.parts {
            match part {
                IrPart::Text { text } => {
                    texts.push(text.as_str());
                    content.push(json!({ "type": "text", "text": text }));
                }
                IrPart::Reasoning { summary, opaque } => {
                    if *opaque {
                        loss.reasoning_dropped += 1;
                    }
                    if !summary.is_empty() {
                        content.push(json!({ "type": "reasoning", "text": summary,
                                              "time": { "created": created, "completed": created } }));
                    }
                }
                IrPart::ToolCall { call_id, name, input } => {
                    let state = match results.get(call_id.as_str()) {
                        Some((output, false)) => json!({ "status": "completed", "input": input_object(input),
                                                          "content": [{ "type": "text", "text": output }] }),
                        Some((output, true)) => json!({ "status": "error", "input": input_object(input),
                                                         "error": { "type": "tool", "message": output } }),
                        None => json!({ "status": "error", "input": input_object(input),
                                         "error": { "type": "interrupted", "message": "No result was recorded for this call." } }),
                    };
                    content.push(json!({ "type": "tool", "id": call_id, "name": tool_name(name, loss),
                                          "state": state, "time": { "created": created, "completed": created } }));
                }
                IrPart::ToolResult { .. } => {}
                IrPart::File { .. } => {
                    loss.notes.push("a file attachment was left out (2.x stores attachments inline)".into());
                }
                IrPart::Agent { transcript, .. } => {
                    loss.agent_transcripts_dropped += 1;
                    loss.notes.push(format!("a subagent run ({} turns) was flattened out", transcript.len()));
                }
                IrPart::Unknown => {}
            }
        }
        let value = match message.role {
            IrRole::Assistant if !content.is_empty() => json!({
                "id": id, "type": "assistant", "agent": "build",
                "model": { "id": model_id, "providerID": provider_id },
                "content": content, "finish": "stop",
                "time": { "created": created, "completed": created },
            }),
            // User and system turns both become user messages, as for 1.x.
            IrRole::User | IrRole::System if !texts.is_empty() => json!({
                "id": id, "type": "user", "text": texts.join("\n\n"), "time": { "created": created },
            }),
            _ => continue,
        };
        last = last.max(created);
        messages.push(value);
        loss.messages_out += 1;
    }
    // A turn that ended: what OpenCode records when it goes idle.
    messages.push(json!({
        "id": idempotency::opencode_message_id(session_id, ir.messages.len(), "idle"),
        "type": "idle", "outcome": "succeeded", "time": { "created": last },
    }));

    if let Some(claude_ext) = ir.extensions.get("claude-code") {
        let classes: Vec<String> = claude_ext
            .get("skipped_record_counts")
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        loss.extension_classes_dropped.extend(classes);
    }
    let mut info = json!({
        "id": session_id,
        "projectID": "x",
        "agent": "build",
        "model": { "id": model_id, "providerID": provider_id },
        "cost": 0,
        "tokens": { "input": 0, "output": 0, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
        "outcome": "succeeded",
        "time": { "created": ms(ir.created), "updated": ms(ir.updated).max(last), "idle": last },
        "title": ir.title.clone().unwrap_or_else(|| format!("Imported from {}", ir.source.agent)),
        "location": { "directory": project_dir.display().to_string() },
    });
    if info["time"]["created"].as_i64() > info["time"]["updated"].as_i64() {
        info["time"]["created"] = info["time"]["updated"].clone();
    }
    json!({ "info": info, "messages": messages })
}
