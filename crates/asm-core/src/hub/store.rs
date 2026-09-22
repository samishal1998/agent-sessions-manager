//! The hub's own store: a content-addressed blob directory, one head per
//! session with every revision kept, and the machines allowed in.
//!
//! ```text
//! <root>/                     0700
//!   hub.json                  { join_token, created }            0600
//!   machines.json             [ { id, name, credential_sha256 } ] 0600
//!   blobs/<aa>/<sha256>       file contents, named by their hash
//!   sessions/<agent>/<id>/head              the current revision
//!   sessions/<agent>/<id>/revisions/<rev>.json
//!   tmp/                      uploads in flight
//! ```
//!
//! Ordering is what makes a crash harmless: blobs land first, a revision
//! only once every blob it names exists, and the head moves last. Killed at
//! any point, the hub holds at worst some unreferenced blobs and a partial
//! upload in `tmp/`, never a head pointing at something missing.
//!
//! A manifest's names never become paths here — only the agent name, a
//! validated id, and a 64-hex sha do.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::manifest::{Manifest, MachineRef, valid_id, valid_sha};
use crate::model::AgentKind;
use crate::{CoreError, fsutil, paths};

pub struct Hub {
    root: PathBuf,
    /// Serializes the writes that must not interleave: machines.json and a
    /// session's head. Blob writes do not take it — two uploads of the same
    /// content write the same bytes.
    // ponytail: one lock for every session; per-session locks if a hub ever
    // serves enough machines for pushes to queue behind each other.
    lock: Mutex<()>,
}

#[derive(Serialize, Deserialize)]
struct HubFile {
    join_token: String,
    created: Timestamp,
}

#[derive(Clone, Serialize, Deserialize)]
struct MachineRecord {
    id: String,
    name: String,
    credential_sha256: String,
    joined: Timestamp,
    #[serde(default)]
    last_seen: Option<Timestamp>,
}

/// A machine as anyone may see it: never the credential hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    pub id: String,
    pub name: String,
    pub joined: Timestamp,
    #[serde(default)]
    pub last_seen: Option<Timestamp>,
}

impl Machine {
    pub fn as_ref(&self) -> MachineRef {
        MachineRef { id: self.id.clone(), name: self.name.clone() }
    }
}

impl From<&MachineRecord> for Machine {
    fn from(r: &MachineRecord) -> Self {
        Machine { id: r.id.clone(), name: r.name.clone(), joined: r.joined, last_seen: r.last_seen }
    }
}

/// Returned once, at join. The hub keeps only its hash.
#[derive(Debug, Serialize, Deserialize)]
pub struct Joined {
    pub machine: Machine,
    pub credential: String,
}

