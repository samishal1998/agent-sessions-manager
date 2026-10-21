//! The JSON wire contract.
//!
//! `asm --json` and the web API are consumed by things that are not this
//! crate — the Vue frontend, and whatever the user pipes `--json` into.
//! Renaming a serialized field is therefore a breaking change that the Rust
//! compiler cannot catch, and has already broken the frontend twice during
//! development (`handle` serializing as `ref`, and `status` being an
//! internally tagged object rather than a plain string).
//!
//! These tests pin the exact shape. If one fails, either restore the field
//! name or update every consumer deliberately — do not just fix the test.

use std::path::PathBuf;

use serde_json::Value;

use asm_core::model::{
    AgentKind, Session, SessionLocation, SessionRef, SessionStatus, Usage,
};

fn sample(status: SessionStatus) -> Session {
    Session {
        handle: SessionRef {
            agent: AgentKind::ClaudeCode,
            native_id: "abc-123".into(),
            location: SessionLocation::JsonlFile { path: PathBuf::from("/t/abc-123.jsonl") },
        },
        title: Some("A title".into()),
        slug: None,
        project_root: PathBuf::from("/t/project"),
        git_branch: Some("main".into()),
        created: None,
        updated: None,
        model: None,
        usage: Usage::default(),
        status,
        parent: None,
        agent_version: None,
        size_bytes: None,
    }
}

fn json(status: SessionStatus) -> Value {
    serde_json::to_value(sample(status)).unwrap()
}

#[test]
fn session_field_names_are_stable() {
    let value = json(SessionStatus::Idle);
    // The handle field is exposed as "ref" — the frontend and --json
    // consumers address sessions by ref.agent / ref.native_id.
    assert!(value.get("handle").is_none(), "handle must serialize as 'ref'");
    assert_eq!(value["ref"]["agent"], "claude-code");
    assert_eq!(value["ref"]["native_id"], "abc-123");
    for field in ["title", "slug", "project_root", "git_branch", "created", "updated", "model", "usage", "status", "parent", "agent_version"] {
        assert!(value.get(field).is_some(), "missing field {field}");
    }
}

/// The UIs group children into a tree from this one field: a null for a
/// root, the parent's native id for a child.
#[test]
fn parent_is_the_native_id_of_the_parent_or_null() {
    assert_eq!(json(SessionStatus::Idle)["parent"], Value::Null);
    let mut child = sample(SessionStatus::Idle);
    child.parent = Some("ses_boss".into());
    let value = serde_json::to_value(&child).unwrap();
    assert_eq!(value["parent"], "ses_boss");
    let back: Session = serde_json::from_value(value).unwrap();
    assert_eq!(back.parent.as_deref(), Some("ses_boss"));
    // No field was added to carry the tree: it is grouped from `parent`.
    assert_eq!(serde_json::to_value(sample(SessionStatus::Idle)).unwrap().as_object().unwrap().len(), 15);
}

/// The exact keys of a session. 0.16 added `id` and `state`, flat copies of
/// `ref.native_id` and `status.state`; anything else is a contract change.
#[test]
fn session_has_exactly_these_keys() {
    let value = json(SessionStatus::Idle);
    let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "agent_version", "created", "git_branch", "id", "model", "parent", "project_root", "ref", "size_bytes",
            "slug", "state", "status", "title", "updated", "usage"
        ]
    );
}

/// `status` stays an object (`{"state": ..}`) and `state` is its flat string,
/// so a reader that only wants the word need not know which it is looking at.
#[test]
fn state_and_id_are_flat_copies() {
    for (status, state) in [
        (SessionStatus::Idle, "idle"),
        (SessionStatus::Archived, "archived"),
        (SessionStatus::Live { pid: Some(3) }, "live"),
    ] {
        let value = json(status);
        assert_eq!(value["state"], state);
        assert_eq!(value["status"]["state"], state, "the object is unchanged");
        assert_eq!(value["id"], "abc-123");
        assert_eq!(value["id"], value["ref"]["native_id"]);
    }
    // And a document with them still reads back, as does one without.
    let mut value = json(SessionStatus::Idle);
    let back: Session = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(back.handle.native_id, "abc-123");
    let object = value.as_object_mut().unwrap();
    object.remove("id");
    object.remove("state");
    assert!(serde_json::from_value::<Session>(value).is_ok(), "a 0.15 document is still a Session");
}

