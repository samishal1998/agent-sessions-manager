//! Read-only enumeration and metadata extraction for the Claude Code store.
//!
//! Transcripts reach tens of MB with single lines over 100 KB, and are
//! appended live — so listing never parses whole files. We stream a bounded
//! head (for the first conversation record) and a bounded tail (for
//! last-writer-wins state records like `ai-title`), tolerating torn lines.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;

use crate::CoreError;
use crate::model::{
    AgentKind, Project, Session, SessionLocation, SessionRef, SessionStatus, Usage,
};

use super::super::SessionFilter;
use super::{ClaudeAdapter, liveness};

/// How many head lines to scan for the first conversation record.
const MAX_HEAD_LINES: usize = 500;
/// Cap on a single head line; longer lines are skipped, not slurped.
const MAX_HEAD_LINE_BYTES: u64 = 4 * 1024 * 1024;
/// Tail window scanned for last-writer-wins state records.
const TAIL_BYTES: u64 = 256 * 1024;

pub(super) fn sessions(
    adapter: &ClaudeAdapter,
    filter: &SessionFilter,
) -> Result<Vec<Session>, CoreError> {
    if let Some(agent) = filter.agent
        && agent != AgentKind::ClaudeCode
    {
        return Ok(Vec::new());
    }

    let projects_dir = adapter.root().join("projects");
    if !projects_dir.is_dir() {
        return Err(CoreError::StoreNotFound { path: projects_dir });
    }

    let live = liveness::live_sessions(adapter.root());
    let rollups = adapter
        .global_config
        .as_deref()
        .map(read_usage_rollups)
        .unwrap_or_default();

    let mut sessions = Vec::new();
    for project_dir in read_dir_sorted(&projects_dir)? {
        if !project_dir.is_dir() {
            continue;
        }
        for path in read_dir_sorted(&project_dir)? {
            let Some(session_id) = transcript_session_id(&path) else {
                continue; // memory/, <uuid>/ sidecar dirs, non-UUID files
            };
            let Some(mut session) = scan_transcript(&path, &session_id) else {
                continue; // no conversation records at all — nothing to show
            };
            if let Some(pid) = live.get(&session_id) {
                session.status = SessionStatus::Live { pid: Some(*pid) };
            }
            if let Some(rollup) = rollups.get(&session_id) {
                session.usage = *rollup;
            }
            if filter.matches(&session) {
                sessions.push(session);
            }
        }
    }

    sessions.sort_by(|a, b| b.updated.cmp(&a.updated));
    Ok(sessions)
}

pub(super) fn projects(adapter: &ClaudeAdapter) -> Result<Vec<Project>, CoreError> {
    let sessions = sessions(adapter, &SessionFilter::default())?;
    let mut by_root: HashMap<PathBuf, Project> = HashMap::new();
    for session in &sessions {
        let native_id = match &session.handle.location {
            SessionLocation::JsonlFile { path } => path
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned()),
            _ => None,
        };
        let entry = by_root
            .entry(session.project_root.clone())
            .or_insert_with(|| Project {
                agent: AgentKind::ClaudeCode,
                root: session.project_root.clone(),
                native_id,
                session_count: 0,
                last_updated: None,
            });
        entry.session_count += 1;
        if session.updated > entry.last_updated {
            entry.last_updated = session.updated;
        }
    }
    let mut projects: Vec<Project> = by_root.into_values().collect();
    projects.sort_by(|a, b| b.last_updated.cmp(&a.last_updated));
    Ok(projects)
}

/// A top-level `<uuid>.jsonl` transcript? Returns the session id (= filename
/// stem). Anything else — `memory/`, `<uuid>/` sidecar dirs, stray files —
/// is not a session.
fn transcript_session_id(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    is_uuid(stem).then(|| stem.to_string())
}

fn is_uuid(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => *b == b'-',
        _ => b.is_ascii_hexdigit(),
    })
}

