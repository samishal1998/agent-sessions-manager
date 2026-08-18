//! Import-conversion tests via dry runs: IR → OpenCode export document and
//! IR → Claude transcript, validated structurally against the rules the
//! real agents enforce (no parentID on root sessions, tool call/result
//! merging, record pairing, deterministic ids).

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use asm_core::adapter::claude::ClaudeAdapter;
use asm_core::adapter::opencode::OpenCodeAdapter;
use asm_core::adapter::AgentWrite;
use asm_core::import::{ImportMode, ImportOpts};
use asm_core::ir::{ExtBag, IrMessage, IrPart, IrProvenance, IrRole, IrSession, PortablePath};
use asm_core::model::{AgentKind, Usage};

fn sample_ir(source: AgentKind) -> IrSession {
    let ts = |s: &str| s.parse::<jiff::Timestamp>().ok();
    IrSession {
        ir_version: 1,
        source: IrProvenance {
            agent: source,
            native_id: match source {
                AgentKind::ClaudeCode => "aaaaaaaa-1111-2222-3333-444444444444".into(),
                AgentKind::OpenCode => "ses_source0000000000000000000".into(),
                _ => unreachable!(),
            },
            agent_version: Some("x".into()),
            exported_at: jiff::Timestamp::UNIX_EPOCH,
            exporter_version: "test".into(),
        },
        title: Some("Sample".into()),
        slug: Some("sample-slug".into()),
        project_path: PortablePath("/tmp/does-not-need-to-exist".into()),
        created: ts("2026-08-01T10:00:00Z"),
        updated: ts("2026-08-01T11:00:00Z"),
        model: Some("test-model".into()),
        usage: Usage::default(),
        messages: vec![
            IrMessage {
                role: IrRole::User,
                timestamp: ts("2026-08-01T10:00:00Z"),
                parts: vec![IrPart::Text { text: "please list files".into() }],
                source_id: Some("u1".into()),
                extensions: ExtBag::new(),
            },
            IrMessage {
                role: IrRole::Assistant,
                timestamp: ts("2026-08-01T10:00:05Z"),
                parts: vec![
                    IrPart::Reasoning { summary: "I should run ls".into(), opaque: true },
                    IrPart::Text { text: "Listing now.".into() },
                    IrPart::ToolCall {
                        call_id: "call_1".into(),
                        name: match source {
                            AgentKind::ClaudeCode => "Bash".into(),
                            AgentKind::OpenCode => "bash".into(),
                            _ => unreachable!(),
                        },
                        input: serde_json::json!({"command": "ls"}),
                    },
                    IrPart::ToolCall {
                        call_id: "call_2".into(),
                        name: "mcp__weird__tool".into(),
                        input: serde_json::json!({"x": 1}),
                    },
                ],
                source_id: Some("a1".into()),
                extensions: ExtBag::new(),
            },
            IrMessage {
                role: IrRole::User,
                timestamp: ts("2026-08-01T10:00:10Z"),
                parts: vec![
                    IrPart::ToolResult {
                        call_id: "call_1".into(),
                        output: "a.txt\nb.txt".into(),
                        is_error: false,
                        truncated: false,
                    },
                    IrPart::ToolResult {
                        call_id: "call_2".into(),
                        output: "boom".into(),
                        is_error: true,
                        truncated: false,
                    },
                ],
                source_id: Some("u2".into()),
                extensions: ExtBag::new(),
            },
            IrMessage {
                role: IrRole::Assistant,
                timestamp: ts("2026-08-01T10:00:15Z"),
                parts: vec![IrPart::Text { text: "Two files found.".into() }],
                source_id: Some("a2".into()),
                extensions: ExtBag::new(),
            },
        ],
        extensions: ExtBag::new(),
    }
}

fn dry_opts(mode: ImportMode) -> ImportOpts {
    ImportOpts { mode, project: None, dry_run: true }
}

