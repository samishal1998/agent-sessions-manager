//! Live-session detection via Claude Code's PID-keyed records in
//! `<root>/sessions/<pid>.json` — `{pid, sessionId, cwd, status, ...}`.
//! These are liveness/IPC records: read-only, never move or copy them.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

/// Map of session id -> pid for sessions whose recorded process is alive.
pub(super) fn live_sessions(root: &Path) -> HashMap<String, u32> {
    let mut live = HashMap::new();
    let sessions_dir = root.join("sessions");
    let Ok(entries) = std::fs::read_dir(&sessions_dir) else {
        return live;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(record) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let (Some(pid), Some(session_id)) = (
            record.get("pid").and_then(Value::as_u64),
            record.get("sessionId").and_then(Value::as_str),
        ) else {
            continue;
        };
        let pid = pid as u32;
        if process_alive(pid) {
            live.insert(session_id.to_string(), pid);
        }
    }
    live
}

#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(not(target_os = "linux"))]
fn process_alive(_pid: u32) -> bool {
    // Conservative on non-Linux until a proper check lands: a stale PID
    // record then shows as Live, which only makes us refuse mutations.
    true
}