/// Everything we can learn about one transcript from a bounded head+tail scan.
#[derive(Default)]
struct ScanState {
    cwd: Option<String>,
    relocated_cwd: Option<String>,
    slug: Option<String>,
    ai_title: Option<String>,
    custom_title: Option<String>,
    agent_name: Option<String>,
    git_branch: Option<String>,
    model: Option<String>,
    version: Option<String>,
    first_ts: Option<Timestamp>,
    last_ts: Option<Timestamp>,
    saw_conversation: bool,
}

impl ScanState {
    /// Fold one parsed record. Identity note: grouping is by filename stem
    /// only; the snake_case `session_id` field is deliberately never read
    /// (it can hold unrelated ids — verified against real transcripts).
    fn absorb(&mut self, record: &Value) {
        if let Some(ts) = record.get("timestamp").and_then(Value::as_str)
            && let Ok(ts) = ts.parse::<Timestamp>()
        {
            if self.first_ts.is_none() {
                self.first_ts = Some(ts);
            }
            self.last_ts = Some(ts);
        }

        match record.get("type").and_then(Value::as_str) {
            // Claude Code resolves the displayed title as
            // `agentName || customTitle || aiTitle`: `ai-title` is the slot
            // its own summarizer writes, `custom-title` is what its rename
            // writes, and each is last-writer-wins within its own slot.
            Some("ai-title") => {
                if let Some(t) = record.get("aiTitle").and_then(Value::as_str) {
                    self.ai_title = Some(t.to_string());
                }
            }
            Some("custom-title") => {
                if let Some(t) = record.get("customTitle").and_then(Value::as_str) {
                    self.custom_title = Some(t.to_string());
                }
            }
            Some("agent-name") => {
                if let Some(t) = record.get("agentName").and_then(Value::as_str) {
                    self.agent_name = Some(t.to_string());
                }
            }
            Some("relocated") => {
                if let Some(c) = record.get("relocatedCwd").and_then(Value::as_str) {
                    self.relocated_cwd = Some(c.to_string());
                }
            }
            Some("user") | Some("assistant") | Some("attachment") | Some("system") => {
                self.saw_conversation = true;
                if let Some(c) = record.get("cwd").and_then(Value::as_str) {
                    self.cwd = Some(c.to_string()); // latest record wins
                }
                if let Some(s) = record.get("slug").and_then(Value::as_str) {
                    self.slug = Some(s.to_string());
                }
                if let Some(b) = record.get("gitBranch").and_then(Value::as_str) {
                    self.git_branch = Some(b.to_string());
                }
                if let Some(v) = record.get("version").and_then(Value::as_str) {
                    self.version = Some(v.to_string());
                }
                if let Some(m) = record
                    .get("message")
                    .and_then(|m| m.get("model"))
                    .and_then(Value::as_str)
                    && m != "<synthetic>"
                {
                    self.model = Some(m.to_string());
                }
            }
            _ => {}
        }
    }
}

