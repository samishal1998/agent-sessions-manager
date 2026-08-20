//! Claude Code's `--output-format stream-json` vocabulary.
//!
//! Shapes taken from a captured run of 2.1.x:
//! `system`/`init` opens the turn, `assistant` and `user` carry Anthropic
//! message blocks, and `result` closes it with cost and token totals.
//! Resuming by id appends to the same transcript — verified by watching the
//! session's JSONL grow while its id stayed the same.

use serde_json::Value;

use crate::ir::IrRole;
use crate::live::{AgentLive, LiveEvent, parse};
use crate::model::{Session, Usage};

use super::ClaudeAdapter;

impl AgentLive for ClaudeAdapter {
    fn send_command(&self, session: &Session, message: &str) -> Option<std::process::Command> {
        let mut cmd = std::process::Command::new("claude");
        cmd.arg("--resume")
            .arg(&session.handle.native_id)
            .arg("-p")
            .arg(message)
            .arg("--output-format")
            .arg("stream-json")
            // stream-json refuses to run without it, and it is what turns
            // the tool calls into events instead of hiding them.
            .arg("--verbose")
            .current_dir(&session.project_root);
        Some(cmd)
    }

    fn parse_event(&self, line: &str) -> Vec<LiveEvent> {
        let value = match parse::json_or_raw(line) {
            Ok(value) => value,
            Err(raw) => return raw,
        };
        match value.get("type").and_then(Value::as_str) {
            Some("system") => vec![LiveEvent::Started {
                session_id: parse::str_at(&value, &["session_id"]).map(str::to_string),
            }],
            Some("assistant") => blocks(&value, IrRole::Assistant),
            Some("user") => blocks(&value, IrRole::User),
            Some("result") => {
                let mut events = Vec::new();
                events.push(LiveEvent::Usage(Usage {
                    cost_usd: value.get("total_cost_usd").and_then(Value::as_f64),
                    input_tokens: parse::u64_at(&value, &["usage", "input_tokens"]),
                    output_tokens: parse::u64_at(&value, &["usage", "output_tokens"]),
                    cache_read_tokens: parse::u64_at(
                        &value,
                        &["usage", "cache_read_input_tokens"],
                    ),
                    cache_write_tokens: parse::u64_at(
                        &value,
                        &["usage", "cache_creation_input_tokens"],
                    ),
                }));
                // The driver emits its own Done from the exit status; this
                // one carries the reason when claude fails in-band and
                // still exits zero.
                if value.get("is_error").and_then(Value::as_bool) == Some(true) {
                    events.push(LiveEvent::Done {
                        ok: false,
                        error: Some(
                            parse::str_at(&value, &["result"])
                                .unwrap_or("the turn ended in an error")
                                .to_string(),
                        ),
                    });
                }
                events
            }
            // Housekeeping the user did not ask to see.
            Some("rate_limit_event") => Vec::new(),
            _ => vec![LiveEvent::Raw { line: line.to_string() }],
        }
    }
}

/// Anthropic content blocks out of a `message` envelope.
fn blocks(value: &Value, role: IrRole) -> Vec<LiveEvent> {
    let Some(content) = value.get("message").and_then(|m| m.get("content")) else {
        return Vec::new();
    };
    // A bare string is legal in the Messages API and does appear.
    if let Some(text) = content.as_str() {
        return vec![LiveEvent::Text { role, text: text.to_string() }];
    }
    let Some(items) = content.as_array() else { return Vec::new() };
    items
        .iter()
        .filter_map(|block| match block.get("type").and_then(Value::as_str) {
            Some("text") => block
                .get("text")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .map(|text| LiveEvent::Text { role, text: text.to_string() }),
            Some("thinking") => block
                .get("thinking")
                .and_then(Value::as_str)
                .map(|text| LiveEvent::Reasoning { text: text.to_string() }),
            Some("tool_use") => Some(LiveEvent::ToolCall {
                name: block.get("name").and_then(Value::as_str).unwrap_or("tool").to_string(),
                detail: block.get("input").map(summarize_input),
            }),
            Some("tool_result") => Some(LiveEvent::ToolResult {
                name: None,
                output: block.get("content").map(flatten_text).unwrap_or_default(),
                is_error: block.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            }),
            _ => None,
        })
        .collect()
}

/// A one-line gist of a tool's input. The full arguments belong in the
/// transcript, not in a streaming status line.
fn summarize_input(input: &Value) -> String {
    for key in ["command", "file_path", "pattern", "path", "prompt", "description"] {
        if let Some(value) = input.get(key).and_then(Value::as_str) {
            return value.chars().take(160).collect();
        }
    }
    input.to_string().chars().take(160).collect()
}

fn flatten_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        other => other.to_string(),
    }
}
