//! `asm daemon`: keep the hub up to date with this machine, so closing the
//! laptop loses at most one interval of work.
//!
//! Push-only. It never writes into an agent's store — every install is an
//! explicit `asm pull` — so it cannot touch a session someone is using. A
//! session goes up once its fingerprint has held still for a whole
//! interval: a streaming turn is pushed when it ends, not on every write.

use std::collections::HashMap;
use std::time::Duration;

use super::actions::{self, key};
use super::bundle;
use super::client::Remote;
use super::state::SyncState;
use crate::adapter::SessionFilter;
use crate::bulk::{BulkReport, ItemOutcome};
use crate::{CoreError, ops};

/// One pass. `seen` carries each session's fingerprint from the previous
/// pass; a session is pushed when it differs from what was last synced and
/// has not moved since then.
// ponytail: a session the hub refuses (diverged) is re-read and re-refused
// every pass until someone resolves it; remember the refused fingerprint if
// that cost ever shows.
pub fn cycle(
    remote: &Remote,
    filter: &SessionFilter,
    seen: &mut HashMap<String, String>,
) -> Result<BulkReport, CoreError> {
    let state = SyncState::load(remote)?;
    let mut ready = Vec::new();
    let mut now = HashMap::new();
    for session in ops::list_sessions(filter)? {
        let k = key(session.handle.agent, &session.handle.native_id);
        let fingerprint = bundle::fingerprint(&session);
        let synced = state.get(&k).is_some_and(|t| t.fingerprint == fingerprint);
        if !synced && seen.get(&k) == Some(&fingerprint) {
            ready.push(session);
        }
        now.insert(k, fingerprint);
    }
    *seen = now;
    if ready.is_empty() {
        return Ok(BulkReport::default());
    }
    actions::push(remote, &ready, false)
}

/// Run passes forever. `say` gets one line per event; a session (or the
/// hub) that keeps failing the same way is reported once, not every pass.
pub fn run(remote: &Remote, filter: &SessionFilter, interval: Duration, mut say: impl FnMut(&str)) -> ! {
    let mut seen = HashMap::new();
    let mut said: HashMap<String, String> = HashMap::new();
    loop {
        match cycle(remote, filter, &mut seen) {
            Ok(report) => {
                said.remove("hub");
                for item in report.items {
                    let k = key(item.agent, &item.native_id);
                    let line = match &item.outcome {
                        // Every push is news, and ends any earlier failure.
                        ItemOutcome::Ok { note } => {
                            said.remove(&k);
                            say(&format!("{}: {note}", item.label));
                            continue;
                        }
                        ItemOutcome::Skipped { reason } => format!("{}: skipped — {reason}", item.label),
                        ItemOutcome::Failed { error } => format!("{}: failed — {error}", item.label),
                    };
                    once(&mut said, k, line, &mut say);
                }
            }
            Err(e) => once(&mut said, "hub".into(), format!("cannot reach the hub: {e}"), &mut say),
        }
        std::thread::sleep(interval);
    }
}

fn once(said: &mut HashMap<String, String>, k: String, line: String, say: &mut impl FnMut(&str)) {
    if said.get(&k) != Some(&line) {
        say(&line);
        said.insert(k, line);
    }
}
