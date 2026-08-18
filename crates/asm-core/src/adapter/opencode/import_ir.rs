//! IR → OpenCode, through the sanctioned path: synthesize the exact
//! document `opencode export` emits and feed it to `opencode import`,
//! invoked with cwd = the target project directory.
//!
//! Why this shape works (verified against the 1.17.18 binary's embedded
//! source): import schema-validates the document, PRESERVES our ids
//! (prefix-checked strings — deterministic ids are legal and make re-import
//! idempotent), computes the opaque projectID/directory from its own cwd,
//! and inserts through OpenCode's migration-aware code path. The one trap:
//! a session with a `parentID` is invisible to `session list` — root
//! sessions must omit it.

use std::collections::HashMap;
use std::fs;

use jiff::Timestamp;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

use crate::CoreError;
use crate::import::{ImportOpts, ImportOutcome, LossReport, idempotency, toolmap};
use crate::ir::{IrPart, IrRole, IrSession};
use crate::model::{AgentKind, SessionLocation, SessionRef};

use super::OpenCodeAdapter;

pub(super) fn import_ir(
    adapter: &OpenCodeAdapter,
    ir: &IrSession,
    opts: &ImportOpts,
) -> Result<ImportOutcome, CoreError> {
    let session_id =
        idempotency::opencode_session_id(ir.source.agent.as_str(), &ir.source.native_id);
    let target_ref = SessionRef {
        agent: AgentKind::OpenCode,
        native_id: session_id.clone(),
        location: SessionLocation::SqliteRow {
            db: adapter.db().to_path_buf(),
            table: "session".to_string(),
        },
    };
    let resume_hint = Some(format!("opencode -s {session_id}   (run in the project dir)"));

    // Idempotency: the deterministic id already present WITH CONTENT means
    // this session was imported before. A bare session row is not enough —
    // `opencode import` commits the session row before decoding parts, so a
    // failed import leaves one behind, and treating that as "in sync" would
    // permanently mask a half-written session.
    if !opts.dry_run && session_has_content(adapter, &session_id)? {
        return Ok(ImportOutcome {
            mode: opts.mode,
            target: Some(target_ref),
            in_sync: true,
            dry_run_artifact: None,
            resume_hint,
            warnings: vec![],
            loss: LossReport::default(),
        });
    }

    let mut loss = LossReport::default();
    // Resume continues with the session's model, so imported messages must
    // reference a provider/model this install can actually serve — pinning
    // the SOURCE model (e.g. anthropic/claude-fable-5 with no anthropic
    // provider configured) makes resume fail with ProviderModelNotFound.
    let model = preferred_model(adapter);
    if let Some((provider, model_id)) = &model {
        loss.notes.push(format!(
            "conversation re-attributed to {provider}/{model_id} (the target install's last-used \
             model) so resume works; the source model ({}) is recorded in the provenance",
            ir.model.as_deref().unwrap_or("unknown")
        ));
    }
    let document = build_document(ir, &session_id, model, &mut loss);
    let pretty = serde_json::to_string_pretty(&document).unwrap();

    if opts.dry_run {
        return Ok(ImportOutcome {
            mode: opts.mode,
            target: Some(target_ref),
            in_sync: false,
            dry_run_artifact: Some(pretty),
            resume_hint,
            warnings: vec![],
            loss,
        });
    }

    let project_dir = ir.project_path.resolve();
    if !project_dir.is_dir() {
        return Err(CoreError::Invalid {
            msg: format!(
                "target project directory {} does not exist (opencode import binds the session \
                 to its cwd)",
                project_dir.display()
            ),
        });
    }

    // Hand the document to `opencode import` from the target project dir.
    let tmp_dir = crate::paths::data_dir()
        .ok_or_else(|| CoreError::Invalid { msg: "cannot determine asm data dir".into() })?
        .join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| CoreError::io(&tmp_dir, e))?;
    let doc_path = tmp_dir.join(format!("{session_id}.import.json"));
    fs::write(&doc_path, &pretty).map_err(|e| CoreError::io(&doc_path, e))?;

    let output = std::process::Command::new("opencode")
        .arg("import")
        .arg(&doc_path)
        .current_dir(&project_dir)
        .output()
        .map_err(|e| CoreError::Invalid { msg: format!("failed to run opencode: {e}") })?;
    // `opencode import` can also report success on stdout while having
    // aborted partway ("Missing key at …"), so the exit status alone is not
    // the test — the store is.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let imported = session_has_content(adapter, &session_id)?;
    if !output.status.success() || !imported {
        // Roll back the partial session rather than leave it to be mistaken
        // for a completed import later.
        let removed = discard_partial(adapter, &session_id).unwrap_or(false);
        let _ = fs::remove_file(&doc_path);
        return Err(CoreError::Invalid {
            msg: format!(
                "opencode import did not complete ({}): {combined}{}",
                output.status,
                if removed { " [partial session rolled back]" } else { "" },
            ),
        });
    }
    let _ = fs::remove_file(&doc_path);

    Ok(ImportOutcome {
        mode: opts.mode,
        target: Some(target_ref),
        in_sync: false,
        dry_run_artifact: None,
        resume_hint,
        warnings: vec![],
        loss,
    })
}