#[test]
fn claude_to_opencode_document_shape() {
    let ir = sample_ir(AgentKind::ClaudeCode);
    let adapter = OpenCodeAdapter::with_db("/nonexistent/opencode.db");
    let outcome = adapter.import_ir(&ir, &dry_opts(ImportMode::Full)).unwrap();
    let doc: Value = serde_json::from_str(outcome.dry_run_artifact.as_deref().unwrap()).unwrap();

    // Root session: no parentID, required fields present.
    let info = &doc["info"];
    assert!(info.get("parentID").is_none(), "parentID would hide the session from the picker");
    assert_eq!(info["title"], "Sample");
    assert!(info["id"].as_str().unwrap().starts_with("ses_"));
    assert!(info["time"]["created"].is_i64());

    let messages = doc["messages"].as_array().unwrap();
    // The tool_result-only user message is consumed by the merge.
    assert_eq!(messages.len(), 3);

    let assistant = &messages[1];
    assert_eq!(assistant["info"]["role"], "assistant");
    assert!(assistant["info"]["parentID"].is_string());
    assert!(assistant["info"]["tokens"]["cache"]["read"].is_i64());

    let parts = assistant["parts"].as_array().unwrap();
    // reasoning + text + two merged tool parts
    let tools: Vec<&Value> =
        parts.iter().filter(|p| p["type"] == "tool").collect();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["tool"], "bash", "Bash mapped to opencode's bash");
    assert_eq!(tools[0]["state"]["status"], "completed");
    assert_eq!(tools[0]["state"]["output"], "a.txt\nb.txt");
    assert_eq!(tools[1]["tool"], "mcp__weird__tool", "unmapped names kept verbatim");
    assert_eq!(tools[1]["state"]["status"], "error");

    // Part ids sort in document order (OpenCode orders by id).
    let mut ids: Vec<String> = Vec::new();
    for message in messages {
        for part in message["parts"].as_array().unwrap() {
            ids.push(part["id"].as_str().unwrap().to_string());
        }
    }
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);

    // Loss report accounting.
    assert_eq!(outcome.loss.reasoning_dropped, 1);
    assert_eq!(outcome.loss.tools_mapped, 1);
    assert_eq!(
        outcome.loss.tools_kept_unmapped,
        BTreeMap::from([("mcp__weird__tool".to_string(), 1)])
    );
}

#[test]
fn opencode_to_claude_transcript_shape() {
    let ir = sample_ir(AgentKind::OpenCode);
    let adapter = ClaudeAdapter::with_root("/nonexistent/claude-root");
    let outcome = adapter.import_ir(&ir, &dry_opts(ImportMode::Full)).unwrap();
    let transcript = outcome.dry_run_artifact.as_deref().unwrap();
    let records: Vec<Value> =
        transcript.lines().map(|l| serde_json::from_str(l).unwrap()).collect();

    // Deterministic session id everywhere; parentUuid chain is connected.
    let session_id = outcome.target.as_ref().unwrap().native_id.clone();
    let conversation: Vec<&Value> =
        records.iter().filter(|r| r["type"] == "user" || r["type"] == "assistant").collect();
    let mut previous: Option<String> = None;
    for record in &conversation {
        assert_eq!(record["sessionId"], session_id.as_str());
        match &previous {
            None => assert!(record["parentUuid"].is_null()),
            Some(p) => assert_eq!(record["parentUuid"], p.as_str()),
        }
        previous = Some(record["uuid"].as_str().unwrap().to_string());
    }

    // First record is the provenance meta message.
    assert_eq!(conversation[0]["isMeta"], true);

    // Assistant tool_use answered by tool_result in the NEXT user record.
    let assistant = conversation
        .iter()
        .find(|r| {
            r["type"] == "assistant"
                && r["message"]["content"]
                    .as_array()
                    .is_some_and(|c| c.iter().any(|b| b["type"] == "tool_use"))
        })
        .unwrap();
    let tool_use_ids: Vec<&str> = assistant["message"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .map(|b| b["id"].as_str().unwrap())
        .collect();
    assert_eq!(tool_use_ids, vec!["call_1", "call_2"]);
    // bash maps back to Claude's Bash.
    let names: Vec<&str> = assistant["message"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"Bash"));

    let assistant_index = conversation.iter().position(|r| std::ptr::eq(*r, *assistant)).unwrap();
    let next = conversation[assistant_index + 1];
    assert_eq!(next["type"], "user");
    let result_ids: Vec<&str> = next["message"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["type"] == "tool_result")
        .map(|b| b["tool_use_id"].as_str().unwrap())
        .collect();
    assert_eq!(result_ids, tool_use_ids);

    // Picker state records present.
    assert!(records.iter().any(|r| r["type"] == "ai-title" && r["aiTitle"] == "Sample"));
    let last_prompt = records.iter().find(|r| r["type"] == "last-prompt").unwrap();
    assert_eq!(last_prompt["leafUuid"], previous.unwrap().as_str());

    // Reasoning cannot cross into Claude (signature-bound).
    assert_eq!(outcome.loss.reasoning_dropped, 1);
}

