//! What this machine last agreed with the hub about, per session.
//!
//! `hub_rev` is the revision this machine's copy was last in sync with, and
//! it is what a push names as its parent. `canonical` is the conversation's
//! identity at that moment; if it has moved since, this machine changed the
//! session. `fingerprint` is the cheap local check (size and mtime) that
//! lets an unchanged session be skipped without reading it. `files` is the
//! bundle's file list at that moment, so a changed sidecar is a change too.
//!
//! Losing this file is safe: a push with no record of a session it finds on
//! the hub is refused as unknown rather than guessed at.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::client::Remote;
use crate::{CoreError, fsutil, paths};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tracked {
    pub hub_rev: String,
    pub canonical: String,
    pub fingerprint: String,
    #[serde(default)]
    pub files: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SyncState {
    /// Which hub and which identity on it this state is about. A machine
    /// that joins another hub starts over rather than trusting revisions
    /// from a different one.
    pub hub: String,
    pub machine: String,
    pub sessions: BTreeMap<String, Tracked>,
}

fn path() -> Result<PathBuf, CoreError> {
    paths::data_dir()
        .map(|d| d.join("hub-state.json"))
        .ok_or_else(|| CoreError::Invalid { msg: "cannot determine asm data dir".into() })
}

impl SyncState {
    pub fn load(remote: &Remote) -> Result<SyncState, CoreError> {
        let path = path()?;
        let state: SyncState = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => SyncState::default(),
            Err(e) => return Err(CoreError::io(&path, e)),
        };
        if state.hub == remote.url && state.machine == remote.machine.id {
            Ok(state)
        } else {
            Ok(SyncState { hub: remote.url.clone(), machine: remote.machine.id.clone(), ..Default::default() })
        }
    }

    pub fn get(&self, key: &str) -> Option<&Tracked> {
        self.sessions.get(key)
    }

    /// Record one session and write the file, re-reading it first so a push
    /// running beside a daemon does not erase the daemon's records.
    // ponytail: re-read-then-write narrows the race between two asm
    // processes to a few microseconds; a lock file if that ever matters.
    pub fn record(&mut self, key: &str, tracked: Tracked) -> Result<(), CoreError> {
        let path = path()?;
        // The file on disk is the base, so another process's newer record
        // for a different session survives; only this key is ours to set.
        if let Ok(bytes) = std::fs::read(&path)
            && let Ok(on_disk) = serde_json::from_slice::<SyncState>(&bytes)
            && on_disk.hub == self.hub
            && on_disk.machine == self.machine
        {
            self.sessions = on_disk.sessions;
        }
        self.sessions.insert(key.to_string(), tracked);
        fsutil::write_atomic(&path, &serde_json::to_vec_pretty(self).unwrap())
    }
}
