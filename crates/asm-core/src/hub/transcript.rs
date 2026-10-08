//! A stored session's conversation, rendered on the hub without touching any
//! real agent store.
//!
//! The head bundle is restored into a scratch directory that is removed
//! afterwards, and the restored session is exported to IR from there. Claude
//! Code, jcode and Codex use their own install function (read and found to
//! write inside the adapter root only, no env, no child process); Antigravity's
//! transcript is placed directly; OpenCode's bundled rows (1.x) or session
//! documents (2.x) are read as data, because its install runs `opencode
//! import`. Nothing leaves the scratch dir.

use std::path::PathBuf;

use serde::Serialize;

use super::actions::Scratch;
use super::store::{Hub, HubError};
use crate::CoreError;
use crate::adapter::antigravity::AntigravityAdapter;
use crate::adapter::claude::ClaudeAdapter;
use crate::adapter::codex::CodexAdapter;
use crate::adapter::jcode::JCodeAdapter;
use crate::adapter::{AgentRead, SessionFilter};
use crate::ir::{IrSession, PortablePath};
use crate::model::AgentKind;

/// Largest transcript the hub will restore to render.
const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// `{ available: true, truncated, ir }` or `{ available: false, reason }`.
#[derive(Debug, Serialize)]
pub struct TranscriptView {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ir: Option<IrSession>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

fn unavailable(reason: impl Into<String>) -> TranscriptView {
    TranscriptView { available: false, truncated: None, ir: None, reason: Some(reason.into()) }
}

/// Render the head of `(agent, id)`, keeping the last `limit` messages.
/// `NotFound` when the hub has no such session.
pub fn render(hub: &Hub, agent: &str, id: &str, limit: usize) -> Result<TranscriptView, HubError> {
    let history = hub.history(agent, id)?;
    let mut manifest = history.manifest;

    // What carries the conversation, per agent. Sidecars add nothing to it.
    // Each agent's own install is only used where it was read end to end
    // and found to write inside the adapter root alone, with no process or
    // process-global state: claude-code, jcode and codex. Antigravity's
    // files are placed directly (its install would copy a database that is
    // not read here); opencode's install runs `opencode import`, so its
    // bundled rows are read as data instead.
    let keep = match manifest.agent {
        AgentKind::ClaudeCode => "transcript.jsonl",
        AgentKind::JCode => "snapshot.json",
        AgentKind::Codex => "rollout.jsonl",
        AgentKind::Antigravity => ANTIGRAVITY_TRANSCRIPT,
        // 1.x bundles hold database rows, 2.x ones session documents.
        AgentKind::OpenCode if manifest.files.iter().any(|f| f.path == V2_TRANSCRIPT) => V2_TRANSCRIPT,
        AgentKind::OpenCode => "rows.json",
    };
    manifest.files.retain(|f| f.path == keep);
    if manifest.files.is_empty() {
        return Ok(unavailable("This session has no transcript on the hub."));
    }
    if manifest.files.iter().map(|f| f.size).sum::<u64>() > MAX_BYTES {
        return Ok(unavailable("This session is too large to render on the hub (over 64 MiB)."));
    }

    // Removed on drop, so on every exit including errors.
    // Inside the hub's own root, so a test can see it is gone.
    let dir = hub.root().join(format!("render-{}", super::store::random_hex(8)?));
    let scratch = Scratch(dir);
    let project = scratch.0.join("project");
    std::fs::create_dir_all(&project).map_err(|e| CoreError::io(&project, e))?;
    let blob = |sha: &str| hub.blob(sha).map_err(|e| CoreError::Invalid { msg: e.to_string() });
    let blob_of = |name: &str| -> Result<PathBuf, CoreError> {
        let sha = manifest.files.iter().find(|f| f.path == name).and_then(|f| f.sha256.as_deref());
        blob(sha.ok_or_else(|| CoreError::Invalid { msg: format!("bundle has no {name}") })?)
    };

    let mut ir = match manifest.agent {
        AgentKind::OpenCode if keep == V2_TRANSCRIPT => {
            let path = blob_of(V2_TRANSCRIPT)?;
            let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).map_err(|e| CoreError::io(&path, e))?)
                .map_err(|e| CoreError::Invalid { msg: format!("{V2_TRANSCRIPT} is unreadable: {e}") })?;
            let tree = doc.pointer(&format!("/sessions/{id}")).filter(|t| t.is_object());
            let tree = tree.ok_or_else(|| CoreError::Invalid { msg: format!("{V2_TRANSCRIPT} has no such session") })?;
            crate::adapter::opencode::v2::ir_from_transfer(tree)
        }
        AgentKind::OpenCode => {
            let path = blob_of("rows.json")?;
            let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).map_err(|e| CoreError::io(&path, e))?)
                .map_err(|e| CoreError::Invalid { msg: format!("rows.json is unreadable: {e}") })?;
            let tree = doc.pointer(&format!("/sessions/{id}")).filter(|t| t.is_object());
            let tree = tree.ok_or_else(|| CoreError::Invalid { msg: "rows.json has no such session".into() })?;
            crate::adapter::opencode::export_ir::from_dump(id, tree)
        }
        AgentKind::Antigravity => {
            let adapter = AntigravityAdapter::with_root(scratch.0.join("agy"));
            let src = blob_of(ANTIGRAVITY_TRANSCRIPT)?;
            let raw = std::fs::read(&src).map_err(|e| CoreError::io(&src, e))?;
            let transcript = adapter.transcript_of(id);
            let db = adapter.conversations_dir().join(format!("{id}.db"));
            for (path, bytes) in [(&transcript, super::bundle::complete_lines(&raw)), (&db, &[][..])] {
                let parent = path.parent().unwrap_or(&scratch.0);
                std::fs::create_dir_all(parent).map_err(|e| CoreError::io(parent, e))?;
                std::fs::write(path, bytes).map_err(|e| CoreError::io(path, e))?;
            }
            export_restored(&adapter, id)?
        }
        AgentKind::ClaudeCode => {
            let adapter = ClaudeAdapter::with_root(scratch.0.join("claude"));
            crate::adapter::claude::hub::install(&adapter, &manifest, &blob, Some(&project), None)?;
            export_restored(&adapter, id)?
        }
        AgentKind::JCode => {
            let adapter = JCodeAdapter::with_root(scratch.0.join("jcode"));
            crate::adapter::jcode::hub::install(&adapter, &manifest, &blob, Some(&project), None)?;
            export_restored(&adapter, id)?
        }
        AgentKind::Codex => {
            // Codex's install wants the project directory to exist and
            // takes its writer lock under the root it is given: a clone of
            // the manifest points at a directory made here, the stored one
            // is untouched.
            let adapter = CodexAdapter::with_root(scratch.0.join("codex"));
            let mut here = manifest.clone();
            here.project_root = project.display().to_string();
            here.project_root_portable = here.project_root.clone();
            crate::adapter::codex::hub::install(&adapter, &here, &blob, Some(&project), None)?;
            export_restored(&adapter, id)?
        }
    };
    ir.project_path = PortablePath(manifest.project_root_portable.clone());

    let truncated = ir.messages.len() > limit;
    if truncated {
        ir.messages.drain(..ir.messages.len() - limit);
    }
    Ok(TranscriptView { available: true, truncated: Some(truncated), ir: Some(ir), reason: None })
}