/// A session counts as imported only if it has both a row and at least one
/// message: `opencode import` commits the session before decoding parts.
fn session_has_content(adapter: &OpenCodeAdapter, id: &str) -> Result<bool, CoreError> {
    if !adapter.db().is_file() {
        return Ok(false);
    }
    let conn = Connection::open_with_flags(
        adapter.db(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| CoreError::Sqlite { db: adapter.db().to_path_buf(), source: Box::new(e) })?;
    let has_session = conn
        .query_row("SELECT 1 FROM session WHERE id = ?1", [id], |_| Ok(()))
        .is_ok();
    let has_message = conn
        .query_row("SELECT 1 FROM message WHERE session_id = ?1 LIMIT 1", [id], |_| Ok(()))
        .is_ok();
    Ok(has_session && has_message)
}

/// Remove the rows a failed import left behind. Best effort: if the store
/// is busy we would rather report the failure than fight for the lock.
fn discard_partial(adapter: &OpenCodeAdapter, id: &str) -> Result<bool, CoreError> {
    if adapter.store_busy() || !adapter.db().is_file() {
        return Ok(false);
    }
    let conn = Connection::open(adapter.db())
        .map_err(|e| CoreError::Sqlite { db: adapter.db().to_path_buf(), source: Box::new(e) })?;
    let mut removed = false;
    for (table, column) in
        [("part", "session_id"), ("message", "session_id"), ("session", "id")]
    {
        if let Ok(count) =
            conn.execute(&format!("DELETE FROM {table} WHERE {column} = ?1"), [id])
        {
            removed |= count > 0;
        }
    }
    Ok(removed)
}

/// Most recently used (providerID, modelID) in the target store — the one
/// model we KNOW the install can serve.
fn preferred_model(adapter: &OpenCodeAdapter) -> Option<(String, String)> {
    if !adapter.db().is_file() {
        return None;
    }
    let conn = Connection::open_with_flags(
        adapter.db(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let model_json: String = conn
        .query_row(
            "SELECT model FROM session WHERE model IS NOT NULL AND model != ''
             ORDER BY time_updated DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok()?;
    let value: Value = serde_json::from_str(&model_json).ok()?;
    Some((
        value.get("providerID")?.as_str()?.to_string(),
        value.get("id")?.as_str()?.to_string(),
    ))
}

/// Build the `{info, messages:[{info,parts}]}` document. `model` overrides
/// the (provider, model) every message is attributed to.
pub(crate) fn build_document(
    ir: &IrSession,
    session_id: &str,
    model: Option<(String, String)>,
    loss: &mut LossReport,
) -> Value {
    loss.messages_in = ir.messages.len();

    // First pass: index tool results by call id so call+result merge into
    // OpenCode's single `tool` part.
    let mut results: HashMap<&str, (&str, bool)> = HashMap::new();
    for message in &ir.messages {
        for part in &message.parts {
            if let IrPart::ToolResult { call_id, output, is_error, .. } = part {
                results.insert(call_id.as_str(), (output.as_str(), *is_error));
            }
        }
    }

    let now_ms = Timestamp::now().as_millisecond();
    let ms = |ts: Option<Timestamp>| ts.map(|t| t.as_millisecond()).unwrap_or(now_ms);
    let (provider_id, model_id) = model.unwrap_or_else(|| split_model(ir));

    let mut messages = Vec::new();
    let mut part_counter = 0usize;
    let mut previous_message_id: Option<String> = None;

    for (index, message) in ir.messages.iter().enumerate() {
        let source_id = message.source_id.clone().unwrap_or_else(|| format!("idx:{index}"));
        let message_id = idempotency::opencode_message_id(session_id, index, &source_id);
        let created = ms(message.timestamp);

        let mut parts = Vec::new();
        for part in &message.parts {
            let value = match part {
                IrPart::Text { text } => Some(json!({ "type": "text", "text": text })),
                IrPart::Reasoning { summary, opaque } => {
                    if *opaque {
                        loss.reasoning_dropped += 1;
                    }
                    // Readable reasoning text survives as a reasoning part.
                    (!summary.is_empty())
                        .then(|| json!({ "type": "reasoning", "text": summary,
                                         "time": { "start": created, "end": created } }))
                }
                IrPart::ToolCall { call_id, name, input } => {
                    let tool = match toolmap::map_tool(name, AgentKind::OpenCode) {
                        Some(mapped) => {
                            loss.tools_mapped += 1;
                            mapped.to_string()
                        }
                        None => {
                            *loss.tools_kept_unmapped.entry(name.clone()).or_default() += 1;
                            name.clone()
                        }
                    };
                    let (output, is_error) =
                        results.get(call_id.as_str()).copied().unwrap_or(("", false));
                    let input = if input.is_null() { json!({}) } else { input.clone() };
                    let time = json!({ "start": created, "end": created });
                    // The tool-state union is discriminated on `status` and
                    // the two members differ in shape: ToolStateError has a
                    // REQUIRED `error` string and no `output`/`title`.
                    // Emitting the completed shape with status "error" makes
                    // `opencode import` reject the part ("Missing key at
                    // [state][error]") and abort the whole import.
                    let state = if is_error {
                        json!({
                            "status": "error",
                            "input": input,
                            "error": output,
                            "time": time,
                            "metadata": {},
                        })
                    } else {
                        json!({
                            "status": "completed",
                            "input": input,
                            "output": output,
                            "title": name,
                            "time": time,
                            "metadata": {},
                        })
                    };
                    Some(json!({
                        "type": "tool",
                        "callID": call_id,
                        "tool": tool,
                        "state": state,
                    }))
                }
                // Consumed by the merge above.
                IrPart::ToolResult { .. } => None,
                IrPart::File { path, mime, .. } => Some(json!({
                    "type": "file",
                    "mime": mime.clone().unwrap_or_else(|| "text/plain".to_string()),
                    "filename": path.as_ref().map(|p| p.0.clone()),
                    "url": path.as_ref().map(|p| format!("file://{}", p.resolve().display())),
                })),
                IrPart::Agent { transcript, .. } => {
                    loss.agent_transcripts_dropped += 1;
                    loss.notes.push(format!(
                        "a subagent run ({} turns) was flattened out",
                        transcript.len()
                    ));
                    None
                }
                IrPart::Unknown => None,
            };
            if let Some(mut value) = value {
                value["id"] = json!(idempotency::opencode_part_id(session_id, part_counter));
                value["sessionID"] = json!(session_id);
                value["messageID"] = json!(message_id);
                part_counter += 1;
                parts.push(value);
            }
        }
        if parts.is_empty() {
            continue;
        }

        let info = match message.role {
            IrRole::Assistant => json!({
                "id": message_id,
                "sessionID": session_id,
                "role": "assistant",
                "time": { "created": created, "completed": created },
                "parentID": previous_message_id.clone().unwrap_or_else(|| message_id.clone()),
                "modelID": model_id,
                "providerID": provider_id,
                "mode": "build",
                "agent": "build",
                "path": { "cwd": ir.project_path.resolve().display().to_string(), "root": "/" },
                "cost": 0.0,
                "tokens": { "input": 0, "output": 0, "reasoning": 0,
                            "cache": { "read": 0, "write": 0 } },
            }),
            // User and system turns both become user messages (OpenCode has
            // no system role in its message union).
            _ => json!({
                "id": message_id,
                "sessionID": session_id,
                "role": "user",
                "time": { "created": created },
                "agent": "build",
                "model": { "providerID": provider_id, "modelID": model_id },
            }),
        };
        previous_message_id = Some(message_id.clone());
        messages.push(json!({ "info": info, "parts": parts }));
        loss.messages_out += 1;
    }

    if let Some(claude_ext) = ir.extensions.get("claude-code") {
        let classes: Vec<String> = claude_ext
            .get("skipped_record_counts")
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        loss.extension_classes_dropped.extend(classes);
    }

    json!({
        "info": {
            "id": session_id,
            "slug": ir.slug.clone().unwrap_or_else(|| format!("imported-{}", &session_id[4..12])),
            "title": ir
                .title
                .clone()
                .unwrap_or_else(|| format!("Imported from {}", ir.source.agent)),
            "version": "1.17.18",
            "time": { "created": ms(ir.created), "updated": ms(ir.updated) },
            "model": { "id": model_id, "providerID": provider_id },
        },
        "messages": messages,
    })
}

fn split_model(ir: &IrSession) -> (String, String) {
    let provider = match ir.source.agent {
        AgentKind::ClaudeCode => "anthropic",
        AgentKind::OpenCode => "unknown",
    };
    (provider.to_string(), ir.model.clone().unwrap_or_else(|| "imported".to_string()))
}