fn keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    keys
}

#[test]
fn search_hit_keeps_a_string_status_and_gains_a_state() {
    let hit = asm_core::index::SearchHit {
        agent: "claude-code".into(),
        native_id: "abc-123".into(),
        title: None,
        project_root: Some("/t/project".into()),
        parent: None,
        role: "user".into(),
        seq: 4,
        timestamp: None,
        snippet: "a \u{1}hit\u{2}".into(),
        score: -1.5,
        status: "archived".into(),
        state: "archived".into(),
    };
    let value = serde_json::to_value(&hit).unwrap();
    assert_eq!(
        keys(&value),
        ["agent", "native_id", "parent", "project_root", "role", "score", "seq", "snippet", "state", "status", "timestamp", "title"]
    );
    // The bare string is the old field and keeps its type; `select(.status ==
    // "live")` is wrong on a Session and right here, and `.state` is right on both.
    assert_eq!(value["status"], "archived");
    assert_eq!(value["state"], "archived");
}

#[test]
fn project_shape_is_pinned() {
    let project = asm_core::model::Project {
        root: "/t/project".into(),
        repo: None,
        agents: vec![AgentKind::ClaudeCode, AgentKind::OpenCode],
        worktrees: vec![asm_core::model::ProjectWorktree {
            path: "/t/project".into(),
            branch: Some("main".into()),
            is_main: true,
            session_count: 2,
        }],
        session_count: 2,
        size_bytes: 10,
        last_updated: None,
    };
    let value = serde_json::to_value(&project).unwrap();
    assert_eq!(keys(&value), ["agents", "last_updated", "repo", "root", "session_count", "size_bytes", "worktrees"]);
    assert_eq!(value["agents"], serde_json::json!(["claude-code", "opencode"]));
    assert_eq!(keys(&value["worktrees"][0]), ["branch", "is_main", "path", "session_count"]);
}

#[test]
fn doctor_report_shape_is_pinned() {
    let report = asm_core::ops::DoctorReport {
        agents: vec![asm_core::ops::AgentHealth {
            agent: AgentKind::OpenCode,
            store_root: "/t/opencode.db".into(),
            store_found: true,
            session_count: 3,
            duplicate_session_ids: vec![],
            warnings: vec!["w".into()],
            capabilities: Default::default(),
            schema: Some("v2"),
            unmigrated: Some(false),
        }],
        warnings: vec![],
    };
    let value = serde_json::to_value(&report).unwrap();
    assert_eq!(keys(&value), ["agents", "warnings"]);
    assert_eq!(
        keys(&value["agents"][0]),
        ["agent", "capabilities", "duplicate_session_ids", "schema", "session_count", "store_found", "store_root", "unmigrated", "warnings"]
    );
    assert_eq!(
        keys(&value["agents"][0]["capabilities"]),
        [
            "archive", "delete", "export_ir", "import_ir", "list", "liveness", "read_transcript", "relocate", "rename",
            "resume_native", "send_message"
        ]
    );
    assert_eq!(value["agents"][0]["agent"], "opencode");
}

#[test]
fn import_outcome_shape_is_pinned() {
    let outcome = asm_core::import::ImportOutcome {
        mode: asm_core::import::ImportMode::Full,
        target: None,
        in_sync: false,
        dry_run_artifact: None,
        resume_hint: None,
        warnings: vec![],
        loss: Default::default(),
    };
    let value = serde_json::to_value(&outcome).unwrap();
    assert_eq!(keys(&value), ["dry_run_artifact", "in_sync", "loss", "mode", "resume_hint", "target", "warnings"]);
    assert_eq!(
        keys(&value["loss"]),
        [
            "agent_transcripts_dropped", "extension_classes_dropped", "messages_in", "messages_out", "notes",
            "reasoning_dropped", "tools_kept_unmapped", "tools_mapped"
        ]
    );
}

