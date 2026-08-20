//! OpenCode's `run --format json` vocabulary.
//!
//! Every line is `{type, timestamp, sessionID, part}`. Verified against a
//! real run: text arrives as one complete part, not as growing snapshots,
//! so no delta bookkeeping is needed. `run -s <id>` appends to the session
//! — `--fork` exists precisely because the default does not fork.

use serde_json::Value;

use crate::ir::IrRole;
use crate::live::{AgentLive, LiveEvent, parse};
use crate::model::{Session, Usage};

use super::OpenCodeAdapter;

impl AgentLive for OpenCodeAdapter {
    fn send_command(&self, session: &Session, message: &str) -> Option<std::process::Command> {
        // Launched through `sh -c 'exec "$0" "$@"'` rather than directly.
        //
        // Executed straight from a non-shell parent, `opencode run` finishes
        // the turn — its own debug log reaches "exiting loop" in about two
        // seconds — and then never writes its buffered stdout and never
        // exits. Going through a shell that immediately `exec`s itself away
        // fixes it, with the same argv and the same process tree. The cause
        // is somewhere in the Bun runtime's startup; the other three agents
        // spawn directly and are fine.
        //
        // `exec "$0" "$@"` keeps every argument a real argv entry, so the
        // message is never interpolated into a shell string — a message
        // containing `;` or backticks is data, not syntax.
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(r#"exec "$0" "$@""#).arg("opencode");
        cmd.arg("run")
            .arg("-s")
            .arg(&session.handle.native_id)
            .arg("--format")
            .arg("json")
            // `--` so a message starting with a dash is not read as a flag.
            .arg("--")
            .arg(message)
            .current_dir(&session.project_root);
        Some(cmd)
    }

    fn parse_event(&self, line: &str) -> Vec<LiveEvent> {
        let value = match parse::json_or_raw(line) {
            Ok(value) => value,
            Err(raw) => return raw,
        };
        let part = value.get("part").unwrap_or(&Value::Null);
        match value.get("type").and_then(Value::as_str) {
            Some("step_start") => vec![LiveEvent::Started {
                session_id: parse::str_at(&value, &["sessionID"]).map(str::to_string),
            }],
            Some("text") => parse::str_at(part, &["text"])
                .map(|text| {
                    vec![LiveEvent::Text { role: IrRole::Assistant, text: text.to_string() }]
                })
                .unwrap_or_default(),
            Some("reasoning") => parse::str_at(part, &["text"])
                .map(|text| vec![LiveEvent::Reasoning { text: text.to_string() }])
                .unwrap_or_default(),
            Some("tool") => tool(part),
            Some("step_finish") => vec![LiveEvent::Usage(Usage {
                cost_usd: part.get("cost").and_then(Value::as_f64),
                input_tokens: parse::u64_at(part, &["tokens", "input"]),
                output_tokens: parse::u64_at(part, &["tokens", "output"]),
                cache_read_tokens: parse::u64_at(part, &["tokens", "cache", "read"]),
                cache_write_tokens: parse::u64_at(part, &["tokens", "cache", "write"]),
            })],
            Some("error") => vec![LiveEvent::Done {
                ok: false,
                // The useful sentence is buried; the envelope around it is
                // an HTTP response dump nobody wants in a chat pane.
                error: Some(
                    parse::str_at(&value, &["error", "data", "message"])
                        .or_else(|| parse::str_at(&value, &["error", "message"]))
                        .or_else(|| parse::str_at(&value, &["error", "name"]))
                        .unwrap_or("the turn failed")
                        .to_string(),
                ),
            }],
            _ => vec![LiveEvent::Raw { line: line.to_string() }],
        }
    }
}

/// A tool part carries the call and, once it has run, the result — in the
/// same shape, distinguished by `state.status`.
fn tool(part: &Value) -> Vec<LiveEvent> {
    let name = parse::str_at(part, &["tool"]).unwrap_or("tool").to_string();
    match parse::str_at(part, &["state", "status"]) {
        Some("completed") => vec![LiveEvent::ToolResult {
            name: Some(name),
            output: parse::str_at(part, &["state", "output"]).unwrap_or_default().to_string(),
            is_error: false,
        }],
        Some("error") => vec![LiveEvent::ToolResult {
            name: Some(name),
            output: parse::str_at(part, &["state", "error"])
                .unwrap_or("the tool failed")
                .to_string(),
            is_error: true,
        }],
        _ => vec![LiveEvent::ToolCall {
            name,
            detail: part.get("state").and_then(|s| s.get("input")).map(|i| {
                i.as_str().map(str::to_string).unwrap_or_else(|| i.to_string())
            }),
        }],
    }
}
