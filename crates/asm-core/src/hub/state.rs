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
    /// Which hub and which box this state is about. A machine that joins
    /// another hub starts over rather than trusting revisions from a
    /// different one. Joining the same hub again does not: the revisions
    /// are the hub's and the conversations are this box's, whatever id the
    /// hub gave the new registration.
    pub hub: String,
    /// The hub's id for this machine when last recorded; informational.
    pub machine: String,
    /// `identity::machine_uid`; empty in a file from before it existed.
    #[serde(default)]
    pub uid: String,
    pub sessions: BTreeMap<String, Tracked>,
}

/// Whether two states are about one box. Identities decide when both have
/// one; a file from before identities existed (or a box that has none)
/// falls back to the hub's machine id when both lack one, and is taken as
/// this box's when only one does: the file is local, so the only way it is
/// not ours is a different hub, which the url already tells.
fn same_box(a_uid: &str, a_machine: &str, b_uid: &str, b_machine: &str) -> bool {
    match (a_uid.is_empty(), b_uid.is_empty()) {
        (false, false) => a_uid == b_uid,
        (true, true) => a_machine == b_machine,
        _ => true,
    }
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
        let uid = super::identity::machine_uid().unwrap_or_default();
        Ok(state.for_registration(&remote.url, &remote.machine.id, uid))
    }

    /// This state if it is about the same hub and box, else a fresh one.
    fn for_registration(self, hub: &str, machine: &str, uid: String) -> SyncState {
        if self.hub == hub && same_box(&self.uid, &self.machine, &uid, machine) {
            SyncState { machine: machine.to_string(), uid, ..self }
        } else {
            SyncState { hub: hub.to_string(), machine: machine.to_string(), uid, ..Default::default() }
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
            && same_box(&on_disk.uid, &on_disk.machine, &self.uid, &self.machine)
        {
            self.sessions = on_disk.sessions;
        }
        self.sessions.insert(key.to_string(), tracked);
        fsutil::write_atomic(&path, &serde_json::to_vec_pretty(self).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked() -> BTreeMap<String, Tracked> {
        let t = Tracked { hub_rev: "r1".into(), canonical: "c".into(), fingerprint: "f".into(), files: String::new() };
        BTreeMap::from([("claude-code:abc".to_string(), t)])
    }

    fn state(hub: &str, machine: &str, uid: &str) -> SyncState {
        SyncState { hub: hub.into(), machine: machine.into(), uid: uid.into(), sessions: tracked() }
    }

    /// The bug: every `asm join` got a new machine id, the state was keyed
    /// on it, and each registration forgot what had been synced, so synced
    /// sessions showed "pull first".
    #[test]
    fn registering_again_keeps_what_was_synced() {
        let kept = state("http://h", "old", "u1").for_registration("http://h", "new", "u1".into());
        assert_eq!((kept.sessions.len(), kept.machine.as_str()), (1, "new"));
        // A file from before identities existed is this box's too.
        let legacy = state("http://h", "old", "").for_registration("http://h", "new", "u1".into());
        assert_eq!((legacy.sessions.len(), legacy.uid.as_str()), (1, "u1"));
        // No identity on either side: the old rule, by machine id.
        assert_eq!(state("http://h", "m", "").for_registration("http://h", "m", String::new()).sessions.len(), 1);
        assert!(state("http://h", "m", "").for_registration("http://h", "n", String::new()).sessions.is_empty());
    }

    #[test]
    fn another_hub_or_another_box_starts_over() {
        assert!(state("http://h", "m", "u1").for_registration("http://other", "m", "u1".into()).sessions.is_empty());
        let other_box = state("http://h", "m", "u1").for_registration("http://h", "m", "u2".into());
        assert!(other_box.sessions.is_empty() && other_box.uid == "u2");
    }
}