#[test]
fn capabilities_shape_is_pinned() {
    let report = asm_core::ops::CapabilitiesReport {
        asm_version: "0.0.0",
        json_contract_version: asm_core::contract::JSON_CONTRACT_VERSION,
        ir_version: asm_core::ir::IR_VERSION,
        index_schema_version: asm_core::index::SCHEMA_VERSION,
        agents: vec![asm_core::ops::AgentCapabilities {
            agent: AgentKind::Codex,
            installed: false,
            store_root: None,
            capabilities: Default::default(),
            hub_sync: true,
            schema: None,
            unmigrated: None,
        }],
    };
    let value = serde_json::to_value(&report).unwrap();
    assert_eq!(keys(&value), ["agents", "asm_version", "index_schema_version", "ir_version", "json_contract_version"]);
    assert_eq!(value["json_contract_version"], 1, "bump only per the rules in reference/json");
    assert_eq!(
        keys(&value["agents"][0]),
        ["agent", "capabilities", "hub_sync", "installed", "schema", "store_root", "unmigrated"]
    );
    assert_eq!(value["agents"][0]["schema"], Value::Null, "null, not absent, for an agent with no schema");
}

#[test]
fn error_envelope_shape_is_pinned() {
    use asm_core::contract::{ErrorBody, ErrorCode};
    let plain = ErrorBody::new(ErrorCode::LiveSession, "in use").envelope();
    assert_eq!(plain, serde_json::json!({"error": {"code": "live_session", "message": "in use"}}));
    let ambiguous = ErrorBody { candidates: Some(vec!["codex:a".into()]), ..ErrorBody::new(ErrorCode::Ambiguous, "m") };
    assert_eq!(ambiguous.envelope()["error"]["candidates"], serde_json::json!(["codex:a"]));
    // The whole vocabulary, so a rename cannot slip by.
    let all = [
        ErrorCode::NotFound, ErrorCode::Ambiguous, ErrorCode::LiveSession, ErrorCode::Unsupported, ErrorCode::Io,
        ErrorCode::Usage, ErrorCode::Conflict, ErrorCode::Unreachable, ErrorCode::Unauthorized, ErrorCode::Internal,
    ];
    let words: Vec<String> = all.iter().map(|c| serde_json::to_value(c).unwrap().as_str().unwrap().to_string()).collect();
    assert_eq!(
        words,
        ["not_found", "ambiguous", "live_session", "unsupported", "io", "usage", "conflict", "unreachable", "unauthorized", "internal"]
    );
}