/// One session as the hub listing shows it: the head manifest without its
/// file list, which a listing of every session does not need.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Head {
    pub rev: String,
    pub file_count: usize,
    pub total_size: u64,
    pub manifest: Manifest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revision {
    pub rev: String,
    pub pushed_at: Option<Timestamp>,
    pub machine: Option<MachineRef>,
    pub canonical: String,
    pub parent_rev: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct History {
    pub head: String,
    pub manifest: Manifest,
    /// Newest first.
    pub revisions: Vec<Revision>,
}

#[derive(Debug, thiserror::Error)]
pub enum HubError {
    #[error("invalid or revoked credential")]
    Unauthorized,
    #[error("{0}")]
    BadRequest(String),
    #[error("not found")]
    NotFound,
    /// The push was based on a revision that is no longer the head.
    #[error("the session changed on the hub since this machine last synced it")]
    Conflict { head: Option<String> },
    #[error("{} file(s) not uploaded yet", .0.len())]
    MissingBlobs(Vec<String>),
    #[error("upload exceeds the {limit}-byte limit")]
    TooLarge { limit: u64 },
    #[error("uploaded content does not match its sha256")]
    ShaMismatch,
    #[error(transparent)]
    Core(#[from] CoreError),
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> CoreError + '_ {
    move |e| CoreError::io(path, e)
}

#[cfg(unix)]
fn restrict(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}
#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) {}

/// Hex from the kernel's CSPRNG. Tokens are bearer secrets, so nothing
/// weaker will do, and std has no RNG of its own.
pub fn random_hex(bytes: usize) -> Result<String, CoreError> {
    let mut buf = vec![0u8; bytes];
    let path = Path::new("/dev/urandom");
    fs::File::open(path).and_then(|mut f| f.read_exact(&mut buf)).map_err(io(path))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn sha256(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time comparison. Both sides are hashes, so an attacker learns
/// nothing useful from timing anyway; this closes the question.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.chars().count()) && !name.chars().any(char::is_control)
}

impl Hub {
    /// `<data>/hub` unless a root is given.
    pub fn default_root() -> Option<PathBuf> {
        paths::data_dir().map(|d| d.join("hub"))
    }

    /// Open (creating on first use) the store at `root`. A first open mints
    /// the join token.
    pub fn open(root: &Path) -> Result<Hub, CoreError> {
        for dir in [root.to_path_buf(), root.join("blobs"), root.join("sessions"), root.join("tmp")]
        {
            fs::create_dir_all(&dir).map_err(io(&dir))?;
        }
        restrict(root, 0o700);
        let hub = Hub { root: root.to_path_buf(), lock: Mutex::new(()) };
        if !hub.hub_file().is_file() {
            hub.write_hub_file(&HubFile {
                join_token: format!("asmj_{}", random_hex(24)?),
                created: Timestamp::now(),
            })?;
        }
        // Uploads a previous run was killed in the middle of. They are
        // incomplete by construction and nothing references them.
        if let Ok(entries) = fs::read_dir(root.join("tmp")) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|e| e == "part") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        Ok(hub)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn hub_file(&self) -> PathBuf {
        self.root.join("hub.json")
    }

    fn machines_file(&self) -> PathBuf {
        self.root.join("machines.json")
    }

    fn write_private(&self, path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
        fsutil::write_atomic(path, bytes)?;
        restrict(path, 0o600);
        Ok(())
    }

    fn write_hub_file(&self, file: &HubFile) -> Result<(), CoreError> {
        self.write_private(&self.hub_file(), &serde_json::to_vec_pretty(file).unwrap())
    }

    fn read_hub_file(&self) -> Result<HubFile, CoreError> {
        let path = self.hub_file();
        let bytes = fs::read(&path).map_err(io(&path))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| CoreError::Invalid { msg: format!("{} is unreadable: {e}", path.display()) })
    }

    fn read_machines(&self) -> Result<Vec<MachineRecord>, CoreError> {
        let path = self.machines_file();
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| CoreError::Invalid {
                msg: format!("{} is unreadable: {e}", path.display()),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(CoreError::io(&path, e)),
        }
    }

    fn write_machines(&self, machines: &[MachineRecord]) -> Result<(), CoreError> {
        self.write_private(&self.machines_file(), &serde_json::to_vec_pretty(machines).unwrap())
    }

    pub fn join_token(&self) -> Result<String, CoreError> {
        Ok(self.read_hub_file()?.join_token)
    }

    /// A new join token; the old one stops working. Machines that already
    /// joined keep their own credentials.
    pub fn rotate_join_token(&self) -> Result<String, CoreError> {
        let _guard = self.lock.lock().unwrap();
        let mut file = self.read_hub_file()?;
        file.join_token = format!("asmj_{}", random_hex(24)?);
        self.write_hub_file(&file)?;
        Ok(file.join_token)
    }

    /// Exchange the join token for a credential of this machine's own.
    pub fn join(&self, token: &str, name: &str) -> Result<Joined, HubError> {
        let expected = self.read_hub_file()?.join_token;
        if !ct_eq(&sha256(token), &sha256(&expected)) {
            return Err(HubError::Unauthorized);
        }
        let name = name.trim();
        if !valid_name(name) {
            return Err(HubError::BadRequest("machine name must be 1-64 printable characters".into()));
        }
        let credential = format!("asmc_{}", random_hex(32)?);
        let record = MachineRecord {
            id: random_hex(8)?,
            name: name.to_string(),
            credential_sha256: hex(&sha256(&credential)),
            joined: Timestamp::now(),
            last_seen: Some(Timestamp::now()),
        };
        let _guard = self.lock.lock().unwrap();
        let mut machines = self.read_machines()?;
        machines.push(record.clone());
        self.write_machines(&machines)?;
        Ok(Joined { machine: Machine::from(&record), credential })
    }

    /// The machine a credential belongs to. Marks it seen, at most once a
    /// minute, so "discovery" can say which machines are around.
    pub fn authenticate(&self, credential: &str) -> Result<Machine, HubError> {
        let presented = sha256(credential);
        let machines = self.read_machines()?;
        let mut found = None;
        // Every record is compared, so timing does not say how far the
        // search got.
        for (i, m) in machines.iter().enumerate() {
            let stored = decode_hex(&m.credential_sha256);
            if ct_eq(&presented, &stored) {
                found = Some(i);
            }
        }
        let i = found.ok_or(HubError::Unauthorized)?;
        let stale = machines[i]
            .last_seen
            .is_none_or(|t| Timestamp::now().duration_since(t).as_secs() >= 60);
        if stale {
            let _guard = self.lock.lock().unwrap();
            let mut fresh = self.read_machines()?;
            if let Some(m) = fresh.iter_mut().find(|m| m.id == machines[i].id) {
                m.last_seen = Some(Timestamp::now());
                self.write_machines(&fresh)?;
            }
        }
        Ok(Machine::from(&machines[i]))
    }

    pub fn machines(&self) -> Result<Vec<Machine>, CoreError> {
        Ok(self.read_machines()?.iter().map(Machine::from).collect())
    }

    /// Remove a machine by id, or by name when the name is unique.
    pub fn revoke(&self, who: &str) -> Result<Machine, HubError> {
        let _guard = self.lock.lock().unwrap();
        let mut machines = self.read_machines()?;
        let matches: Vec<usize> = machines
            .iter()
            .enumerate()
            .filter(|(_, m)| m.id == who || m.name == who)
            .map(|(i, _)| i)
            .collect();
        let i = match matches.as_slice() {
            [i] => *i,
            [] => return Err(HubError::NotFound),
            _ => {
                return Err(HubError::BadRequest(format!(
                    "{who:?} names {} machines; revoke by id instead",
                    matches.len()
                )));
            }
        };
        let removed = machines.remove(i);
        self.write_machines(&machines)?;
        Ok(Machine::from(&removed))
    }

    fn blob_path(&self, sha: &str) -> PathBuf {
        self.root.join("blobs").join(&sha[..2]).join(sha)
    }

    pub fn missing(&self, shas: &[String]) -> Result<Vec<String>, HubError> {
        if let Some(bad) = shas.iter().find(|s| !valid_sha(s)) {
            return Err(HubError::BadRequest(format!("invalid sha256 {bad:?}")));
        }
        Ok(shas.iter().filter(|s| !self.blob_path(s).is_file()).cloned().collect())
    }

    /// Store `body` as the blob named `sha`, verifying it while it streams.
    /// Returns whether it was new. Nothing is published unless the whole
    /// body arrived and hashed to its name.
    pub fn put_blob(&self, sha: &str, mut body: impl Read, limit: u64) -> Result<bool, HubError> {
        if !valid_sha(sha) {
            return Err(HubError::BadRequest(format!("invalid sha256 {sha:?}")));
        }
        let tmp = self.root.join("tmp").join(format!("{}.part", random_hex(12)?));
        let result = (|| {
            let mut file = fs::File::create(&tmp).map_err(io(&tmp))?;
            let mut hasher = Sha256::new();
            let mut total: u64 = 0;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = body.read(&mut buf).map_err(io(&tmp))?;
                if n == 0 {
                    break;
                }
                total += n as u64;
                if total > limit {
                    return Err(HubError::TooLarge { limit });
                }
                hasher.update(&buf[..n]);
                file.write_all(&buf[..n]).map_err(io(&tmp))?;
            }
            file.sync_all().map_err(io(&tmp))?;
            if hex(&hasher.finalize()) != sha {
                return Err(HubError::ShaMismatch);
            }
            let dest = self.blob_path(sha);
            if dest.is_file() {
                return Ok(false);
            }
            let parent = dest.parent().unwrap();
            fs::create_dir_all(parent).map_err(io(parent))?;
            fs::rename(&tmp, &dest).map_err(io(&dest))?;
            Ok(true)
        })();
        let _ = fs::remove_file(&tmp);
        result
    }

    pub fn blob(&self, sha: &str) -> Result<PathBuf, HubError> {
        if !valid_sha(sha) {
            return Err(HubError::BadRequest(format!("invalid sha256 {sha:?}")));
        }
        let path = self.blob_path(sha);
        if path.is_file() { Ok(path) } else { Err(HubError::NotFound) }
    }

    fn session_dir(&self, agent: AgentKind, id: &str) -> PathBuf {
        self.root.join("sessions").join(agent.as_str()).join(id)
    }

    fn head_of(&self, agent: AgentKind, id: &str) -> Result<Option<String>, CoreError> {
        let path = self.session_dir(agent, id).join("head");
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text.trim().to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(CoreError::io(&path, e)),
        }
    }

    fn read_revision(&self, agent: AgentKind, id: &str, rev: &str) -> Result<Manifest, CoreError> {
        let path = self.session_dir(agent, id).join("revisions").join(format!("{rev}.json"));
        let bytes = fs::read(&path).map_err(io(&path))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| CoreError::Invalid { msg: format!("{} is unreadable: {e}", path.display()) })
    }

    /// Accept a new revision of a session. Refused unless it names the
    /// current head as its parent, and unless every file it lists has
    /// already been uploaded. Returns the new revision id.
    ///
    /// There is no way around the parent check: a client that means to
    /// replace another machine's copy names that copy as the parent, so a
    /// third push landing in between is still refused.
    pub fn put_revision(
        &self,
        agent: &str,
        id: &str,
        body: &[u8],
        by: &Machine,
    ) -> Result<String, HubError> {
        let agent = AgentKind::parse(agent)
            .ok_or_else(|| HubError::BadRequest(format!("unknown agent {agent:?}")))?;
        let mut manifest: Manifest = serde_json::from_slice(body)
            .map_err(|e| HubError::BadRequest(format!("manifest is not valid: {e}")))?;
        manifest.validate().map_err(HubError::BadRequest)?;
        if manifest.agent != agent || manifest.id != id || !valid_id(id) {
            return Err(HubError::BadRequest("manifest does not match the URL".into()));
        }

        let _guard = self.lock.lock().unwrap();
        let head = self.head_of(agent, id)?;
        if manifest.parent_rev != head {
            return Err(HubError::Conflict { head });
        }
        let missing: Vec<String> =
            manifest.blob_shas().filter(|s| !self.blob_path(s).is_file()).map(String::from).collect();
        if !missing.is_empty() {
            return Err(HubError::MissingBlobs(missing));
        }

        // What the hub records is who pushed it and when by its own clock,
        // whatever the client claimed.
        manifest.machine = Some(by.as_ref());
        manifest.pushed_at = Some(Timestamp::now());
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let rev = fsutil::sha256_hex(&bytes);

        let dir = self.session_dir(agent, id);
        let revisions = dir.join("revisions");
        fs::create_dir_all(&revisions).map_err(io(&revisions))?;
        fsutil::write_atomic(&revisions.join(format!("{rev}.json")), &bytes)?;
        fsutil::write_atomic(&dir.join("head"), rev.as_bytes())?;
        Ok(rev)
    }

    pub fn heads(&self) -> Result<Vec<Head>, CoreError> {
        let mut heads = Vec::new();
        let sessions = self.root.join("sessions");
        for agent_dir in read_dirs(&sessions) {
            let Some(agent) = agent_dir.file_name().and_then(|n| n.to_str()).and_then(AgentKind::parse)
            else {
                continue;
            };
            for session_dir in read_dirs(&agent_dir) {
                let Some(id) = session_dir.file_name().and_then(|n| n.to_str()) else { continue };
                let Some(rev) = self.head_of(agent, id)? else { continue };
                let mut manifest = self.read_revision(agent, id, &rev)?;
                let file_count = manifest.files.len();
                let total_size = manifest.total_size();
                manifest.files.clear();
                heads.push(Head { rev, file_count, total_size, manifest });
            }
        }
        heads.sort_by_key(|h| std::cmp::Reverse(h.manifest.updated));
        Ok(heads)
    }

    /// One revision's manifest, files and all: what a machine last synced,
    /// so a pull can tell what changed on each side since.
    pub fn revision(&self, agent: &str, id: &str, rev: &str) -> Result<Manifest, HubError> {
        let agent = AgentKind::parse(agent).ok_or(HubError::NotFound)?;
        if !valid_id(id) || !super::manifest::valid_sha(rev) {
            return Err(HubError::NotFound);
        }
        if !self.session_dir(agent, id).join("revisions").join(format!("{rev}.json")).is_file() {
            return Err(HubError::NotFound);
        }
        Ok(self.read_revision(agent, id, rev)?)
    }

    pub fn history(&self, agent: &str, id: &str) -> Result<History, HubError> {
        let agent = AgentKind::parse(agent).ok_or(HubError::NotFound)?;
        if !valid_id(id) {
            return Err(HubError::NotFound);
        }
        let head = self.head_of(agent, id)?.ok_or(HubError::NotFound)?;
        let manifest = self.read_revision(agent, id, &head)?;
        let mut revisions = Vec::new();
        let dir = self.session_dir(agent, id).join("revisions");
        for entry in fs::read_dir(&dir).map_err(io(&dir))?.flatten() {
            let Some(rev) = entry.path().file_stem().and_then(|s| s.to_str()).map(String::from)
            else {
                continue;
            };
            if let Ok(m) = self.read_revision(agent, id, &rev) {
                revisions.push(Revision {
                    rev,
                    pushed_at: m.pushed_at,
                    machine: m.machine,
                    canonical: m.canonical,
                    parent_rev: m.parent_rev,
                });
            }
        }
        revisions.sort_by_key(|r| std::cmp::Reverse(r.pushed_at));
        Ok(History { head, manifest, revisions })
    }
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .map(|e| e.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default();
    out.sort();
    out
}

