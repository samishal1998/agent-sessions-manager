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
