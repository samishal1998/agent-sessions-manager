//! `asm daemon`: keep the hub up to date with this machine.
//!
//! Push-only. It never writes into an agent's store — every install is an
//! explicit `asm pull` — so it cannot touch a session someone is using. A
//! session goes up once its fingerprint has held still for a whole
//! interval, so a streaming turn is pushed when it ends rather than on every
//! write; one that never holds still (a long agent run) goes up anyway every
//! `MAX_WAIT` passes. Closing the lid therefore loses up to two intervals of
//! a session at rest, and up to `MAX_WAIT` of one still being written.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::actions::{self, key};
use super::bundle;
use super::client::Remote;
use super::state::SyncState;
use crate::adapter::SessionFilter;
use crate::bulk::{BulkReport, ItemOutcome};
use crate::{CoreError, fsutil, ops, paths};

/// Passes a changed session may keep changing before it is pushed anyway.
const MAX_WAIT: u32 = 10;

/// One pass. `seen` carries each session's fingerprint from the previous
/// pass and how many passes it has been waiting; a session is pushed when
/// it differs from what was last synced and has not moved since, or has
/// waited `MAX_WAIT` passes.
// ponytail: a session the hub refuses (diverged) is re-read and re-refused
// every pass until someone resolves it; remember the refused fingerprint if
// that cost ever shows.
pub fn cycle(
    remote: &Remote,
    filter: &SessionFilter,
    seen: &mut HashMap<String, (String, u32)>,
) -> Result<Pass, CoreError> {
    let state = SyncState::load(remote)?;
    let mut ready = Vec::new();
    let mut now = HashMap::new();
    let mut unsynced = 0;
    for session in ops::list_sessions(filter)? {
        let k = key(session.handle.agent, &session.handle.native_id);
        let fingerprint = bundle::fingerprint(&session);
        let synced = state.get(&k).is_some_and(|t| t.fingerprint == fingerprint);
        let (before, waited) = seen.get(&k).cloned().unwrap_or_default();
        unsynced += usize::from(!synced);
        let waited = if synced { 0 } else { waited + 1 };
        if !synced && (before == fingerprint || waited >= MAX_WAIT) {
            ready.push(session);
        }
        now.insert(k, (fingerprint, waited));
    }
    let report = if ready.is_empty() { BulkReport::default() } else { actions::push(remote, &ready, false, false)? };
    // The wait starts over only for what the hub now has; a push that
    // failed is tried again next pass rather than waiting another round.
    for item in &report.items {
        if matches!(item.outcome, ItemOutcome::Ok { .. })
            && let Some(entry) = now.get_mut(&key(item.agent, &item.native_id))
        {
            entry.1 = 0;
        }
    }
    *seen = now;
    let pending = unsynced.saturating_sub(report.ok());
    Ok(Pass { report, pending })
}

/// What one pass did, and how many sessions are still out of step with the
/// hub (waiting for a quiet pass, refused, or failed).
pub struct Pass {
    pub report: BulkReport,
    pub pending: usize,
}

/// Events kept in the status file, newest last.
const RECENT: usize = 20;

/// A longest-quiet pass can legitimately take a while (a big upload), so a
/// daemon is called hung only after this many intervals — and at least
/// `HUNG_FLOOR` seconds — without finishing one.
const HUNG_INTERVALS: u64 = 5;
const HUNG_FLOOR: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Unix seconds.
    pub at: u64,
    pub ok: bool,
    pub line: String,
}

/// What the running daemon writes after every pass, for `asm daemon status`
/// and the UIs. The daemon is the only writer; everything else only reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonFile {
    pub pid: u32,
    pub version: String,
    pub started: u64,
    pub interval: u64,
    pub hub: String,
    pub machine: String,
    pub passes: u64,
    /// When the last pass finished (unix seconds); `None` before the first.
    pub last_pass: Option<u64>,
    pub last_push: Option<u64>,
    /// Sessions not yet in step with the hub after the last pass.
    pub pending: usize,
    pub hub_reachable: bool,
    pub last_error: Option<String>,
    /// Sessions brought into step with the hub, over this run.
    pub synced: u64,
    pub recent: Vec<Event>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonState {
    /// No process holds the daemon lock.
    NotRunning,
    Running,
    /// Holds the lock but has not finished a pass in far too long.
    Hung,
}