#[test]
fn seed_mode_produces_handoff_document() {
    let ir = sample_ir(AgentKind::ClaudeCode);
    // Engine-level seed wrap is exercised via ops::import in the live
    // acceptance test; here we check the seed document itself renders.
    let doc = asm_core::import::seed::seed_document(&ir);
    assert!(doc.contains("# Imported session handoff"));
    assert!(doc.contains("please list files"));
    assert!(doc.contains("→ Bash: ls"));
    assert!(doc.contains("[ok, 2 lines]"));
    assert!(!doc.contains("I should run ls"), "reasoning stays internal");
}

#[test]
fn import_ids_are_deterministic_across_runs() {
    let ir = sample_ir(AgentKind::ClaudeCode);
    let adapter = OpenCodeAdapter::with_db("/nonexistent/opencode.db");
    let a = adapter.import_ir(&ir, &dry_opts(ImportMode::Full)).unwrap();
    let b = adapter.import_ir(&ir, &dry_opts(ImportMode::Full)).unwrap();
    assert_eq!(
        a.target.as_ref().unwrap().native_id,
        b.target.as_ref().unwrap().native_id
    );
    assert_eq!(a.dry_run_artifact, b.dry_run_artifact);
}

/// Build a store containing the deterministic target id, with or without
/// the message rows that prove the import actually completed.
fn store_with_target(db: &Path, ir: &IrSession, with_message: bool) -> String {
    let expected_id = {
        let adapter = OpenCodeAdapter::with_db(db);
        let dry = adapter.import_ir(ir, &dry_opts(ImportMode::Full)).unwrap();
        dry.target.unwrap().native_id
    };
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE session (id TEXT PRIMARY KEY);
         CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT);
         INSERT INTO session VALUES ('{expected_id}');"
    ))
    .unwrap();
    if with_message {
        conn.execute_batch(&format!(
            "INSERT INTO message VALUES ('msg_1', '{expected_id}');"
        ))
        .unwrap();
    }
    expected_id
}

#[test]
fn opencode_in_sync_when_the_session_was_actually_imported() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("opencode.db");
    let ir = sample_ir(AgentKind::ClaudeCode);
    store_with_target(&db, &ir, true);

    let adapter = OpenCodeAdapter::with_db(&db);
    let outcome = adapter
        .import_ir(&ir, &ImportOpts { mode: ImportMode::Full, project: None, dry_run: false })
        .unwrap();
    assert!(outcome.in_sync);
}

#[test]
fn a_bare_session_row_is_not_mistaken_for_a_completed_import() {
    // `opencode import` commits the session row before decoding parts, so a
    // failed import leaves one behind. Treating that as "in sync" would
    // permanently mask a half-written session.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("opencode.db");
    let ir = sample_ir(AgentKind::ClaudeCode);
    store_with_target(&db, &ir, false);

    let adapter = OpenCodeAdapter::with_db(&db);
    let result = adapter
        .import_ir(&ir, &ImportOpts { mode: ImportMode::Full, project: None, dry_run: false });

    // It must get past the idempotency check and attempt a real import
    // (which then fails here only because the fixture's project dir is
    // fictional) rather than silently reporting success.
    let err = result.unwrap_err().to_string();
    assert!(err.contains("project directory"), "{err}");
}

#[test]
fn claude_in_sync_when_transcript_exists_anywhere() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = ClaudeAdapter::with_root(dir.path());
    let ir = sample_ir(AgentKind::OpenCode);
    let expected_id = adapter
        .import_ir(&ir, &dry_opts(ImportMode::Full))
        .unwrap()
        .target
        .unwrap()
        .native_id;

    let other_project = dir.path().join("projects/-some-other-project");
    std::fs::create_dir_all(&other_project).unwrap();
    std::fs::write(other_project.join(format!("{expected_id}.jsonl")), "{}\n").unwrap();

    let outcome = adapter
        .import_ir(&ir, &ImportOpts { mode: ImportMode::Full, project: None, dry_run: false })
        .unwrap();
    assert!(outcome.in_sync, "existing transcript anywhere means in-sync, never a duplicate");
    assert!(!Path::new(&dir.path().join("projects")).join("imported").exists());
}
