//! Live-stream normalizers, fed the real output of each agent.
//!
//! Every line below was captured from an actual run against a real install,
//! not written from the flag documentation: Claude Code 2.1.x
//! (`--output-format stream-json`), OpenCode 1.x (`run --format json`) and
//! Codex 0.148.0 (`exec --json`). When one of these formats changes, these
//! tests are what notices.

use asm_core::adapter::antigravity::AntigravityAdapter;
use asm_core::adapter::claude::ClaudeAdapter;
use asm_core::adapter::codex::CodexAdapter;
use asm_core::adapter::opencode::OpenCodeAdapter;
use asm_core::ir::IrRole;
use asm_core::live::{AgentLive, LiveEvent};

fn claude() -> ClaudeAdapter {
    ClaudeAdapter::with_root("/nonexistent")
}
fn opencode() -> OpenCodeAdapter {
    OpenCodeAdapter::with_db("/nonexistent/opencode.db")
}
fn codex() -> CodexAdapter {
    CodexAdapter::with_root("/nonexistent")
}
fn antigravity() -> AntigravityAdapter {
    AntigravityAdapter::with_root("/nonexistent")
}

fn texts(events: &[LiveEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| match e {
            LiveEvent::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn claude_init_assistant_and_result() {
    let a = claude();

    let started = a.parse_event(
        r#"{"type":"system","subtype":"init","cwd":"/p","session_id":"d82945c2-88c5-4ddb-bcdf-fdff19b1e6ff"}"#,
    );
    assert_eq!(
        started,
        vec![LiveEvent::Started {
            session_id: Some("d82945c2-88c5-4ddb-bcdf-fdff19b1e6ff".into())
        }]
    );

    let reply = a.parse_event(
        r#"{"type":"assistant","message":{"model":"claude-opus-5","role":"assistant","content":[{"type":"text","text":"first"}]}}"#,
    );
    assert_eq!(texts(&reply), vec!["first"]);

    let tool = a.parse_event(
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls -la"}}]}}"#,
    );
    assert_eq!(
        tool,
        vec![LiveEvent::ToolCall { name: "Bash".into(), detail: Some("ls -la".into()) }]
    );

    let result = a.parse_event(
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"d8","total_cost_usd":0.073339,"usage":{"input_tokens":2,"output_tokens":7,"cache_read_input_tokens":11}}"#,
    );
    match &result[0] {
        LiveEvent::Usage(usage) => {
            assert_eq!(usage.cost_usd, Some(0.073339));
            assert_eq!(usage.output_tokens, Some(7));
            assert_eq!(usage.cache_read_tokens, Some(11));
        }
        other => panic!("expected usage, got {other:?}"),
    }

    // Rate-limit notices are housekeeping, not conversation.
    assert!(
        a.parse_event(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#)
            .is_empty()
    );
}

#[test]
fn opencode_text_tokens_and_error() {
    let a = opencode();

    let text = a.parse_event(
        r#"{"type":"text","timestamp":1787255554689,"sessionID":"ses_x","part":{"id":"prt_1","type":"text","text":"one\ntwo"}}"#,
    );
    assert_eq!(texts(&text), vec!["one\ntwo"]);

    // Token totals live under `part.tokens`, with the cache split one level
    // deeper again.
    let finish = a.parse_event(
        r#"{"type":"step_finish","sessionID":"ses_x","part":{"id":"p","reason":"stop","tokens":{"total":6256,"input":588,"output":7,"reasoning":29,"cache":{"write":0,"read":5632}},"cost":0.5}}"#,
    );
    match &finish[0] {
        LiveEvent::Usage(usage) => {
            assert_eq!(usage.input_tokens, Some(588));
            assert_eq!(usage.cache_read_tokens, Some(5632));
            assert_eq!(usage.cost_usd, Some(0.5));
        }
        other => panic!("expected usage, got {other:?}"),
    }

    // The one useful sentence is three levels inside an HTTP dump.
    let failed = a.parse_event(
        r#"{"type":"error","sessionID":"ses_x","error":{"name":"APIError","data":{"message":"The requested model is not approved for this API key","statusCode":403}}}"#,
    );
    assert_eq!(
        failed,
        vec![LiveEvent::Done {
            ok: false,
            error: Some("The requested model is not approved for this API key".into())
        }]
    );
}

#[test]
fn codex_items_and_turn_totals() {
    let a = codex();

    assert_eq!(
        a.parse_event(r#"{"type":"thread.started","thread_id":"01a020ab-9a2c"}"#),
        vec![LiveEvent::Started { session_id: Some("01a020ab-9a2c".into()) }]
    );

    let message = a.parse_event(
        r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"second"}}"#,
    );
    assert_eq!(texts(&message), vec!["second"]);

    let usage = a.parse_event(
        r#"{"type":"turn.completed","usage":{"input_tokens":15216,"cached_input_tokens":14080,"output_tokens":5}}"#,
    );
    match &usage[0] {
        LiveEvent::Usage(u) => {
            assert_eq!(u.input_tokens, Some(15216));
            assert_eq!(u.cache_read_tokens, Some(14080));
        }
        other => panic!("expected usage, got {other:?}"),
    }

    // Codex prints a line per transport retry. Those are progress; the
    // outcome is `turn.failed`, and only that must read as a failure.
    let retry = a.parse_event(r#"{"type":"error","message":"Reconnecting... 2/5 (401)"}"#);
    assert!(matches!(retry.as_slice(), [LiveEvent::Raw { .. }]), "{retry:?}");
    let failed = a.parse_event(
        r#"{"type":"turn.failed","error":{"message":"unexpected status 401 Unauthorized"}}"#,
    );
    assert_eq!(
        failed,
        vec![LiveEvent::Done {
            ok: false,
            error: Some("unexpected status 401 Unauthorized".into())
        }]
    );
}

/// Format drift must be visible, not silent. A line nobody recognizes is
/// still shown to the user rather than dropped, or a changed vocabulary
/// reads as "the agent replied with nothing".
#[test]
fn unknown_lines_are_kept_verbatim_by_every_parser() {
    let line = r#"{"type":"some_future_event","detail":42}"#;
    for events in [
        claude().parse_event(line),
        opencode().parse_event(line),
        codex().parse_event(line),
        antigravity().parse_event(line),
    ] {
        assert!(matches!(events.as_slice(), [LiveEvent::Raw { .. }]), "{events:?}");
    }
    // Not every line is even JSON; update notices land on the same stream.
    let notice = "npm notice New version available";
    for events in [
        claude().parse_event(notice),
        opencode().parse_event(notice),
        codex().parse_event(notice),
        antigravity().parse_event(notice),
    ] {
        assert_eq!(events, vec![LiveEvent::Raw { line: notice.to_string() }]);
    }
}

/// The roles matter: a tool result arrives inside a `user` envelope in
/// Claude's stream, and rendering that as something the human typed would
/// be a lie.
#[test]
fn claude_tool_results_are_not_user_speech() {
    let events = claude().parse_event(
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"ok"}],"is_error":false}]}}"#,
    );
    assert_eq!(
        events,
        vec![LiveEvent::ToolResult { name: None, output: "ok".into(), is_error: false }]
    );
    assert!(texts(&events).is_empty());
}