#[derive(Debug, Clone, Serialize)]
pub struct DaemonStatus {
    pub state: DaemonState,
    /// The last thing the daemon wrote. For `NotRunning` this is what the
    /// previous run left behind, or `None` if one never ran here.
    pub file: Option<DaemonFile>,
    /// Seconds since `now` was last written (the pass, or the start).
    pub quiet_for: Option<u64>,
}

/// `n` seconds as "12s", "4m", "3h", "2d".
pub fn ago(n: u64) -> String {
    match n {
        0..60 => format!("{n}s"),
        60..3600 => format!("{}m", n / 60),
        3600..86400 => format!("{}h", n / 3600),
        _ => format!("{}d", n / 86400),
    }
}

impl DaemonStatus {
    /// One line for a status bar: whether it runs, when it last pushed, what
    /// waits. The same words in the CLI header and the TUI.
    pub fn line(&self) -> String {
        let now = now_secs();
        let since = |t: Option<u64>| t.map_or("not yet".to_string(), |t| format!("{} ago", ago(now.saturating_sub(t))));
        match (&self.state, &self.file) {
            (DaemonState::Running, Some(f)) => {
                let wait = if f.pending > 0 { format!(" · {} waiting", f.pending) } else { String::new() };
                format!("daemon running · last push {}{wait}", since(f.last_push))
            }
            (DaemonState::Running, None) => "daemon running".into(),
            (DaemonState::Hung, _) => format!("daemon not responding for {}", ago(self.quiet_for.unwrap_or(0))),
            (DaemonState::NotRunning, Some(f)) => {
                format!("daemon stopped (last ran {})", since(Some(f.last_pass.unwrap_or(f.started))))
            }
            (DaemonState::NotRunning, None) => "no daemon on this machine".into(),
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn dir() -> Result<PathBuf, CoreError> {
    paths::data_dir()
        .map(|d| d.join("daemon"))
        .ok_or_else(|| CoreError::Invalid { msg: "cannot determine asm data dir".into() })
}

/// Is a daemon running here, and what did it last report? The lock is the
/// truth about running — a pid in a file can be stale or reused.
pub fn status() -> Result<DaemonStatus, CoreError> {
    status_in(&dir()?, now_secs())
}

fn status_in(dir: &std::path::Path, now: u64) -> Result<DaemonStatus, CoreError> {
    let file: Option<DaemonFile> =
        std::fs::read(dir.join("status.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let held = if dir.join("daemon.lock").exists() { fsutil::lock_exclusive(&dir.join("daemon.lock"))?.is_none() } else { false };
    let quiet_for = file.as_ref().map(|f| now.saturating_sub(f.last_pass.unwrap_or(f.started)));
    let state = match (held, &file, quiet_for) {
        (false, ..) => DaemonState::NotRunning,
        (true, Some(f), Some(q)) if q > (f.interval * HUNG_INTERVALS).max(HUNG_FLOOR) => DaemonState::Hung,
        _ => DaemonState::Running,
    };
    Ok(DaemonStatus { state, file, quiet_for })
}

/// Run passes until the process is stopped. Only one daemon runs per data
/// directory: a second one is refused rather than racing the first.
/// `say` gets one line per event; a session (or the hub) that keeps failing
/// the same way is reported once, not every pass.
pub fn run(
    remote: &Remote,
    filter: &SessionFilter,
    interval: Duration,
    mut say: impl FnMut(&str),
) -> Result<(), CoreError> {
    let dir = dir()?;
    let Some(_lock) = fsutil::lock_exclusive(&dir.join("daemon.lock"))? else {
        let pid = status()?.file.map(|f| format!(" (pid {})", f.pid)).unwrap_or_default();
        return Err(CoreError::Invalid { msg: format!("a daemon is already running{pid}; see `asm daemon status`") });
    };
    let mut info = DaemonFile {
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").into(),
        started: now_secs(),
        interval: interval.as_secs(),
        hub: remote.url.clone(),
        machine: remote.machine.name.clone(),
        passes: 0,
        last_pass: None,
        last_push: None,
        pending: 0,
        hub_reachable: true,
        last_error: None,
        synced: 0,
        recent: Vec::new(),
    };
    // A status that cannot be written must not stop the daemon from doing
    // its job; the UIs just show it as quiet.
    let publish = |info: &DaemonFile| {
        if let Ok(bytes) = serde_json::to_vec_pretty(info) {
            let _ = fsutil::write_atomic(&dir.join("status.json"), &bytes);
        }
    };
    publish(&info);
    let mut seen = HashMap::new();
    let mut said: HashMap<String, String> = HashMap::new();
    let note = |info: &mut DaemonFile, ok: bool, line: &str| {
        info.recent.push(Event { at: now_secs(), ok, line: line.into() });
        if info.recent.len() > RECENT {
            info.recent.remove(0);
        }
    };
    loop {
        match cycle(remote, filter, &mut seen) {
            Ok(Pass { report, pending }) => {
                info.pending = pending;
                // A pass with nothing to push never talks to the hub, so
                // ask it, or "reachable" would just mean "not asked".
                let ping = if report.items.is_empty() { remote.ping() } else { Ok(()) };
                info.hub_reachable = ping.is_ok();
                let mut failing = None;
                for item in report.items {
                    let k = key(item.agent, &item.native_id);
                    let (ok, line) = match &item.outcome {
                        // Every push is news, and ends any earlier failure.
                        ItemOutcome::Ok { note: n } => {
                            said.remove(&k);
                            info.synced += 1;
                            info.last_push = Some(now_secs());
                            let line = format!("{}: {n}", item.label);
                            say(&line);
                            note(&mut info, true, &line);
                            continue;
                        }
                        ItemOutcome::Skipped { reason } => (true, format!("{}: skipped — {reason}", item.label)),
                        ItemOutcome::Failed { error } => (false, format!("{}: failed — {error}", item.label)),
                    };
                    if !ok {
                        failing = Some(line.clone());
                    }
                    if once(&mut said, k, line.clone(), &mut say) {
                        note(&mut info, ok, &line);
                    }
                }
                match ping {
                    Ok(()) => {
                        said.remove("hub");
                        info.last_error = failing;
                    }
                    Err(e) => {
                        let line = format!("cannot reach the hub: {e}");
                        info.last_error = Some(line.clone());
                        if once(&mut said, "hub".into(), line.clone(), &mut say) {
                            note(&mut info, false, &line);
                        }
                    }
                }
            }
            Err(e) => {
                let line = format!("cannot reach the hub: {e}");
                info.hub_reachable = false;
                info.last_error = Some(line.clone());
                if once(&mut said, "hub".into(), line.clone(), &mut say) {
                    note(&mut info, false, &line);
                }
            }
        }
        info.passes += 1;
        info.last_pass = Some(now_secs());
        publish(&info);
        std::thread::sleep(interval);
    }
}

/// Say `line` unless it repeats what was last said for `k`; true if said.
fn once(said: &mut HashMap<String, String>, k: String, line: String, say: &mut impl FnMut(&str)) -> bool {
    if said.get(&k) == Some(&line) {
        return false;
    }
    say(&line);
    said.insert(k, line);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(last_pass: Option<u64>) -> DaemonFile {
        DaemonFile {
            pid: 1,
            version: "t".into(),
            started: 1000,
            interval: 30,
            hub: "http://h".into(),
            machine: "m".into(),
            passes: 1,
            last_pass,
            last_push: None,
            pending: 0,
            hub_reachable: true,
            last_error: None,
            synced: 0,
            recent: vec![],
        }
    }

    #[test]
    fn the_lock_decides_running_and_silence_decides_hung() {
        let dir = tempfile::tempdir().unwrap();
        let st = |now| status_in(dir.path(), now).unwrap();
        assert_eq!(st(1000).state, DaemonState::NotRunning);
        assert!(st(1000).file.is_none());

        std::fs::write(dir.path().join("status.json"), serde_json::to_vec(&file(Some(2000))).unwrap()).unwrap();
        // A leftover file with no lock holder is a daemon that stopped.
        assert_eq!(st(2001).state, DaemonState::NotRunning);
        assert!(st(2001).file.is_some());

        let _held = fsutil::lock_exclusive(&dir.path().join("daemon.lock")).unwrap().unwrap();
        assert_eq!(st(2010).state, DaemonState::Running);
        assert_eq!(st(2010).quiet_for, Some(10));
        // Hung only past max(5 intervals, 300s) without a finished pass.
        assert_eq!(st(2000 + 299).state, DaemonState::Running);
        assert_eq!(st(2000 + 301).state, DaemonState::Hung);
    }

    #[test]
    fn a_second_lock_holder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("daemon.lock");
        let _first = fsutil::lock_exclusive(&p).unwrap().unwrap();
        assert!(fsutil::lock_exclusive(&p).unwrap().is_none());
    }
}
