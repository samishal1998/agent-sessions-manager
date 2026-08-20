//! Antigravity's `agy --output-format stream-json` vocabulary.
//!
//! Captured from a real run: `{"event":"init"|"step_update"|"result", …}`.
//! `agy --conversation <id>` appends — the step indices continued 3,4,5 into
//! the same conversation database, with the same conversation id.
//!
//! Unlike the other three agents, antigravity streams *deltas*
//! (`text_delta`), so the reply arrives a piece at a time. The final
//! `result.response` repeats the whole answer, which is why it is not
//! emitted: doing so would print every reply twice.

use serde_json::Value;

use crate::ir::IrRole;
use crate::live::{AgentLive, LiveEvent, parse};
use crate::model::{Session, Usage};

use super::AntigravityAdapter;

fn usage_of(value: &Value) -> Option<Usage> {
    let usage = value.get("usage")?;
    Some(Usage {
        cost_usd: None,
        input_tokens: usage.get("input_tokens").and_then(Value::as_u64),
        output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
        cache_read_tokens: usage.get("cache_read_tokens").and_then(Value::as_u64),
        cache_write_tokens: None,
    })
}

impl AgentLive for AntigravityAdapter {
    fn send_command(&self, session: &Session, message: &str) -> Option<std::process::Command> {
        let mut cmd = std::process::Command::new("agy");
        cmd.arg("--conversation")
            .arg(&session.handle.native_id)
            .arg("--output-format")
            .arg("stream-json")
            .arg("-p")
            .arg(message);
        if session.project_root.is_dir() {
            cmd.current_dir(&session.project_root);
        }
        Some(cmd)
    }

    fn parse_event(&self, line: &str) -> Vec<LiveEvent> {
        let value = match parse::json_or_raw(line) {
            Ok(value) => value,
            Err(raw) => return raw,
        };
        match value.get("event").and_then(Value::as_str) {
            Some("init") => vec![LiveEvent::Started {
                session_id: parse::str_at(&value, &["conversation_id"]).map(str::to_string),
            }],
            Some("step_update") => {
                let step = value.get("step_update").unwrap_or(&Value::Null);
                let mut events = Vec::new();
                if let Some(text) = parse::str_at(step, &["text_delta"]) {
                    let event = match parse::str_at(step, &["step_type"]) {
                        // Antigravity's thinking arrives under its own step
                        // type rather than mixed into the answer.
                        Some("thinking") | Some("reasoning") => {
                            LiveEvent::Reasoning { text: text.to_string() }
                        }
                        _ => LiveEvent::Text { role: IrRole::Assistant, text: text.to_string() },
                    };
                    events.push(event);
                } else if let Some(step_type) = parse::str_at(step, &["step_type"])
                    && matches!(step_type, "tool_call" | "command" | "action")
                {
                    events.push(LiveEvent::ToolCall {
                        name: step_type.to_string(),
                        detail: parse::str_at(step, &["title"]).map(str::to_string),
                    });
                }
                if let Some(usage) = usage_of(step) {
                    events.push(LiveEvent::Usage(usage));
                }
                events
            }
            Some("result") => {
                let result = value.get("result").unwrap_or(&Value::Null);
                let mut events = Vec::new();
                if let Some(usage) = usage_of(result) {
                    events.push(LiveEvent::Usage(usage));
                }
                // `result.response` is the whole answer again; the deltas
                // above already carried it.
                let ok = parse::str_at(result, &["status"]) == Some("SUCCESS");
                if !ok {
                    events.push(LiveEvent::Done {
                        ok: false,
                        error: Some(
                            parse::str_at(result, &["error"])
                                .or_else(|| parse::str_at(result, &["status"]))
                                .unwrap_or("the turn failed")
                                .to_string(),
                        ),
                    });
                }
                events
            }
            _ => vec![LiveEvent::Raw { line: line.to_string() }],
        }
    }
}