fn scan_transcript(path: &Path, session_id: &str) -> Option<Session> {
    let mut state = ScanState::default();

    // Head: stream lines until the first conversation record fixes cwd/slug/
    // version, or the line budget runs out.
    let file = File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    let mut head = BufReader::new(file).take(MAX_HEAD_LINE_BYTES * 4);
    let mut line = String::new();
    let mut head_lines = 0usize;
    let mut head_created: Option<Timestamp> = None;
    while head_lines < MAX_HEAD_LINES {
        line.clear();
        match head.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        head_lines += 1;
        if let Ok(record) = serde_json::from_str::<Value>(&line) {
            state.absorb(&record);
            if state.saw_conversation {
                head_created = state.first_ts;
                break;
            }
        }
    }
    let head_state = std::mem::take(&mut state);

    // Tail: last TAIL_BYTES, skipping the first (possibly partial) line when
    // we seeked into the middle of the file. A torn final line is normal for
    // a live transcript and is silently skipped by the JSON parse.
    let mut tail_state = ScanState::default();
    if let Ok(mut file) = File::open(path) {
        let start = file_len.saturating_sub(TAIL_BYTES);
        if file.seek(SeekFrom::Start(start)).is_ok() {
            let mut buf = String::new();
            if file.read_to_string(&mut buf).is_ok() {
                let mut lines = buf.lines();
                if start > 0 {
                    lines.next(); // partial first line
                }
                for line in lines {
                    if let Ok(record) = serde_json::from_str::<Value>(line) {
                        tail_state.absorb(&record);
                    }
                }
            }
        }
    }

    if !head_state.saw_conversation && !tail_state.saw_conversation {
        return None;
    }

    // Precedence: an explicit relocation marker beats the latest record cwd,
    // which beats the head cwd.
    let project_root = tail_state
        .relocated_cwd
        .or(tail_state.cwd)
        .or(head_state.cwd)?;

    Some(Session {
        handle: SessionRef {
            agent: AgentKind::ClaudeCode,
            native_id: session_id.to_string(),
            location: SessionLocation::JsonlFile { path: path.to_path_buf() },
        },
        title: tail_state
            .agent_name
            .or(head_state.agent_name)
            .or(tail_state.custom_title)
            .or(head_state.custom_title)
            .or(tail_state.ai_title)
            .or(head_state.ai_title),
        slug: tail_state.slug.or(head_state.slug),
        project_root: PathBuf::from(project_root),
        git_branch: tail_state.git_branch.or(head_state.git_branch),
        created: head_created.or(head_state.first_ts).or(tail_state.first_ts),
        updated: tail_state.last_ts.or(head_state.last_ts),
        model: tail_state.model.or(head_state.model),
        usage: Usage::default(),
        status: SessionStatus::Idle,
        parent: None,
        agent_version: tail_state.version.or(head_state.version),
    })
}

/// Cost/usage rollups from `~/.claude.json`: `projects[<cwd>]` keeps stats
/// for the LAST session per project, keyed by `lastSessionId`. Hints only —
/// never identity.
fn read_usage_rollups(path: &Path) -> HashMap<String, Usage> {
    let mut rollups = HashMap::new();
    let Ok(bytes) = std::fs::read(path) else {
        return rollups;
    };
    let Ok(root) = serde_json::from_slice::<Value>(&bytes) else {
        return rollups;
    };
    let Some(projects) = root.get("projects").and_then(Value::as_object) else {
        return rollups;
    };
    for project in projects.values() {
        let Some(session_id) = project.get("lastSessionId").and_then(Value::as_str) else {
            continue;
        };
        rollups.insert(
            session_id.to_string(),
            Usage {
                cost_usd: project.get("lastCost").and_then(Value::as_f64),
                input_tokens: project.get("lastTotalInputTokens").and_then(Value::as_u64),
                output_tokens: project.get("lastTotalOutputTokens").and_then(Value::as_u64),
                cache_read_tokens: project
                    .get("lastTotalCacheReadInputTokens")
                    .and_then(Value::as_u64),
                cache_write_tokens: project
                    .get("lastTotalCacheCreationInputTokens")
                    .and_then(Value::as_u64),
            },
        );
    }
    rollups
}

/// Session ids whose transcript exists in MORE than one project directory.
/// Claude Code's cross-project `--resume <id>` hard-fails on 2+ matches, so
/// every duplicate silently poisons resume — `doctor` surfaces them.
pub(super) fn duplicate_session_ids(
    adapter: &ClaudeAdapter,
) -> Result<Vec<(String, Vec<PathBuf>)>, CoreError> {
    let projects_dir = adapter.root().join("projects");
    let mut by_id: HashMap<String, Vec<PathBuf>> = HashMap::new();
    if !projects_dir.is_dir() {
        return Ok(Vec::new());
    }
    for project_dir in read_dir_sorted(&projects_dir)? {
        if !project_dir.is_dir() {
            continue;
        }
        for path in read_dir_sorted(&project_dir)? {
            if let Some(id) = transcript_session_id(&path) {
                by_id.entry(id).or_default().push(path);
            }
        }
    }
    let mut duplicates: Vec<(String, Vec<PathBuf>)> =
        by_id.into_iter().filter(|(_, paths)| paths.len() > 1).collect();
    duplicates.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(duplicates)
}

fn read_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>, CoreError> {
    let entries = std::fs::read_dir(dir).map_err(|e| CoreError::io(dir, e))?;
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    Ok(paths)
}
