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
    /// Hash of the admin token, if one was ever minted. The admin API and
    /// UI exist only once it is: a hub with none answers 401 to both.
    #[serde(default)]
    admin_token_sha256: Option<String>,
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
        Self::open_with(root, true)
    }

    /// Open without sweeping `tmp/`. For a command run beside a live
    /// `asm hub serve`: the uploads in `tmp/` are that server's in-flight
    /// ones, and only a server starting up may call them abandoned.
    pub fn attach(root: &Path) -> Result<Hub, CoreError> {
        Self::open_with(root, false)
    }

    fn open_with(root: &Path, sweep: bool) -> Result<Hub, CoreError> {
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
                admin_token_sha256: None,
            })?;
        }
        // Uploads a previous run was killed in the middle of. They are
        // incomplete by construction and nothing references them.
        if sweep
            && let Ok(entries) = fs::read_dir(root.join("tmp"))
        {
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

    /// Hold an exclusive lock on the hub file's writers across processes: the
    /// CLI mints tokens while a server may be rotating one. `self.lock` only
    /// orders threads of one process.
    fn hub_file_lock(&self) -> Result<fs::File, CoreError> {
        let path = self.root.join("hub.lock");
        for _ in 0..100 {
            if let Some(file) = fsutil::lock_exclusive(&path)? {
                return Ok(file);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Err(CoreError::Invalid { msg: "the hub file is locked by another process".into() })
    }

    pub fn join_token(&self) -> Result<String, CoreError> {
        Ok(self.read_hub_file()?.join_token)
    }

    /// A new join token; the old one stops working. Machines that already
    /// joined keep their own credentials.
    pub fn rotate_join_token(&self) -> Result<String, CoreError> {
        let _guard = self.lock.lock().unwrap();
        let _flock = self.hub_file_lock()?;
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
        // A blob reported as present is about to be relied on by a revision:
        // refresh its age so a collection in between does not take it.
        Ok(shas
            .iter()
            .filter(|s| {
                let path = self.blob_path(s);
                let present = path.is_file();
                if present {
                    touch(&path);
                }
                !present
            })
            .cloned()
            .collect())
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
                touch(&dest);
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

/// What an administrator sees of the hub as a whole.
#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub machines: usize,
    pub sessions: usize,
    pub revisions: usize,
    pub blobs: usize,
    pub blob_bytes: u64,
}

/// One session on the hub, as the admin list shows it.
#[derive(Debug, Clone, Serialize)]
pub struct AdminSession {
    pub agent: AgentKind,
    pub id: String,
    pub title: Option<String>,
    pub project: String,
    pub machine: Option<String>,
    pub rev: String,
    pub revisions: usize,
    pub size: u64,
    pub pushed_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminMachine {
    #[serde(flatten)]
    pub machine: Machine,
    /// Sessions whose current copy this machine pushed.
    pub sessions: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct GcReport {
    pub removed: usize,
    pub freed_bytes: u64,
    /// True when nothing was deleted: a preview.
    pub dry_run: bool,
}

/// A blob uploaded this recently may belong to a push that has not committed
/// its revision yet, so it is never collected.
const GC_GRACE_SECS: u64 = 3600;

/// One line of the administrator's own history on this hub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub at: Timestamp,
    pub action: String,
    pub target: String,
}

impl Hub {
    pub fn admin_enabled(&self) -> bool {
        self.read_hub_file().is_ok_and(|f| f.admin_token_sha256.is_some())
    }

    /// Mint an admin token (replacing any earlier one) and return it. Only
    /// its hash is kept, so this is the one time it can be shown.
    pub fn rotate_admin_token(&self) -> Result<String, CoreError> {
        let _guard = self.lock.lock().unwrap();
        let _flock = self.hub_file_lock()?;
        let mut file = self.read_hub_file()?;
        let token = format!("asma_{}", random_hex(32)?);
        file.admin_token_sha256 = Some(hex(&sha256(&token)));
        self.write_hub_file(&file)?;
        Ok(token)
    }

    /// `Ok` only for the admin token. A hub that never minted one refuses
    /// everything, as does a machine credential.
    pub fn check_admin(&self, token: &str) -> Result<(), HubError> {
        let stored = self.read_hub_file()?.admin_token_sha256.ok_or(HubError::Unauthorized)?;
        if ct_eq(&sha256(token), &decode_hex(&stored)) { Ok(()) } else { Err(HubError::Unauthorized) }
    }

    fn audit(&self, action: &str, target: &str) {
        let entry = AuditEntry { at: Timestamp::now(), action: action.into(), target: target.into() };
        if let Ok(line) = serde_json::to_string(&entry) {
            let path = self.root.join("admin.log");
            if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
                let _ = writeln!(file, "{line}");
                restrict(&path, 0o600);
            }
        }
    }

    /// The most recent administrative actions, newest first.
    pub fn audit_log(&self, limit: usize) -> Vec<AuditEntry> {
        // Only the tail: the log only grows, and a page asks for a few lines.
        let text = (|| {
            use std::io::{Read, Seek, SeekFrom};
            let mut file = fs::File::open(self.root.join("admin.log")).ok()?;
            let len = file.metadata().ok()?.len();
            let start = len.saturating_sub(256 * 1024);
            file.seek(SeekFrom::Start(start)).ok()?;
            let mut text = String::new();
            file.read_to_string(&mut text).ok()?;
            // A cut mid-line leaves a partial first record: drop it.
            if start > 0 {
                text = text.split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or_default();
            }
            Some(text)
        })()
        .unwrap_or_default();
        let mut entries: Vec<AuditEntry> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        entries.reverse();
        entries.truncate(limit);
        entries
    }

    pub fn stats(&self) -> Result<Stats, CoreError> {
        let (mut blobs, mut blob_bytes) = (0, 0);
        for shard in read_dirs(&self.root.join("blobs")) {
            if let Ok(entries) = fs::read_dir(&shard) {
                for e in entries.flatten() {
                    if let Ok(m) = e.metadata()
                        && m.is_file()
                    {
                        blobs += 1;
                        blob_bytes += m.len();
                    }
                }
            }
        }
        let (mut sessions, mut revisions) = (0, 0);
        for agent_dir in read_dirs(&self.root.join("sessions")) {
            for session_dir in read_dirs(&agent_dir) {
                sessions += 1;
                revisions += fs::read_dir(session_dir.join("revisions")).map(|d| d.flatten().count()).unwrap_or(0);
            }
        }
        Ok(Stats { machines: self.read_machines()?.len(), sessions, revisions, blobs, blob_bytes })
    }

    pub fn admin_sessions(&self) -> Result<Vec<AdminSession>, CoreError> {
        let mut out = Vec::new();
        for head in self.heads()? {
            let m = head.manifest;
            let revisions = fs::read_dir(self.session_dir(m.agent, &m.id).join("revisions"))
                .map(|d| d.flatten().count())
                .unwrap_or(0);
            out.push(AdminSession {
                agent: m.agent,
                id: m.id,
                title: m.title,
                project: m.git_origin.unwrap_or(m.project_root_portable),
                machine: m.machine.map(|x| x.name),
                rev: head.rev,
                revisions,
                size: head.total_size,
                pushed_at: m.pushed_at,
            });
        }
        Ok(out)
    }

    pub fn admin_machines(&self) -> Result<Vec<AdminMachine>, CoreError> {
        let heads = self.heads()?;
        Ok(self
            .machines()?
            .into_iter()
            .map(|machine| {
                let sessions = heads
                    .iter()
                    .filter(|h| h.manifest.machine.as_ref().is_some_and(|m| m.id == machine.id))
                    .count();
                AdminMachine { machine, sessions }
            })
            .collect())
    }

    /// Remove a machine (see `revoke`), on the record.
    pub fn admin_revoke(&self, who: &str) -> Result<Machine, HubError> {
        let removed = self.revoke(who)?;
        self.audit("revoke machine", &format!("{} ({})", removed.name, removed.id));
        Ok(removed)
    }

    pub fn admin_rotate_join_token(&self) -> Result<String, CoreError> {
        let token = self.rotate_join_token()?;
        self.audit("rotate join token", "");
        Ok(token)
    }

    /// Delete a session and every revision of it from the hub. Machines that
    /// still have it keep it, and will see it as not on the hub. Its files
    /// stay on disk until a collection removes the ones nothing else uses.
    pub fn delete_session(&self, agent: &str, id: &str) -> Result<usize, HubError> {
        let agent = AgentKind::parse(agent).ok_or(HubError::NotFound)?;
        if !valid_id(id) {
            return Err(HubError::NotFound);
        }
        let _guard = self.lock.lock().unwrap();
        let dir = self.session_dir(agent, id);
        if !dir.is_dir() {
            return Err(HubError::NotFound);
        }
        let revisions = fs::read_dir(dir.join("revisions")).map(|d| d.flatten().count()).unwrap_or(0);
        fsutil::remove_recursive(&dir)?;
        self.audit("delete session", &format!("{agent}:{id} ({revisions} revisions)"));
        Ok(revisions)
    }

    /// Remove the blobs no revision names (left by deleted sessions or failed
    /// pushes). `dry_run` only counts. Recent blobs are spared: a push
    /// uploads before it commits.
    pub fn gc(&self, dry_run: bool) -> Result<GcReport, CoreError> {
        // Hold the lock so no revision is committed while the references are
        // gathered; a blob a commit would need is either already referenced
        // or too new to touch.
        let _guard = self.lock.lock().unwrap();
        let mut referenced = std::collections::HashSet::new();
        for agent_dir in read_dirs(&self.root.join("sessions")) {
            for session_dir in read_dirs(&agent_dir) {
                let revisions = session_dir.join("revisions");
                // A revision that cannot be read or understood might be the
                // only thing naming some blob. Treating it as naming none
                // would delete data, so collecting stops instead.
                let entries = match fs::read_dir(&revisions) {
                    Ok(entries) => entries,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(CoreError::io(&revisions, e)),
                };
                for e in entries.flatten() {
                    let bytes = fs::read(e.path()).map_err(io(&e.path()))?;
                    let m: Manifest = serde_json::from_slice(&bytes).map_err(|err| CoreError::Invalid {
                        msg: format!("{} is unreadable ({err}); not collecting anything", e.path().display()),
                    })?;
                    referenced.extend(m.blob_shas().map(String::from));
                }
            }
        }
        let (mut removed, mut freed) = (0, 0);
        let now = std::time::SystemTime::now();
        for shard in read_dirs(&self.root.join("blobs")) {
            let Ok(entries) = fs::read_dir(&shard) else { continue };
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if referenced.contains(&name) || !valid_sha(&name) {
                    continue;
                }
                let Ok(meta) = e.metadata() else { continue };
                let age = meta.modified().ok().and_then(|t| now.duration_since(t).ok()).map_or(0, |d| d.as_secs());
                if age < GC_GRACE_SECS {
                    continue;
                }
                removed += 1;
                freed += meta.len();
                if !dry_run {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
        if !dry_run {
            self.audit("collect unused files", &format!("{removed} files, {freed} bytes"));
        }
        Ok(GcReport { removed, freed_bytes: freed, dry_run })
    }
}

/// Mark a file as just used. Failing to is harmless: it only keeps an old
/// file eligible for collection.
fn touch(path: &Path) {
    if let Ok(file) = fs::File::options().write(true).open(path) {
        let _ = file.set_modified(std::time::SystemTime::now());
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

    fn sha_of(text: &str) -> String {
        hex(&sha256(text))
    }

    fn put(hub: &Hub, text: &str) -> String {
        let sha = sha_of(text);
        hub.put_blob(&sha, text.as_bytes(), 1 << 20).unwrap();
        sha
    }

    fn age(hub: &Hub, sha: &str, secs: u64) {
        let path = hub.blob_path(sha);
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
        fs::File::options().write(true).open(path).unwrap().set_modified(when).unwrap();
    }

    /// Nothing is administrable until an admin token exists, and only that
    /// token opens it — not a machine's credential, not the join token.
    #[test]
    fn only_the_admin_token_opens_the_admin_api() {
        let (_d, hub) = hub();
        let joined = hub.join(&hub.join_token().unwrap(), "laptop").unwrap();
        assert!(!hub.admin_enabled());
        assert!(matches!(hub.check_admin("anything"), Err(HubError::Unauthorized)));

        let token = hub.rotate_admin_token().unwrap();
        assert!(hub.admin_enabled() && token.starts_with("asma_"));
        assert!(hub.check_admin(&token).is_ok());
        for wrong in [joined.credential.as_str(), hub.join_token().unwrap().as_str(), "", "asma_nope"] {
            assert!(matches!(hub.check_admin(wrong), Err(HubError::Unauthorized)), "{wrong:?}");
        }
        // Only the hash is stored, and a new token retires the old one.
        assert!(!fs::read_to_string(hub.hub_file()).unwrap().contains(&token));
        let newer = hub.rotate_admin_token().unwrap();
        assert!(matches!(hub.check_admin(&token), Err(HubError::Unauthorized)));
        assert!(hub.check_admin(&newer).is_ok());
        // The admin token is not a machine credential either.
        assert!(matches!(hub.authenticate(&newer), Err(HubError::Unauthorized)));
    }

    /// Deleting a session leaves its files; a collection removes the ones
    /// nothing else names, and only once they are old enough to be safe.
    #[test]
    fn collecting_removes_only_old_unreferenced_files() {
        let (_d, hub) = hub();
        let joined = hub.join(&hub.join_token().unwrap(), "laptop").unwrap();
        let machine = hub.authenticate(&joined.credential).unwrap();
        let kept = put(&hub, "kept");
        let orphan_old = put(&hub, "orphan-old");
        let orphan_new = put(&hub, "orphan-new");
        hub.put_revision("claude-code", ID, &manifest(None, &[&kept]), &machine).unwrap();
        age(&hub, &kept, 7200);
        age(&hub, &orphan_old, 7200);

        let preview = hub.gc(true).unwrap();
        assert_eq!((preview.removed, preview.dry_run), (1, true));
        assert!(hub.blob_path(&orphan_old).is_file(), "a preview deletes nothing");

        let done = hub.gc(false).unwrap();
        assert_eq!(done.removed, 1);
        assert!(!hub.blob_path(&orphan_old).is_file());
        assert!(hub.blob_path(&kept).is_file(), "referenced files stay");
        assert!(hub.blob_path(&orphan_new).is_file(), "a fresh upload may belong to a push in flight");

        // Delete the session: its file becomes collectable.
        assert_eq!(hub.delete_session("claude-code", ID).unwrap(), 1);
        assert!(hub.heads().unwrap().is_empty());
        assert_eq!(hub.gc(false).unwrap().removed, 1);
        assert!(!hub.blob_path(&kept).is_file());
        assert!(hub.audit_log(10).iter().any(|e| e.action == "delete session"));
        assert!(matches!(hub.delete_session("claude-code", ID), Err(HubError::NotFound)));
        assert!(matches!(hub.delete_session("claude-code", "../etc"), Err(HubError::NotFound)));
    }

    /// A revision the hub cannot understand might be the only thing naming a
    /// file, so collecting refuses rather than treating it as naming none.
    #[test]
    fn collecting_stops_when_a_revision_cannot_be_read() {
        let (_d, hub) = hub();
        let joined = hub.join(&hub.join_token().unwrap(), "laptop").unwrap();
        let machine = hub.authenticate(&joined.credential).unwrap();
        let kept = put(&hub, "kept");
        let rev = hub.put_revision("claude-code", ID, &manifest(None, &[&kept]), &machine).unwrap();
        age(&hub, &kept, 7200);
        let path = hub.session_dir(AgentKind::ClaudeCode, ID).join("revisions").join(format!("{rev}.json"));
        fs::write(&path, b"{ not json").unwrap();
        assert!(hub.gc(false).is_err());
        assert!(hub.gc(true).is_err());
        assert!(hub.blob_path(&kept).is_file(), "nothing was deleted");
    }

    /// A dedup hit or a presence check refreshes a file's age, so a
    /// collection cannot take what a push is about to rely on.
    #[test]
    fn a_file_a_push_reuses_is_not_collected_from_under_it() {
        let (_d, hub) = hub();
        let sha = put(&hub, "reused");
        age(&hub, &sha, 7200);
        assert!(hub.missing(std::slice::from_ref(&sha)).unwrap().is_empty());
        assert_eq!(hub.gc(true).unwrap().removed, 0, "presence check refreshed it");
        age(&hub, &sha, 7200);
        hub.put_blob(&sha, "reused".as_bytes(), 1 << 20).unwrap();
        assert_eq!(hub.gc(true).unwrap().removed, 0, "a dedup upload refreshed it");
    }

    /// `attach` leaves another process's in-flight uploads alone.
    #[test]
    fn attaching_does_not_sweep_uploads_in_flight() {
        let (d, _hub) = hub();
        let root = d.path().join("hub");
        let part = root.join("tmp").join("abc.part");
        fs::write(&part, b"half").unwrap();
        Hub::attach(&root).unwrap();
        assert!(part.is_file());
        Hub::open(&root).unwrap();
        assert!(!part.is_file());
    }

    #[test]
    fn stats_and_the_admin_lists_count_what_is_there() {
        let (_d, hub) = hub();
        let joined = hub.join(&hub.join_token().unwrap(), "laptop").unwrap();
        let machine = hub.authenticate(&joined.credential).unwrap();
        let a = put(&hub, "a");
        hub.put_revision("claude-code", ID, &manifest(None, &[&a]), &machine).unwrap();
        let st = hub.stats().unwrap();
        assert_eq!((st.machines, st.sessions, st.revisions, st.blobs), (1, 1, 1, 1));
        let machines = hub.admin_machines().unwrap();
        assert_eq!((machines[0].machine.name.as_str(), machines[0].sessions), ("laptop", 1));
        let sessions = hub.admin_sessions().unwrap();
        assert_eq!((sessions[0].id.as_str(), sessions[0].revisions), (ID, 1));
        assert_eq!(sessions[0].machine.as_deref(), Some("laptop"));
    }

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
