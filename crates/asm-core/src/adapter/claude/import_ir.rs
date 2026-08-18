//! IR → Claude Code: synthesize a native transcript JSONL.
//!
//! This is the fragile direction (the record format is documented as
//! internal and version-dependent), so the writer emits the minimal record
//! shape resume actually needs — a parentUuid-chained conversation whose
//! filename stem equals the sessionId — plus the state records that make
//! the picker useful (`ai-title`, `last-prompt`). Structure rules:
//! - assistant tool_use blocks are answered by tool_result blocks in the
//!   NEXT user record (Anthropic message pairing);
//! - reasoning cannot cross (thinking blocks are signature-bound) and is
//!   dropped with a loss entry;
//! - before writing, the id is scanned for across ALL project dirs — an
//!   existing transcript means "in sync" (a duplicate would poison
//!   cross-project resume).

use std::fs;

use jiff::Timestamp;
use serde_json::{Value, json};

use crate::CoreError;
use crate::import::{ImportOpts, ImportOutcome, LossReport, idempotency, toolmap};
use crate::ir::{IrPart, IrRole, IrSession};
use crate::model::{AgentKind, SessionLocation, SessionRef};

use super::ClaudeAdapter;

pub(super) fn import_ir(
    adapter: &ClaudeAdapter,
    ir: &IrSession,
    opts: &ImportOpts,
) -> Result<ImportOutcome, CoreError> {
    let session_id =
        idempotency::claude_uuid(ir.source.agent.as_str(), &ir.source.native_id, "session");
    let project_dir = ir.project_path.resolve();
    let project_dir_str = project_dir
        .to_str()
        .ok_or_else(|| CoreError::Invalid { msg: "project path is not UTF-8".into() })?;
    let encoded = super::path_encode::encode_project_dir(project_dir_str);
    let transcript_path =
        adapter.root().join("projects").join(&encoded).join(format!("{session_id}.jsonl"));

    let target_ref = SessionRef {
        agent: AgentKind::ClaudeCode,
        native_id: session_id.clone(),
        location: SessionLocation::JsonlFile { path: transcript_path.clone() },
    };
    let resume_hint = Some(format!("claude --resume {session_id}   (run in the project dir)"));

    // In-sync / duplicate scan across every project dir.
    if !opts.dry_run && transcript_exists_anywhere(adapter, &session_id)? {
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
    let transcript = build_transcript(ir, &session_id, project_dir_str, &mut loss);

    if opts.dry_run {
        return Ok(ImportOutcome {
            mode: opts.mode,
            target: Some(target_ref),
            in_sync: false,
            dry_run_artifact: Some(transcript),
            resume_hint,
            warnings: vec![],
            loss,
        });
    }

    if !project_dir.is_dir() {
        return Err(CoreError::Invalid {
            msg: format!("target project directory {} does not exist", project_dir.display()),
        });
    }

    let store_dir = transcript_path.parent().unwrap();
    fs::create_dir_all(store_dir).map_err(|e| CoreError::io(store_dir, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(store_dir, fs::Permissions::from_mode(0o700));
    }
    // Atomic materialization: write sibling temp, then rename.
    let tmp = store_dir.join(format!(".{session_id}.jsonl.tmp"));
    fs::write(&tmp, &transcript).map_err(|e| CoreError::io(&tmp, e))?;
    fs::rename(&tmp, &transcript_path).map_err(|e| CoreError::io(&transcript_path, e))?;

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

fn transcript_exists_anywhere(
    adapter: &ClaudeAdapter,
    session_id: &str,
) -> Result<bool, CoreError> {
    let projects = adapter.root().join("projects");
    let Ok(entries) = fs::read_dir(&projects) else { return Ok(false) };
    for entry in entries.filter_map(Result::ok) {
        if entry.path().join(format!("{session_id}.jsonl")).is_file() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn build_transcript(
    ir: &IrSession,
    session_id: &str,
    cwd: &str,
    loss: &mut LossReport,
) -> String {
    loss.messages_in = ir.messages.len();
    let source = ir.source.agent.as_str();
    let seed = &ir.source.native_id;
    let uuid = |disc: &str| idempotency::claude_uuid(source, seed, disc);
    let now = Timestamp::now();
    let slug = ir.slug.clone().unwrap_or_else(|| format!("imported-{}", &session_id[..8]));
    let version = crate::import::tested_versions::installed_version(AgentKind::ClaudeCode)
        .unwrap_or_else(|| "2.1.233".to_string());

    let mut lines: Vec<String> = Vec::new();
    let mut parent: Option<String> = None;
    let mut record_index = 0usize;
    let mut last_user_text = String::new();

    let envelope = |uuid_value: &str, parent: &Option<String>, ts: Timestamp| {
        json!({
            "uuid": uuid_value,
            "parentUuid": parent,
            "isSidechain": false,
            "userType": "external",
            "cwd": cwd,
            "sessionId": session_id,
            "version": version,
            "slug": slug,
            "timestamp": ts.to_string(),
        })
    };

    let mut push_record = |record: Value| lines.push(record.to_string());

    // Provenance rides as a meta user record — a standard type the UI hides
    // but the model sees as context.
    {
        let id = uuid(&format!("rec:{record_index}"));
        record_index += 1;
        let mut record = envelope(&id, &parent, ir.created.unwrap_or(now));
        record["type"] = json!("user");
        record["isMeta"] = json!(true);
        record["message"] = json!({
            "role": "user",
            "content": format!(
                "This session was imported from {} (session {}) by asm. The conversation \
                 below was translated from that agent's history; tool calls reference the \
                 source agent's runs.",
                ir.source.agent, ir.source.native_id
            ),
        });
        parent = Some(id);
        push_record(record);
    }

    for message in &ir.messages {
        let ts = message.timestamp.unwrap_or(now);
        match message.role {
            IrRole::Assistant => {
                // Assistant blocks first...
                let mut blocks: Vec<Value> = Vec::new();
                let mut pending_results: Vec<(String, String, bool)> = Vec::new();
                for part in &message.parts {
                    match part {
                        IrPart::Text { text } => blocks.push(json!({"type": "text", "text": text})),
                        IrPart::Reasoning { .. } => loss.reasoning_dropped += 1,
                        IrPart::ToolCall { call_id, name, input } => {
                            let mapped = match toolmap::map_tool(name, AgentKind::ClaudeCode) {
                                Some(m) => {
                                    loss.tools_mapped += 1;
                                    m.to_string()
                                }
                                None => {
                                    *loss.tools_kept_unmapped.entry(name.clone()).or_default() +=
                                        1;
                                    name.clone()
                                }
                            };
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": call_id,
                                "name": mapped,
                                "input": if input.is_null() { json!({}) } else { input.clone() },
                            }));
                        }
                        IrPart::ToolResult { call_id, output, is_error, .. } => {
                            pending_results.push((call_id.clone(), output.clone(), *is_error));
                        }
                        IrPart::Agent { transcript, .. } => {
                            loss.agent_transcripts_dropped += 1;
                            loss.notes.push(format!(
                                "a subagent run ({} turns) was flattened out",
                                transcript.len()
                            ));
                        }
                        IrPart::File { .. } | IrPart::Unknown => {}
                    }
                }
                if !blocks.is_empty() {
                    let id = uuid(&format!("rec:{record_index}"));
                    record_index += 1;
                    let mut record = envelope(&id, &parent, ts);
                    record["type"] = json!("assistant");
                    record["message"] = json!({
                        "id": format!("msg_{}", &uuid(&format!("msg:{record_index}"))[..24]),
                        "type": "message",
                        "role": "assistant",
                        "model": ir.model.clone().unwrap_or_else(|| "imported".to_string()),
                        "content": blocks,
                        "stop_reason": "end_turn",
                        "stop_sequence": null,
                        "usage": { "input_tokens": 0, "output_tokens": 0 },
                    });
                    parent = Some(id);
                    push_record(record);
                    loss.messages_out += 1;
                }
                // ...then their results as the following user record.
                if !pending_results.is_empty() {
                    let content: Vec<Value> = pending_results
                        .iter()
                        .map(|(call_id, output, is_error)| {
                            json!({
                                "type": "tool_result",
                                "tool_use_id": call_id,
                                "content": output,
                                "is_error": is_error,
                            })
                        })
                        .collect();
                    let id = uuid(&format!("rec:{record_index}"));
                    record_index += 1;
                    let mut record = envelope(&id, &parent, ts);
                    record["type"] = json!("user");
                    record["message"] = json!({ "role": "user", "content": content });
                    parent = Some(id);
                    push_record(record);
                }
            }
            IrRole::User | IrRole::System => {
                let mut texts: Vec<String> = Vec::new();
                let mut results: Vec<Value> = Vec::new();
                for part in &message.parts {
                    match part {
                        IrPart::Text { text } => texts.push(text.clone()),
                        IrPart::ToolResult { call_id, output, is_error, .. } => {
                            results.push(json!({
                                "type": "tool_result",
                                "tool_use_id": call_id,
                                "content": output,
                                "is_error": is_error,
                            }));
                        }
                        IrPart::Reasoning { .. } => loss.reasoning_dropped += 1,
                        _ => {}
                    }
                }
                let content: Value = if results.is_empty() && texts.len() == 1 {
                    json!(texts[0])
                } else {
                    let mut blocks = results;
                    blocks
                        .extend(texts.iter().map(|t| json!({ "type": "text", "text": t })));
                    if blocks.is_empty() {
                        continue;
                    }
                    json!(blocks)
                };
                if let Some(text) = texts.last() {
                    last_user_text = text.clone();
                }
                let id = uuid(&format!("rec:{record_index}"));
                record_index += 1;
                let mut record = envelope(&id, &parent, ts);
                record["type"] = json!("user");
                record["message"] = json!({ "role": "user", "content": content });
                parent = Some(id);
                push_record(record);
                loss.messages_out += 1;
            }
        }
    }

    // Picker state: title + resume leaf.
    let title = ir
        .title
        .clone()
        .unwrap_or_else(|| format!("Imported from {}", ir.source.agent));
    lines.push(json!({ "type": "ai-title", "aiTitle": title, "sessionId": session_id }).to_string());
    if let Some(leaf) = &parent {
        lines.push(
            json!({
                "type": "last-prompt",
                "lastPrompt": clip(&last_user_text, 500),
                "leafUuid": leaf,
                "sessionId": session_id,
            })
            .to_string(),
        );
    }

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}