/// An OpenCode 2.x bundle's file: `{sessions: {<id>: {info, messages}}}`.
const V2_TRANSCRIPT: &str = "transfer.json";

const ANTIGRAVITY_TRANSCRIPT: &str = "brain/.system_generated/logs/transcript.jsonl";

/// The session `id` as the restored store lists it, exported to IR.
fn export_restored(adapter: &dyn AgentRead, id: &str) -> Result<IrSession, CoreError> {
    let session = adapter
        .sessions(&SessionFilter::default())?
        .into_iter()
        .find(|s| s.handle.native_id == id)
        .ok_or_else(|| CoreError::Invalid { msg: "the restored session could not be read".into() })?;
    adapter.export_ir(&session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::sha256_hex;
    use serde_json::json;

    const ID: &str = "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43";

    fn rec(kind: &str, i: usize, content: serde_json::Value) -> String {
        format!(
            "{}\n",
            json!({
                "type": kind, "cwd": "/home/a/x", "sessionId": ID, "uuid": format!("u{i}"),
                "parentUuid": i.checked_sub(1).map(|p| format!("u{p}")),
                "timestamp": format!("2026-09-22T10:00:{i:02}Z"),
                "message": { "role": kind, "content": content },
            })
        )
    }

    /// A hub holding one session of `agent` whose transcript has `n` messages.
    pub(crate) fn pushed(agent: &str, n: usize) -> (tempfile::TempDir, Hub) {
        let dir = tempfile::tempdir().unwrap();
        let hub = Hub::open(&dir.path().join("hub")).unwrap();
        let joined = hub.join(&hub.join_token().unwrap(), "laptop").unwrap();
        let text: String = (0..n)
            .map(|i| match i % 2 {
                0 => rec("user", i, json!(format!("question {i}"))),
                _ => rec("assistant", i, json!([{ "type": "text", "text": format!("answer {i}") }])),
            })
            .collect();
        let sha = sha256_hex(text.as_bytes());
        hub.put_blob(&sha, text.as_bytes(), 1 << 20).unwrap();
        let manifest = json!({
            "schema": 1, "agent": agent, "id": ID,
            "project_root": "/home/a/x", "project_root_portable": "${HOME}/x",
            "canonical": sha, "parent_rev": null,
            "files": [{ "path": if agent == "claude-code" { "transcript.jsonl" } else { "rollout.jsonl" },
                        "sha256": sha, "size": text.len() }],
        });
        hub.put_revision(agent, ID, manifest.to_string().as_bytes(), &joined.machine).unwrap();
        (dir, hub)
    }

    fn scratch_dirs(hub: &Hub) -> usize {
        std::fs::read_dir(hub.root())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("render-"))
            .count()
    }

    #[test]
    fn renders_a_claude_session_and_cleans_up() {
        let (_d, hub) = pushed("claude-code", 4);
        let v = render(&hub, "claude-code", ID, 2000).unwrap();
        assert!(v.available && v.truncated == Some(false));
        let ir = v.ir.unwrap();
        assert_eq!(ir.messages.len(), 4);
        assert_eq!(ir.project_path.0, "${HOME}/x", "not the scratch path");
        let json = serde_json::to_string(&ir).unwrap();
        assert!(json.contains("question 0") && json.contains("answer 3"));
        assert_eq!(scratch_dirs(&hub), 0, "scratch dir removed");
    }

    #[test]
    fn keeps_only_the_most_recent_messages() {
        let (_d, hub) = pushed("claude-code", 6);
        let v = render(&hub, "claude-code", ID, 2).unwrap();
        assert_eq!(v.truncated, Some(true));
        let json = serde_json::to_string(&v.ir.unwrap()).unwrap();
        assert!(json.contains("answer 5") && json.contains("question 4") && !json.contains("answer 3"));
    }

    #[test]
    fn a_missing_session_and_a_bundle_without_the_transcript() {
        let (_d, hub) = pushed("claude-code", 2);
        assert!(matches!(render(&hub, "codex", ID, 10), Err(HubError::NotFound)));
        let (_d, hub) = pushed("codex", 2); // pushed with rollout.jsonl under the claude shape: no rollout_rel
        assert!(render(&hub, "codex", ID, 10).is_err(), "no rollout_rel, no restore");
        assert_eq!(scratch_dirs(&hub), 0);
    }

    // ---- the other agents, with bundles laid out as each collect() does ----

    /// Every file under `dir`, with its size, for before/after comparison.
    fn listing(dir: &std::path::Path) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                let rel = p.strip_prefix(dir).unwrap().display().to_string();
                if p.is_dir() {
                    out.push((format!("{rel}/"), 0));
                    stack.push(p);
                } else {
                    out.push((rel, e.metadata().unwrap().len()));
                }
            }
        }
        out.sort();
        out
    }

    /// A hub with one `agent` session made of `files`, and a sibling dir
    /// standing in for the machine's own home, all inside one temp parent.
    fn hub_with(agent: &str, extra: serde_json::Value, files: &[(&str, Vec<u8>)]) -> (tempfile::TempDir, Hub) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("home")).unwrap();
        let hub = Hub::open(&dir.path().join("hub")).unwrap();
        let joined = hub.join(&hub.join_token().unwrap(), "laptop").unwrap();
        let mut entries = Vec::new();
        for (path, bytes) in files {
            let sha = sha256_hex(bytes);
            hub.put_blob(&sha, &bytes[..], 1 << 20).unwrap();
            entries.push(json!({ "path": path, "sha256": sha, "size": bytes.len() }));
        }
        let manifest = json!({
            "schema": 1, "agent": agent, "id": ID,
            "project_root": "/home/a/x", "project_root_portable": "${HOME}/x",
            "canonical": sha256_hex(b"c"), "parent_rev": null, "files": entries, "extra": extra,
        });
        hub.put_revision(agent, ID, manifest.to_string().as_bytes(), &joined.machine).unwrap();
        (dir, hub)
    }

    /// Render, and prove the scratch dir is gone and nothing else moved.
    fn render_clean(dir: &tempfile::TempDir, hub: &Hub, agent: &str) -> crate::ir::IrSession {
        let before = listing(dir.path());
        let v = render(hub, agent, ID, 2000).unwrap();
        assert!(v.available && v.truncated == Some(false), "{:?}", v.reason);
        assert_eq!(scratch_dirs(hub), 0, "scratch dir removed");
        assert_eq!(listing(dir.path()), before, "nothing written outside the scratch dir");
        let ir = v.ir.unwrap();
        assert_eq!(ir.project_path.0, "${HOME}/x", "not the scratch path");
        ir
    }

    fn texts(ir: &crate::ir::IrSession) -> String {
        serde_json::to_string(&ir.messages).unwrap()
    }

    #[test]
    fn jcode_renders_from_its_snapshot() {
        let jid = ID; // jcode ids are free-form; the hub's id check is what matters
        let snap = json!({
            "id": jid, "short_name": "otter", "created_at": "2026-09-22T10:00:00Z",
            "working_dir": "/home/a/x", "last_pid": 4242,
            "messages": [
                { "id": "m0", "role": "user", "content": [{ "type": "text", "text": "question zero" }] },
                { "id": "m1", "role": "assistant", "content": [
                    { "type": "text", "text": "answer one" },
                    { "type": "tool_use", "id": "t1", "name": "bash", "input": { "cmd": "ls" } } ] },
            ],
        });
        // The journal and the backup do not travel into the render.
        let (d, hub) = hub_with("jcode", json!(null), &[
            ("snapshot.json", serde_json::to_vec(&snap).unwrap()),
            ("snapshot.bak", b"{}".to_vec()),
        ]);
        let ir = render_clean(&d, &hub, "jcode");
        assert_eq!(ir.messages.len(), 2);
        let t = texts(&ir);
        assert!(t.contains("question zero") && t.contains("answer one") && t.contains("bash"), "{t}");
    }

    #[test]
    fn codex_renders_without_the_project_existing_here() {
        let rollout = [
            json!({ "timestamp": "2026-09-22T10:00:00Z", "type": "session_meta",
                    "payload": { "id": ID, "cwd": "/home/a/x", "timestamp": "2026-09-22T10:00:00Z", "cli_version": "0.151.0" } }),
            json!({ "timestamp": "2026-09-22T10:00:01Z", "type": "response_item",
                    "payload": { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "question zero" }] } }),
            json!({ "timestamp": "2026-09-22T10:00:02Z", "type": "response_item",
                    "payload": { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "answer one" }] } }),
        ]
        .iter()
        .map(|l| format!("{l}\n"))
        .collect::<String>();
        let name = format!("rollout-2026-09-22T10-00-00-{ID}.jsonl");
        let (d, hub) = hub_with(
            "codex",
            json!({ "rollout_rel": format!("sessions/2026/09/22/{name}") }),
            &[("rollout.jsonl", rollout.into_bytes()), ("thread.json", br#"{"id":"x"}"#.to_vec())],
        );
        // /home/a/x does not exist here; the clone of the manifest points into the scratch dir.
        assert!(!std::path::Path::new("/home/a/x").exists());
        let ir = render_clean(&d, &hub, "codex");
        let t = texts(&ir);
        assert!(t.contains("question zero") && t.contains("answer one"), "{t}");
        // The stored manifest still names the pushing machine's directory.
        assert_eq!(hub.history("codex", ID).unwrap().manifest.project_root, "/home/a/x");
    }

    #[test]
    fn antigravity_renders_from_its_transcript() {
        let lines = [
            json!({ "step_index": 0, "type": "USER_INPUT", "created_at": "2026-09-22T10:00:00Z",
                    "content": "<USER_REQUEST>question zero</USER_REQUEST>" }),
            json!({ "step_index": 1, "type": "PLANNER_RESPONSE", "created_at": "2026-09-22T10:00:01Z", "content": "answer one" }),
        ]
        .iter()
        .map(|l| format!("{l}\n"))
        .collect::<String>();
        // A torn last line is dropped, as install does; the database is never needed.
        let torn = format!("{lines}{{\"step_index\":2,\"ty");
        let (d, hub) = hub_with(
            "antigravity",
            json!(null),
            &[(ANTIGRAVITY_TRANSCRIPT, torn.into_bytes()), ("conversation.db", vec![0u8; 4096])],
        );
        let ir = render_clean(&d, &hub, "antigravity");
        assert_eq!(ir.messages.len(), 2);
        let t = texts(&ir);
        assert!(t.contains("question zero") && !t.contains("USER_REQUEST") && t.contains("answer one"), "{t}");
    }

    #[test]
    fn opencode_renders_from_its_rows_without_a_database_or_the_binary() {
        let rows = json!({ "root": ID, "sessions": { ID: {
            "session": [{ "id": ID, "directory": "/home/a/x", "title": "Retry schema", "slug": "calm-otter",
                          "version": "1.18.31", "time_created": 1, "time_updated": 50, "model": "{\"id\":\"m\"}" }],
            "message": [
                { "id": "msg_2", "session_id": ID, "time_created": 20, "time_updated": 20, "data": "{\"role\":\"assistant\"}" },
                { "id": "msg_1", "session_id": ID, "time_created": 10, "time_updated": 10, "data": "{\"role\":\"user\"}" },
            ],
            "part": [
                { "id": "prt_2", "message_id": "msg_2", "session_id": ID, "time_created": 20, "time_updated": 20,
                  "data": "{\"type\":\"text\",\"text\":\"answer one\"}" },
                { "id": "prt_1", "message_id": "msg_1", "session_id": ID, "time_created": 10, "time_updated": 10,
                  "data": "{\"type\":\"text\",\"text\":\"question zero\"}" },
            ],
        } } });
        let (d, hub) = hub_with("opencode", json!(null), &[("rows.json", serde_json::to_vec(&rows).unwrap())]);
        let ir = render_clean(&d, &hub, "opencode");
        assert_eq!(ir.messages.len(), 2);
        assert_eq!(ir.title.as_deref(), Some("Retry schema"));
        let t = texts(&ir);
        assert!(t.find("question zero").unwrap() < t.find("answer one").unwrap(), "ordered by time: {t}");
    }

    #[test]
    fn opencode_2x_renders_from_its_session_documents() {
        let doc = json!({ "generation": "opencode-v2", "root": ID, "sessions": { ID: {
            "info": { "id": ID, "title": "Two point oh", "location": { "directory": "/home/a/x" },
                      "model": { "id": "m", "providerID": "p" }, "time": { "created": 1, "updated": 2 } },
            "messages": [
                { "id": "msg_1", "type": "user", "time": { "created": 10 }, "text": "question zero" },
                { "id": "msg_2", "type": "assistant", "time": { "created": 20, "completed": 21 }, "agent": "build",
                  "content": [ { "type": "text", "text": "answer one" },
                               { "type": "tool", "id": "c1", "name": "shell", "time": { "created": 20 },
                                 "state": { "status": "completed", "input": { "command": "ls" }, "content": [{ "type": "text", "text": "a.txt" }] } } ] },
                { "id": "msg_3", "type": "idle", "time": { "created": 30 }, "outcome": "succeeded" },
            ],
        } } });
        let (d, hub) = hub_with(
            "opencode",
            json!({ "generation": "opencode-v2" }),
            &[("transfer.json", serde_json::to_vec(&doc).unwrap())],
        );
        let ir = render_clean(&d, &hub, "opencode");
        assert_eq!(ir.messages.len(), 2, "the idle marker is not a message");
        assert_eq!(ir.title.as_deref(), Some("Two point oh"));
        let t = texts(&ir);
        assert!(t.find("question zero").unwrap() < t.find("answer one").unwrap(), "{t}");
        assert!(ir.messages[1].parts.iter().any(|p| matches!(p, crate::ir::IrPart::ToolResult { output, .. } if output == "a.txt")));
    }
}