/// Antigravity is the only agent that streams deltas rather than whole
/// messages, and the only one whose final event repeats the entire answer.
/// Emitting that repeat would print every reply twice.
#[test]
fn antigravity_streams_deltas_and_does_not_repeat_the_result() {
    let a = antigravity();

    assert_eq!(
        a.parse_event(
            r#"{"event":"init","conversation_id":"3f6bfd66-67b6-4c35-9065-5316a755e9af","init":{"cwd":"/p"}}"#
        ),
        vec![LiveEvent::Started {
            session_id: Some("3f6bfd66-67b6-4c35-9065-5316a755e9af".into())
        }]
    );

    let first = a.parse_event(
        r#"{"event":"step_update","step_update":{"conversation_id":"c","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"sev"}}"#,
    );
    let second = a.parse_event(
        r#"{"event":"step_update","step_update":{"conversation_id":"c","step_index":2,"state":"DONE","step_type":"agent_response","text_delta":"enth","usage":{"input_tokens":13875,"output_tokens":30}}}"#,
    );
    assert_eq!(texts(&first), vec!["sev"]);
    assert_eq!(texts(&second), vec!["enth"], "the two deltas concatenate to the answer");
    assert!(second.iter().any(|e| matches!(e, LiveEvent::Usage(_))));

    // The result repeats "seventh" whole; only its totals are new.
    let result = a.parse_event(
        r#"{"event":"result","result":{"conversation_id":"c","status":"SUCCESS","response":"seventh\n","usage":{"input_tokens":13875,"output_tokens":30}}}"#,
    );
    assert!(texts(&result).is_empty(), "the whole answer must not be emitted again");
    assert!(result.iter().any(|e| matches!(e, LiveEvent::Usage(_))));

    let failed = a.parse_event(
        r#"{"event":"result","result":{"conversation_id":"c","status":"ERROR","error":"quota exhausted"}}"#,
    );
    assert!(
        failed.iter().any(
            |e| matches!(e, LiveEvent::Done { ok: false, error } if error.as_deref() == Some("quota exhausted"))
        ),
        "{failed:?}"
    );
}

/// jcode has no captured stream yet, so it must not claim it can send.
#[test]
fn jcode_does_not_pretend_to_support_sending() {
    use asm_core::adapter::AgentRead;
    use asm_core::adapter::jcode::JCodeAdapter;
    let a = JCodeAdapter::with_root("/nonexistent");
    assert!(!a.capabilities().send_message);
}

/// A defensive check on the role enum used by the parsers, so a reordering
/// of `IrRole` cannot silently relabel assistant output.
#[test]
fn assistant_text_is_labelled_assistant() {
    let events = codex()
        .parse_event(r#"{"type":"item.completed","item":{"type":"agent_message","text":"hi"}}"#);
    assert_eq!(events, vec![LiveEvent::Text { role: IrRole::Assistant, text: "hi".into() }]);
}
