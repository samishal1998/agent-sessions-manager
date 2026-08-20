//! Codex's `exec --json` vocabulary.
//!
//! Note that this is *not* the rollout format: `exec --json` emits a
//! thread/turn/item event stream, while the rollout on disk records
//! `response_item` envelopes. The two describe the same turn in different
//! words, and only this one is a live stream.
//!
//! `codex exec resume <id>` appends — verified by watching the rollout grow
//! while `thread.started` reported the id we asked for.

use serde_json::Value;

use crate::ir::IrRole;
use crate::live::{AgentLive, LiveEvent, parse};
use crate::model::{Session, Usage};

use super::CodexAdapter;

impl AgentLive for CodexAdapter {
    fn send_command(&self, session: &Session, message: &str) -> Option<std::process::Command> {
        let mut cmd = std::process::Command::new("codex");
        cmd.arg("exec")
            .arg("resume")
            .arg(&session.handle.native_id)
            // Codex refuses to run outside a trusted directory, and there
            // is no way to answer that prompt headlessly. The session was
            // created in this directory by codex itself, so the check has
            // nothing left to protect here.
            .arg("--skip-git-repo-check")
            .arg("--json")
            .arg(message)
            .current_dir(&session.project_root);
        Some(cmd)
    }

    fn parse_event(&self, line: &str) -> Vec<LiveEvent> {
        let value = match parse::json_or_raw(line) {
            Ok(value) => value,
            Err(raw) => return raw,
        };
        match value.get("type").and_then(Value::as_str) {
            Some("thread.started") => vec![LiveEvent::Started {
                session_id: parse::str_at(&value, &["thread_id"]).map(str::to_string),
            }],
            Some("turn.started") => Vec::new(),
            Some("item.completed") | Some("item.started") => {
                item(value.get("item").unwrap_or(&Value::Null))
            }
            Some("turn.completed") => vec![LiveEvent::Usage(Usage {
                cost_usd: None,
                input_tokens: parse::u64_at(&value, &["usage", "input_tokens"]),
                output_tokens: parse::u64_at(&value, &["usage", "output_tokens"]),
                cache_read_tokens: parse::u64_at(&value, &["usage", "cached_input_tokens"]),
                cache_write_tokens: parse::u64_at(
                    &value,
                    &["usage", "cache_write_input_tokens"],
                ),
            })],
            Some("turn.failed") => vec![LiveEvent::Done {
                ok: false,
                error: Some(
                    parse::str_at(&value, &["error", "message"])
                        .unwrap_or("the turn failed")
                        .to_string(),
                ),
            }],
            // Codex retries transport failures and prints each attempt.
            // These are progress, not the outcome; the outcome is
            // `turn.failed`.
            Some("error") => vec![LiveEvent::Raw {
                line: parse::str_at(&value, &["message"]).unwrap_or(line).to_string(),
            }],
            _ => vec![LiveEvent::Raw { line: line.to_string() }],
        }
    }
}

fn item(item: &Value) -> Vec<LiveEvent> {
    match item.get("type").and_then(Value::as_str) {
        Some("agent_message") => parse::str_at(item, &["text"])
            .map(|text| {
                vec![LiveEvent::Text { role: IrRole::Assistant, text: text.to_string() }]
            })
            .unwrap_or_default(),
        Some("reasoning") => parse::str_at(item, &["text"])
            .map(|text| vec![LiveEvent::Reasoning { text: text.to_string() }])
            .unwrap_or_default(),
        Some("command_execution") => {
            let command =
                parse::str_at(item, &["command"]).unwrap_or_default().to_string();
            match parse::str_at(item, &["aggregated_output"]) {
                Some(output) => vec![LiveEvent::ToolResult {
                    name: Some("exec".to_string()),
                    output: output.to_string(),
                    is_error: item.get("exit_code").and_then(Value::as_i64).unwrap_or(0) != 0,
                }],
                None => vec![LiveEvent::ToolCall {
                    name: "exec".to_string(),
                    detail: Some(command),
                }],
            }
        }
        Some("error") => vec![LiveEvent::Raw {
            line: parse::str_at(item, &["message"]).unwrap_or_default().to_string(),
        }],
        Some(other) => vec![LiveEvent::ToolCall {
            name: other.to_string(),
            detail: parse::str_at(item, &["text"]).map(str::to_string),
        }],
        None => Vec::new(),
    }
}