fn decode_hex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .filter_map(|i| u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::manifest::{FileEntry, SCHEMA};

    fn hub() -> (tempfile::TempDir, Hub) {
        let dir = tempfile::tempdir().unwrap();
        let hub = Hub::open(&dir.path().join("hub")).unwrap();
        (dir, hub)
    }

    fn manifest(parent: Option<String>, shas: &[&str]) -> Vec<u8> {
        let m = Manifest {
            schema: SCHEMA,
            agent: AgentKind::ClaudeCode,
            id: "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43".into(),
            title: Some("t".into()),
            slug: None,
            project_root: "/home/a/x".into(),
            project_root_portable: "${HOME}/x".into(),
            git_origin: None,
            git_branch: None,
            agent_version: None,
            created: None,
            updated: None,
            // A client claim the hub must overwrite.
            machine: Some(MachineRef { id: "forged".into(), name: "forged".into() }),
            pushed_at: None,
            canonical: "c".into(),
            parent_rev: parent,
            files: shas
                .iter()
                .enumerate()
                .map(|(i, s)| FileEntry {
                    path: if i == 0 { "transcript.jsonl".into() } else { format!("tasks/{i}") },
                    sha256: Some(s.to_string()),
                    size: 1,
                    symlink: None,
                })
                .collect(),
            extra: serde_json::Value::Null,
        };
        serde_json::to_vec(&m).unwrap()
    }

    const ID: &str = "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43";

    #[test]
    fn a_wrong_join_token_mints_nothing() {
        let (_d, hub) = hub();
        assert!(matches!(hub.join("asmj_wrong", "laptop"), Err(HubError::Unauthorized)));
        assert!(hub.machines().unwrap().is_empty());
    }

    #[test]
    fn a_credential_works_until_revoked() {
        let (_d, hub) = hub();
        let joined = hub.join(&hub.join_token().unwrap(), "laptop").unwrap();
        assert_eq!(hub.authenticate(&joined.credential).unwrap().name, "laptop");
        assert!(matches!(hub.authenticate("asmc_nope"), Err(HubError::Unauthorized)));
        hub.revoke("laptop").unwrap();
        assert!(matches!(hub.authenticate(&joined.credential), Err(HubError::Unauthorized)));
    }

    /// The hub keeps hashes, never the credential itself.
    #[test]
    fn credentials_are_stored_hashed() {
        let (_d, hub) = hub();
        let joined = hub.join(&hub.join_token().unwrap(), "laptop").unwrap();
        let stored = fs::read_to_string(hub.machines_file()).unwrap();
        assert!(!stored.contains(&joined.credential));
    }

    #[test]
    fn rotating_the_join_token_keeps_joined_machines() {
        let (_d, hub) = hub();
        let old = hub.join_token().unwrap();
        let joined = hub.join(&old, "a").unwrap();
        let new = hub.rotate_join_token().unwrap();
        assert_ne!(old, new);
        assert!(matches!(hub.join(&old, "b"), Err(HubError::Unauthorized)));
        assert!(hub.authenticate(&joined.credential).is_ok());
    }

    #[test]
    fn a_blob_is_only_published_under_its_own_hash() {
        let (_d, hub) = hub();
        let sha = fsutil::sha256_hex(b"hello");
        assert!(matches!(
            hub.put_blob(&sha, &b"goodbye"[..], 1024),
            Err(HubError::ShaMismatch)
        ));
        assert!(hub.blob(&sha).is_err());
        assert!(hub.put_blob(&sha, &b"hello"[..], 1024).unwrap());
        assert!(!hub.put_blob(&sha, &b"hello"[..], 1024).unwrap(), "second copy is not new");
        assert_eq!(fs::read(hub.blob(&sha).unwrap()).unwrap(), b"hello");
        assert!(fs::read_dir(hub.root().join("tmp")).unwrap().next().is_none(), "no litter");
    }

    #[test]
    fn an_oversized_blob_is_refused_without_litter() {
        let (_d, hub) = hub();
        let body = vec![7u8; 5000];
        let sha = fsutil::sha256_hex(&body);
        assert!(matches!(hub.put_blob(&sha, &body[..], 4096), Err(HubError::TooLarge { .. })));
        assert!(hub.blob(&sha).is_err());
        assert!(fs::read_dir(hub.root().join("tmp")).unwrap().next().is_none());
    }

    #[test]
    fn a_revision_needs_every_blob_and_the_right_parent() {
        let (_d, hub) = hub();
        let me = hub.join(&hub.join_token().unwrap(), "a").unwrap().machine;
        let sha = fsutil::sha256_hex(b"one");

        match hub.put_revision("claude-code", ID, &manifest(None, &[&sha]), &me) {
            Err(HubError::MissingBlobs(m)) => assert_eq!(m, vec![sha.clone()]),
            other => panic!("expected missing blobs, got {other:?}"),
        }
        hub.put_blob(&sha, &b"one"[..], 1024).unwrap();
        let rev1 = hub.put_revision("claude-code", ID, &manifest(None, &[&sha]), &me).unwrap();

        // Based on nothing, but the session has a head now: someone else's
        // push must not be silently replaced.
        match hub.put_revision("claude-code", ID, &manifest(None, &[&sha]), &me) {
            Err(HubError::Conflict { head }) => assert_eq!(head.as_deref(), Some(rev1.as_str())),
            other => panic!("expected conflict, got {other:?}"),
        }
        let rev2 =
            hub.put_revision("claude-code", ID, &manifest(Some(rev1.clone()), &[&sha]), &me)
                .unwrap();

        let history = hub.history("claude-code", ID).unwrap();
        assert_eq!(history.head, rev2);
        assert_eq!(history.revisions.len(), 2, "every revision is kept");
        assert_eq!(history.manifest.parent_rev.as_deref(), Some(rev1.as_str()));
        assert_eq!(history.manifest.machine.unwrap().name, "a", "the hub says who pushed it");
    }

    #[test]
    fn the_url_and_the_manifest_must_agree() {
        let (_d, hub) = hub();
        let me = hub.join(&hub.join_token().unwrap(), "a").unwrap().machine;
        for (agent, id) in [("codex", ID), ("claude-code", "other"), ("claude-code", "../x")] {
            assert!(matches!(
                hub.put_revision(agent, id, &manifest(None, &[]), &me),
                Err(HubError::BadRequest(_))
            ));
        }
    }

    #[test]
    fn listing_drops_file_lists() {
        let (_d, hub) = hub();
        let me = hub.join(&hub.join_token().unwrap(), "a").unwrap().machine;
        let sha = fsutil::sha256_hex(b"one");
        hub.put_blob(&sha, &b"one"[..], 1024).unwrap();
        hub.put_revision("claude-code", ID, &manifest(None, &[&sha]), &me).unwrap();
        let heads = hub.heads().unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].file_count, 1);
        assert!(heads[0].manifest.files.is_empty());
    }

    #[test]
    fn reopening_clears_abandoned_uploads_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("hub");
        let hub = Hub::open(&root).unwrap();
        let token = hub.join_token().unwrap();
        fs::write(root.join("tmp/abc.part"), b"half").unwrap();
        let hub = Hub::open(&root).unwrap();
        assert!(!root.join("tmp/abc.part").exists());
        assert_eq!(hub.join_token().unwrap(), token, "a reopen keeps the token");
    }
}