#[test]
fn message_rows_are_index_role_ts_parts() {
    use asm_core::ir::{IrMessage, IrPart, IrProvenance, IrRole, IrSession, PortablePath};
    use asm_core::ops::{MessageQuery, messages_from_ir};
    let message = |role, parts| IrMessage { role, timestamp: None, parts, source_id: None, extensions: Default::default() };
    let ir = IrSession {
        ir_version: 1,
        source: IrProvenance {
            agent: AgentKind::ClaudeCode,
            native_id: "abc".into(),
            agent_version: None,
            exported_at: jiff::Timestamp::UNIX_EPOCH,
            exporter_version: "0".into(),
        },
        title: None,
        slug: None,
        project_path: PortablePath("/t".into()),
        created: None,
        updated: None,
        model: None,
        usage: Usage::default(),
        messages: vec![
            message(IrRole::User, vec![IrPart::Text { text: "hi".into() }]),
            message(
                IrRole::Assistant,
                vec![
                    IrPart::Text { text: "ls".into() },
                    IrPart::ToolCall { call_id: "c".into(), name: "bash".into(), input: serde_json::json!({}) },
                ],
            ),
            message(IrRole::User, vec![IrPart::ToolResult { call_id: "c".into(), output: "x".into(), is_error: false, truncated: false }]),
            message(IrRole::Assistant, vec![IrPart::Text { text: "done".into() }]),
        ],
        extensions: Default::default(),
    };
    let rows = messages_from_ir(&ir, &MessageQuery::default());
    let value = serde_json::to_value(&rows[1]).unwrap();
    assert_eq!(keys(&value), ["index", "parts", "role"], "ts and source_id are left out when there are none");
    assert_eq!(value["role"], "assistant");
    assert_eq!(value["parts"][1]["type"], "tool_call");

    let of = |q: MessageQuery| messages_from_ir(&ir, &q).iter().map(|r| r.index).collect::<Vec<_>>();
    assert_eq!(of(MessageQuery { role: Some("user".into()), ..Default::default() }), [0, 2]);
    assert_eq!(of(MessageQuery { role: Some("tool".into()), ..Default::default() }), [1, 2]);
    assert_eq!(of(MessageQuery { text_only: true, ..Default::default() }), [0, 1, 3], "a message with no text is dropped");
    assert_eq!(of(MessageQuery { tail: Some(2), ..Default::default() }), [2, 3]);
    assert_eq!(of(MessageQuery { range: Some((1, Some(3))), ..Default::default() }), [1, 2]);
    assert_eq!(of(MessageQuery { range: Some((2, None)), tail: Some(1), ..Default::default() }), [3]);
    // The index is the transcript's, not the filtered list's.
    let text = messages_from_ir(&ir, &MessageQuery { text_only: true, ..Default::default() });
    assert_eq!(text[1].parts.len(), 1, "the tool call is gone from the assistant's message");
    assert_eq!(MessageQuery::parse_range("2..5"), Ok((2, Some(5))));
    assert_eq!(MessageQuery::parse_range("..5"), Ok((0, Some(5))));
    assert_eq!(MessageQuery::parse_range("3.."), Ok((3, None)));
    assert!(MessageQuery::parse_range("3").is_err() && MessageQuery::parse_range("a..b").is_err());
}

#[test]
fn agent_names_match_their_cli_spelling() {
    // Must agree with Display, AgentKind::parse and the `--agent` flag —
    // kebab-case derive would have made this "open-code".
    assert_eq!(serde_json::to_value(AgentKind::OpenCode).unwrap(), "opencode");
    assert_eq!(serde_json::to_value(AgentKind::ClaudeCode).unwrap(), "claude-code");
    assert_eq!(AgentKind::OpenCode.to_string(), "opencode");
    assert_eq!(AgentKind::parse("opencode"), Some(AgentKind::OpenCode));
    // Tolerated on the way in, never emitted.
    assert_eq!(AgentKind::parse("open-code"), Some(AgentKind::OpenCode));
}

#[test]
fn status_is_an_internally_tagged_object() {
    // Consumers must read status.state, not compare status to a string.
    assert_eq!(json(SessionStatus::Idle)["status"], serde_json::json!({"state": "idle"}));
    assert_eq!(
        json(SessionStatus::Archived)["status"],
        serde_json::json!({"state": "archived"})
    );
    assert_eq!(
        json(SessionStatus::Live { pid: Some(42) })["status"],
        serde_json::json!({"state": "live", "pid": 42})
    );
}

#[test]
fn location_is_tagged_by_kind() {
    let value = json(SessionStatus::Idle);
    assert_eq!(value["ref"]["location"]["kind"], "jsonl_file");
    assert_eq!(value["ref"]["location"]["path"], "/t/abc-123.jsonl");
}

#[test]
fn session_round_trips_through_json() {
    let original = sample(SessionStatus::Live { pid: Some(7) });
    let text = serde_json::to_string(&original).unwrap();
    let back: Session = serde_json::from_str(&text).unwrap();
    assert_eq!(back.handle.native_id, original.handle.native_id);
    assert_eq!(back.status, original.status);
    assert_eq!(back.handle.agent, original.handle.agent);
}
