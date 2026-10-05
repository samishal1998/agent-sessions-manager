//! Remote control, a machine's half: ask the hub for commands, run the ones
//! this machine's owner allowed, say how they went.
//!
//! Nothing here trusts the hub more than a pull already does. A command is
//! one of three verbs (`push`, `pull`, `archive`) about one session; the hub
//! supplies no path, argument vector or shell, and a pull installs where this
//! machine's own rules put it (the pusher's project path under this home, or
//! nowhere: a command never picks a directory). It is off until `asm control
//! enable` writes `control.json`, which only a person at this machine can do.
//! `archive` (the last step of a move) is on only when the owner listed it:
//! the default is `push` and `pull`.
//!
//! The daemon calls `Poller::step` between its passes. A command may take
//! longer than the loop's tick (a big upload): the loop is single-threaded,
//! so polling pauses meanwhile; the hub's lease outlasts it, and a result
//! that arrives late is still accepted.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::actions::{self, key};
use super::client::Remote;
use super::commands::{Caps, Code, Op, PROTOCOL, Report, Work};
use super::manifest::{files_hash, valid_id, valid_sha};
use super::state::{SyncState, Tracked};
use super::store::Head;
use crate::adapter::SessionFilter;
use crate::adapter::ArchiveOutcome;
use crate::bulk::ItemOutcome;
use crate::hub::bundle::InstallOutcome;
use crate::model::{AgentKind, Session, SessionLocation, SessionStatus};
use crate::{CoreError, fsutil, ops, paths};

/// How often the daemon asks while remote control is on.
pub const TICK_SECS: u64 = 5;
/// The longest it waits after the hub could not be reached.
const BACKOFF_MAX: u64 = 300;
/// A hub from before remote control: ask again in an hour.
const OLD_HUB_WAIT: u64 = 3600;
const RING: usize = 50;

fn invalid(msg: impl Into<String>) -> CoreError {
    CoreError::Invalid { msg: msg.into() }
}

fn dir() -> Result<PathBuf, CoreError> {
    paths::data_dir().ok_or_else(|| invalid("cannot determine asm data dir"))
}

/// What this machine's owner allows. Written only by `asm control enable` /
/// `disable`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub enabled: bool,
    /// Operations that may run here, a subset of `push`, `pull`, `archive`.
    pub allow: Vec<String>,
}

/// What `asm control enable` allows when the owner names nothing.
pub const DEFAULT_ALLOW: [Op; 2] = [Op::Push, Op::Pull];

impl Default for Config {
    fn default() -> Self {
        // `archive` is never part of the default: the owner asks for it.
        Config { enabled: false, allow: DEFAULT_ALLOW.iter().map(|o| o.as_str().to_string()).collect() }
    }
}

impl Config {
    fn path() -> Result<PathBuf, CoreError> {
        Ok(dir()?.join("control.json"))
    }

    /// Off when the file is missing or unreadable: a damaged config must
    /// never turn remote control on.
    pub fn load() -> Config {
        Self::path()
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), CoreError> {
        fsutil::write_atomic(&Self::path()?, &serde_json::to_vec_pretty(self).unwrap())
    }

    pub fn allows(&self, op: Op) -> bool {
        self.enabled && self.allow.iter().any(|a| a == op.as_str())
    }

    /// What this machine tells the hub on every poll.
    pub fn caps(&self) -> Caps {
        let ops = if self.enabled { self.allow.clone() } else { Vec::new() };
        Caps { v: PROTOCOL, ops, enabled: self.enabled }
    }
}

/// Turn remote control on here. Refused for a hub reached over plain HTTP
/// outside a private network: the commands themselves are not secret, but
/// the credential and every transcript they move would cross that path.
pub fn enable(hub_url: &str, allow: &[Op]) -> Result<Config, CoreError> {
    if hub_url.starts_with("http://") && !super::client::plain_http_is_private(hub_url) {
        return Err(invalid(format!(
            "{hub_url} is plain HTTP to an address outside loopback, a private LAN or a VPN; \
             put the hub behind HTTPS before letting it send this machine commands"
        )));
    }
    let config = Config { enabled: true, allow: allow.iter().map(|o| o.as_str().to_string()).collect() };
    config.save()?;
    Ok(config)
}

pub fn disable() -> Result<Config, CoreError> {
    let mut config = Config::load();
    config.enabled = false;
    config.save()?;
    Ok(config)
}

// What this machine did whose result the hub has not confirmed, so a command
// offered again (a lost reply, a lease that ran out) is answered, not run
// again. An entry goes as soon as the hub has the result: a command retried
// later, under the same id, must run.

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Done {
    id: String,
    /// `None` while it runs: a crash then re-runs it, which is safe because
    /// a push and a pull both end "already in sync" the second time.
    result: Option<Report>,
}

fn ring_path() -> Result<PathBuf, CoreError> {
    Ok(dir()?.join("control-done.json"))
}

fn ring_load() -> Vec<Done> {
    ring_path().ok().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn ring_save(ring: &mut Vec<Done>) {
    if ring.len() > RING {
        ring.drain(..ring.len() - RING);
    }
    if let Ok(path) = ring_path() {
        let _ = fsutil::write_atomic(&path, &serde_json::to_vec(ring).unwrap());
    }
}

fn report(code: Code, detail: impl Into<String>) -> Report {
    Report { code, detail: Some(detail.into()), rev: None }
}

/// Words in other modules' messages that decide a code. Pinned by a test, so
/// rewording one of them fails here instead of quietly turning a refusal
/// into a generic failure.
mod said {
    pub const NEWER: &str = "has a newer copy";
    pub const DIVERGED: &str = "diverged";
    pub const RACE: &str = "another machine pushed it while";
    pub const UNKNOWN_HISTORY: &str = "has no record of syncing it";
    pub const NO_DIR: &str = "does not exist on this machine";
    pub const CANNOT_RESTORE: &str = "cannot restore them onto";
}

/// The code and sentence for a push that did not go through, or did.
pub fn classify_push(outcome: &ItemOutcome, rev: Option<String>) -> Report {
    match outcome {
        ItemOutcome::Ok { note } if note == "in sync" => match rev {
            Some(rev) => Report { code: Code::InSync, detail: Some("the hub already had exactly this copy".into()), rev: Some(rev) },
            None => report(Code::Failed, "in sync, but the hub's revision could not be read"),
        },
        ItemOutcome::Ok { note } => match rev {
            Some(rev) => Report { code: Code::Ok, detail: Some(note.clone()), rev: Some(rev) },
            None => report(Code::Failed, "pushed, but the new revision could not be read"),
        },
        ItemOutcome::Skipped { reason } if reason.contains(said::NEWER) => report(Code::HubNewer, reason.clone()),
        ItemOutcome::Skipped { reason } => report(Code::Failed, reason.clone()),
        ItemOutcome::Failed { error } if error.contains(said::DIVERGED) => report(Code::Diverged, error.clone()),
        ItemOutcome::Failed { error } if error.contains(said::RACE) => report(Code::Conflict, error.clone()),
        ItemOutcome::Failed { error } if error.contains(said::UNKNOWN_HISTORY) => report(Code::HubNewer, error.clone()),
        ItemOutcome::Failed { error } => report(Code::Failed, error.clone()),
    }
}

/// The code and sentence for an error that stopped a command.
pub fn classify_error(e: &CoreError) -> Report {
    let text = e.to_string();
    let code = match e {
        CoreError::SessionLive { .. } | CoreError::StoreBusy { .. } => Code::Live,
        CoreError::NotSupported { .. } => Code::NotRestorable,
        _ if text.contains(said::NO_DIR) => Code::NoDir,
        _ if text.contains(said::CANNOT_RESTORE) => Code::NotRestorable,
        _ => Code::Failed,
    };
    report(code, text)
}

/// The session with exactly this id on this machine, archived or not. Never
/// a prefix or a name that happens to match: the id came from the hub, and
/// pushing, installing over or archiving some other session because it
/// starts the same would be the worst way to be helpful.
fn find_local(agent: AgentKind, id: &str) -> Option<Session> {
    let filter = SessionFilter { agent: Some(agent), include_children: true, ..Default::default() };
    pick(ops::list_sessions(&filter).ok()?, id)
}

fn pick(sessions: Vec<Session>, id: &str) -> Option<Session> {
    sessions.into_iter().find(|s| s.handle.native_id == id)
}

/// Whether asm's archive store holds an entry for the session (agents that
/// archive by moving their files).
fn in_archive_store(agent: AgentKind, id: &str) -> bool {
    // The manifest is written last: a directory a failed archive left half
    // made is not an archived session.
    paths::archive_dir(agent.as_str(), id).is_some_and(|d| d.join("manifest.json").is_file())
}

/// A copy archived on this machine is hidden here: installing onto it, or
/// calling the session "arrived" because it is there, would leave it hidden
/// everywhere. `found` is the session as listed, `in_store` whether the
/// archive store holds it.
fn archived_here(found: Option<&Session>, in_store: bool) -> Option<Report> {
    let archived = match found {
        Some(s) => s.status == SessionStatus::Archived,
        None => in_store,
    };
    archived.then(|| {
        // An agent asm cannot unarchive for (Codex archives in its own app)
        // is restored there.
        let restore = match found {
            Some(s) if !can_archive(s.handle.agent) => format!("restore it in {} there", s.handle.agent),
            _ => "asm unarchive it there".to_string(),
        };
        report(Code::ArchivedHere, format!("this machine has that session archived; {restore}, then retry. Nothing else was changed."))
    })
}

fn run_push(remote: &Remote, work: &Work) -> Result<Report, CoreError> {
    let Some(session) = find_local(work.agent, &work.session) else {
        return Ok(report(Code::Failed, "this machine does not have that session"));
    };
    if matches!(session.status, SessionStatus::Live { .. }) {
        return Ok(report(Code::Live, "the session is running on this machine"));
    }
    let k = key(work.agent, &work.session);
    let mut revs = HashMap::new();
    let pushed = actions::push_collecting(remote, &[session], false, work.exact.unwrap_or(false), &mut revs)?;
    let Some(item) = pushed.items.first() else {
        return Ok(report(Code::Failed, "nothing was pushed"));
    };
    Ok(classify_push(&item.outcome, revs.remove(&k)))
}

/// Whether an unattended install may put a session at `dir`: under this
/// home and not in a hidden folder. A person pulling at the keyboard may
/// choose any directory; a command from the hub may not.
fn unattended_dir_ok(dir: &std::path::Path) -> bool {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return false };
    let Ok(rest) = dir.strip_prefix(&home) else { return false };
    !rest.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
}

fn run_pull(remote: &Remote, work: &Work) -> Result<Report, CoreError> {
    let Some(rev) = work.rev.as_deref().filter(|r| valid_sha(r)) else {
        return Ok(report(Code::Failed, "the command names no revision to install"));
    };
    let Some(history) = remote.history(work.agent.as_str(), &work.session)? else {
        return Ok(report(Code::Failed, "that session is no longer on the hub"));
    };
    let manifest = if rev == history.head {
        history.manifest.clone()
    } else {
        match remote.revision(work.agent.as_str(), &work.session, rev)? {
            Some(m) => m,
            None => return Ok(report(Code::Failed, "that revision is no longer on the hub")),
        }
    };
    let found = find_local(work.agent, &work.session);
    if let Some(blocked) = archived_here(found.as_ref(), in_archive_store(work.agent, &work.session)) {
        return Ok(blocked);
    }
    if found.is_none() {
        let wanted = crate::ir::PortablePath(manifest.project_root_portable.clone()).resolve();
        if !wanted.is_dir() {
            return Ok(report(Code::NoDir, format!("{} {}", wanted.display(), said::NO_DIR)));
        }
        if !unattended_dir_ok(&wanted) {
            return Ok(report(
                Code::NoDir,
                format!("{} is outside this machine's home folder or hidden; pull it by hand with --project-dir", wanted.display()),
            ));
        }
    }
    let head = Head { rev: rev.to_string(), file_count: manifest.files.len(), total_size: manifest.total_size(), manifest };
    let pulled = actions::pull_head_rev(remote, &head, None, Some(rev))?;
    // Again, from what is on disk now: an install that "found the copy in
    // sync" must not be taken for one that is visible.
    let found = find_local(work.agent, &work.session);
    if let Some(blocked) = archived_here(found.as_ref(), in_archive_store(work.agent, &work.session)) {
        return Ok(blocked);
    }
    Ok(match pulled.installed.outcome {
        InstallOutcome::New => report(Code::Ok, format!("installed at {}", pulled.installed.project_root.display())),
        InstallOutcome::FastForward { .. } => report(Code::Ok, "brought up to date"),
        InstallOutcome::Replaced => report(Code::Ok, "overwritten with the hub's copy; the old one was backed up first"),
        InstallOutcome::InSync => report(Code::AlreadyApplied, "this machine already had that revision"),
        InstallOutcome::Ahead => report(Code::Ahead, "this machine has more than the revision being pulled"),
        InstallOutcome::Diverged => report(Code::Diverged, "both machines continued the session; nothing was changed"),
    })
}

/// An agent whose sessions can be restored elsewhere and archived here. A
/// property of the agent, not of what is installed on this machine, so the
/// hub asks it too before it plans a move.
pub fn can_archive(agent: AgentKind) -> bool {
    super::bundle::restorable(agent) && crate::adapter::Adapter::capabilities_for(agent).archive
}

/// Why this session may not be archived here, whatever the hub asked: the
/// same refusals `asm push --move` makes before it uploads. `None` is fine.
fn blocker(session: &Session, can_archive: bool) -> Option<Report> {
    if !can_archive {
        return Some(report(Code::NotArchivable, "cannot be archived here"));
    }
    // jcode keeps turns in a journal until it next loads the session; a
    // copy sent without them is not the whole session.
    if session.handle.agent == AgentKind::JCode
        && let SessionLocation::JsonlFile { path } = &session.handle.location
        && std::fs::metadata(crate::adapter::jcode::hub::journal_of(path)).is_ok_and(|m| m.len() > 0)
    {
        return Some(report(
            Code::NotArchivable,
            "jcode has turns it has not written into the session yet; resume it once so it writes its turns",
        ));
    }
    None
}

/// What this machine knows when the archive step of a move runs.
struct Here<'a> {
    /// This machine's name, for the sentences the admin reads.
    machine: &'a str,
    /// `agent:id`, to say what to unarchive.
    key: &'a str,
    /// The session as listed (`None`: not listed).
    found: Option<&'a Session>,
    /// Whether asm's archive store already holds it.
    in_store: bool,
    can_archive: bool,
    /// What this machine last agreed with the hub about it.
    tracked: Option<&'a Tracked>,
}

/// The archive step of a move, decided from what is on this machine now. The
/// archive happens only if this machine's own record shows the hub's copy
/// `rev` is the very copy that is here now, down to its sidecar files, and
/// the hub still holds it: anything else means work the destination does not
/// have. `hub_canonical` reads the conversation identity of `rev` on the hub,
/// `now` collects the session again (conversation identity, file list hash).
fn archive_step(
    here: Here,
    rev: &str,
    hub_canonical: impl FnOnce() -> Result<Option<String>, CoreError>,
    now: impl FnOnce(&Session) -> Result<(String, String), CoreError>,
    archiver: impl FnOnce(&Session) -> Result<ArchiveOutcome, CoreError>,
) -> Result<Report, CoreError> {
    let Here { machine, key, found, in_store, can_archive, tracked } = here;
    let already = || report(Code::AlreadyApplied, "already archived");
    let Some(session) = found else {
        return Ok(if in_store { already() } else { report(Code::Failed, "this machine does not have that session") });
    };
    match session.status {
        SessionStatus::Archived => return Ok(already()),
        SessionStatus::Live { .. } => return Ok(report(Code::Live, "the session is running on this machine")),
        SessionStatus::Idle => {}
    }
    if let Some(refusal) = blocker(session, can_archive) {
        return Ok(refusal);
    }
    // An older archived copy (from an earlier move away and back) holds the
    // archive's place: archiving this one would fail for good.
    if in_store {
        return Ok(report(
            Code::NotArchivable,
            format!(
                "an older archived copy of this session is in asm's archive on {machine} and in the way: remove that copy from the archive if you no longer need it, then retry"
            ),
        ));
    }
    let changed = |why: &str| report(Code::ChangedSinceMove, why.to_string());
    let after_the_copy = format!("the session changed on {machine} after the copy that was sent; nothing was archived");
    // Which copy this machine last synced is the record's word; whether the
    // session is still that copy is the content's, below. (Not the cheap
    // size-and-mtime fingerprint: a touch, a backup or an agent's trailing
    // write moves that without changing a byte of the session.)
    let Some(tracked) = tracked.filter(|t| t.hub_rev == rev) else {
        return Ok(changed(&after_the_copy));
    };
    // The record is of the push; the hub must still hold what it sent.
    if hub_canonical()?.as_deref() != Some(tracked.canonical.as_str()) {
        return Ok(changed("the hub no longer holds the copy that was sent; nothing was archived"));
    }
    // The whole bundle: the conversation and the sidecar files (subagents,
    // tool results, diffs).
    let (canonical, files) = now(session)?;
    if canonical != tracked.canonical || files != tracked.files {
        return Ok(changed(&after_the_copy));
    }
    archiver(session)?;
    Ok(report(Code::Ok, format!("archived on {machine}; asm unarchive {key} there brings it back")))
}

fn run_archive(remote: &Remote, work: &Work) -> Result<Report, CoreError> {
    let Some(rev) = work.rev.as_deref().filter(|r| valid_sha(r)) else {
        return Ok(report(Code::Failed, "the command names no revision whose copy to archive"));
    };
    let k = key(work.agent, &work.session);
    let found = find_local(work.agent, &work.session);
    let tracked = SyncState::load(remote)?.get(&k).cloned();
    let here = Here {
        machine: &remote.machine.name,
        key: &k,
        found: found.as_ref(),
        in_store: in_archive_store(work.agent, &work.session),
        can_archive: can_archive(work.agent),
        tracked: tracked.as_ref(),
    };
    archive_step(
        here,
        rev,
        || Ok(remote.revision(work.agent.as_str(), &work.session, rev)?.map(|m| m.canonical)),
        |s| {
            let bundle = super::bundle::collect(s)?;
            let files = files_hash(bundle.files.iter().map(|f| &f.entry));
            Ok((bundle.canonical, files))
        },
        ops::archive,
    )
}

/// Why this machine will not run `work` at all, whatever it says.
fn refusal(work: &Work, config: &Config) -> Option<Report> {
    if !config.enabled {
        return Some(report(Code::RemoteOff, "remote control is off on this machine"));
    }
    (!config.allows(work.op)).then(|| {
        report(
            Code::Unsupported,
            "this machine does not allow that kind of command: asm control enable --allow push,pull,archive",
        )
    })
}

/// Run one command and say how it went. Never panics on what the hub sent.
pub fn execute(remote: &Remote, work: &Work, config: &Config) -> Report {
    if !valid_id(&work.session) {
        return report(Code::Failed, "that is not a session id");
    }
    if let Some(refused) = refusal(work, config) {
        return refused;
    }
    let result = match work.op {
        Op::Push => run_push(remote, work),
        Op::Pull => run_pull(remote, work),
        Op::Archive => run_archive(remote, work),
    };
    result.unwrap_or_else(|e| classify_error(&e))
}

/// One poll: ask, and run what is waiting. `say` hears one line per command.
/// Returns how many were run.
pub fn step(remote: &Remote, say: &mut impl FnMut(&str)) -> Result<usize, CoreError> {
    let config = Config::load();
    let waiting = remote.inbox(&config.caps())?;
    if !config.enabled {
        return Ok(0);
    }
    let mut ring = ring_load();
    let mut ran = 0;
    for work in waiting {
        let label = format!("{} {}", work.op.as_str(), key(work.agent, &work.session));
        // Already done here: the result just was not delivered.
        if let Some(Done { result: Some(result), .. }) = ring.iter().find(|d| d.id == work.id) {
            if remote.report(&work.id, result).is_ok() {
                ring.retain(|d| d.id != work.id);
                ring_save(&mut ring);
            }
            continue;
        }
        // Offered again after this machine was killed while it ran (or never
        // heard back): claimed again and run again, which is safe — a second
        // push or pull of the same copy ends "already in sync".
        if remote.claim(&work.id)?.is_none() {
            continue;
        }
        if !ring.iter().any(|d| d.id == work.id) {
            ring.push(Done { id: work.id.clone(), result: None });
            ring_save(&mut ring);
        }
        say(&format!("running {label} for the hub"));
        let result = execute(remote, &work, &config);
        say(&format!(
            "{label}: {}{}",
            if result.code.succeeded() { "done" } else { "not done" },
            result.detail.as_deref().map(|d| format!(" — {d}")).unwrap_or_default()
        ));
        if let Some(entry) = ring.iter_mut().find(|d| d.id == work.id) {
            entry.result = Some(result.clone());
        }
        ring_save(&mut ring);
        remote.report(&work.id, &result)?;
        ring.retain(|d| d.id != work.id);
        ring_save(&mut ring);
        ran += 1;
    }
    Ok(ran)
}

/// When to ask next, and how to back off when the hub does not answer.
#[derive(Default)]
pub struct Poller {
    next: u64,
    failures: u32,
    said: Option<String>,
}

impl Poller {
    /// Poll if it is time. `idle` is how often to ask when remote control is
    /// off (just to say so); `now` is unix seconds.
    pub fn step(&mut self, remote: &Remote, now: u64, idle: u64, say: &mut impl FnMut(&str)) {
        if now < self.next {
            return;
        }
        let enabled = Config::load().enabled;
        match step(remote, say) {
            Ok(_) => {
                self.failures = 0;
                self.said = None;
                self.next = now + if enabled { TICK_SECS } else { idle };
            }
            Err(e) => {
                let text = e.to_string();
                let old_hub = text.contains("does not offer remote control");
                self.failures += 1;
                let wait = if old_hub {
                    OLD_HUB_WAIT
                } else {
                    (TICK_SECS << self.failures.min(6)).min(BACKOFF_MAX)
                };
                self.next = now + wait;
                // Said once per distinct problem, and never for an old hub
                // (it is not a fault, only a hub that cannot do this).
                if !old_hub && self.said.as_deref() != Some(text.as_str()) {
                    say(&format!("remote control: {text}"));
                    self.said = Some(text);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REV: &str = "abababababababababababababababababababababababababababababababab";

    fn session(agent: AgentKind, path: &std::path::Path, status: SessionStatus) -> Session {
        Session {
            handle: crate::model::SessionRef {
                agent,
                native_id: "s1".into(),
                location: SessionLocation::JsonlFile { path: path.to_path_buf() },
            },
            title: None,
            slug: None,
            project_root: PathBuf::from("/p"),
            git_branch: None,
            created: None,
            updated: None,
            model: None,
            usage: Default::default(),
            status,
            parent: None,
            agent_version: None,
            size_bytes: None,
        }
    }

    /// A transcript on disk, the session over it, and the record a push
    /// of exactly that file would have left.
    fn synced(dir: &std::path::Path, agent: AgentKind) -> (Session, Tracked) {
        let path = dir.join("s1.jsonl");
        std::fs::write(&path, "{\"type\":\"user\"}\n").unwrap();
        let s = session(agent, &path, SessionStatus::Idle);
        let t = Tracked {
            hub_rev: REV.into(),
            canonical: "c".into(),
            fingerprint: super::super::bundle::fingerprint(&s),
            files: String::new(),
        };
        (s, t)
    }

    /// What the hub and a fresh collect say, as `synced` leaves them.
    struct World<'a> {
        hub: Option<&'a str>,
        now: (&'a str, &'a str),
    }
    const AS_SENT: World = World { hub: Some("c"), now: ("c", "") };

    /// Runs the archive step and says whether the archiver was reached.
    fn step_in(w: World, s: Option<&Session>, in_store: bool, can: bool, t: Option<&Tracked>, rev: &str) -> (Report, bool) {
        let mut archived = false;
        let here = Here { machine: "alpha", key: "claude-code:s1", found: s, in_store, can_archive: can, tracked: t };
        let r = archive_step(
            here,
            rev,
            || Ok(w.hub.map(String::from)),
            |_| Ok((w.now.0.to_string(), w.now.1.to_string())),
            |_| {
                archived = true;
                Ok(ArchiveOutcome { archived_to: None })
            },
        )
        .unwrap();
        (r, archived)
    }

    fn step_of(s: Option<&Session>, in_store: bool, can: bool, t: Option<&Tracked>, rev: &str) -> (Report, bool) {
        step_in(AS_SENT, s, in_store, can, t, rev)
    }

    #[test]
    fn archive_is_opt_in_and_never_in_the_default() {
        let c = Config { enabled: true, ..Config::default() };
        assert!(c.allows(Op::Push) && c.allows(Op::Pull) && !c.allows(Op::Archive));
        assert_eq!(c.caps().ops, vec!["push".to_string(), "pull".to_string()]);
        let on = Config { enabled: true, allow: vec!["push".into(), "archive".into()] };
        assert!(on.allows(Op::Archive) && !on.allows(Op::Pull));
        assert_eq!(on.caps().ops, vec!["push".to_string(), "archive".to_string()]);
        let off = Config { enabled: false, allow: vec!["archive".into()] };
        assert!(!off.allows(Op::Archive) && off.caps().ops.is_empty(), "off allows nothing");
    }

    #[test]
    fn a_session_unchanged_since_the_copy_that_was_sent_is_archived() {
        let dir = tempfile::tempdir().unwrap();
        let (s, t) = synced(dir.path(), AgentKind::ClaudeCode);
        let (r, archived) = step_of(Some(&s), false, true, Some(&t), REV);
        assert_eq!(r.code, Code::Ok, "{r:?}");
        assert!(archived);
        assert_eq!(r.detail.as_deref(), Some("archived on alpha; asm unarchive claude-code:s1 there brings it back"));
    }

    #[test]
    fn a_session_changed_since_the_copy_was_sent_is_not_archived() {
        let dir = tempfile::tempdir().unwrap();
        let (s, t) = synced(dir.path(), AgentKind::ClaudeCode);
        // Continued after the push: the bundle's content is no longer the one sent.
        std::fs::write(dir.path().join("s1.jsonl"), "{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n").unwrap();
        let (r, archived) = step_in(World { hub: Some("c"), now: ("continued", "") }, Some(&s), false, true, Some(&t), REV);
        assert_eq!((r.code, archived), (Code::ChangedSinceMove, false));
        assert_eq!(
            r.detail.as_deref(),
            Some("the session changed on alpha after the copy that was sent; nothing was archived")
        );
    }

    /// A touch, a backup or an agent's trailing write moves the file's size or
    /// mtime without changing the session: that must not block a move.
    #[test]
    fn a_session_that_was_only_touched_is_still_archived() {
        let dir = tempfile::tempdir().unwrap();
        let (s, t) = synced(dir.path(), AgentKind::ClaudeCode);
        std::fs::write(dir.path().join("s1.jsonl"), "{\"type\":\"user\"}\n\n").unwrap();
        assert_ne!(t.fingerprint, super::super::bundle::fingerprint(&s), "the cheap fingerprint did move");
        let (r, archived) = step_of(Some(&s), false, true, Some(&t), REV);
        assert_eq!((r.code, archived), (Code::Ok, true), "{r:?}");
    }

    #[test]
    fn a_different_hub_revision_or_no_record_is_not_archived() {
        let dir = tempfile::tempdir().unwrap();
        let (s, t) = synced(dir.path(), AgentKind::ClaudeCode);
        let other = "cd".repeat(32);
        let (r, archived) = step_of(Some(&s), false, true, Some(&t), &other);
        assert_eq!((r.code, archived), (Code::ChangedSinceMove, false), "pushed since: hub_rev differs");
        let (r, archived) = step_of(Some(&s), false, true, None, REV);
        assert_eq!((r.code, archived), (Code::ChangedSinceMove, false), "never synced from here");
    }

    #[test]
    fn archiving_twice_is_fine_and_a_running_session_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, t) = synced(dir.path(), AgentKind::ClaudeCode);
        // Gone from the listing but in the archive store (Claude's way).
        let (r, archived) = step_of(None, true, true, Some(&t), REV);
        assert_eq!((r.code, archived), (Code::AlreadyApplied, false));
        assert_eq!(r.detail.as_deref(), Some("already archived"));
        // Still listed, flagged archived (OpenCode's way), even with no record.
        s.status = SessionStatus::Archived;
        let (r, archived) = step_of(Some(&s), false, true, None, REV);
        assert_eq!((r.code, archived), (Code::AlreadyApplied, false));
        s.status = SessionStatus::Live { pid: Some(1) };
        let (r, archived) = step_of(Some(&s), false, true, Some(&t), REV);
        assert_eq!((r.code, archived), (Code::Live, false));
        let (r, _) = step_of(None, false, true, Some(&t), REV);
        assert_eq!(r.code, Code::Failed, "neither listed nor archived");
    }

    #[test]
    fn an_agent_that_cannot_archive_is_refused_before_anything_else() {
        let dir = tempfile::tempdir().unwrap();
        let (s, t) = synced(dir.path(), AgentKind::ClaudeCode);
        let (r, archived) = step_of(Some(&s), false, false, Some(&t), REV);
        assert_eq!((r.code, archived), (Code::NotArchivable, false));
        assert_eq!(r.detail.as_deref(), Some("cannot be archived here"));
    }

    #[test]
    fn a_jcode_session_with_unwritten_turns_is_refused_until_it_is_resumed() {
        let dir = tempfile::tempdir().unwrap();
        let (s, t) = synced(dir.path(), AgentKind::JCode);
        let journal = dir.path().join("s1.journal.jsonl");
        assert_eq!(crate::adapter::jcode::hub::journal_of(&dir.path().join("s1.jsonl")), journal);
        // No journal, or an empty one: archived.
        assert_eq!(step_of(Some(&s), false, true, Some(&t), REV).0.code, Code::Ok);
        std::fs::write(&journal, "").unwrap();
        assert_eq!(step_of(Some(&s), false, true, Some(&t), REV).0.code, Code::Ok);
        std::fs::write(&journal, "{\"turn\":1}\n").unwrap();
        // The journal is part of jcode's fingerprint, so this also changed;
        // the refusal still names the real reason, and comes first.
        let (r, archived) = step_of(Some(&s), false, true, Some(&t), REV);
        assert_eq!((r.code, archived), (Code::NotArchivable, false));
        assert!(r.detail.unwrap().contains("resume it once"));
        // Another agent's sidecar by the same name means nothing.
        let (c, _) = synced(dir.path(), AgentKind::ClaudeCode);
        assert!(blocker(&c, true).is_none());
    }

    #[test]
    fn the_archive_step_is_not_trusted_to_a_revision_that_is_not_a_sha() {
        // run_archive's own guard, shared with pull's.
        assert!(!valid_sha("main") && !valid_sha(""));
        assert!(valid_sha(REV));
    }

    #[test]
    fn a_change_to_a_sidecar_alone_is_a_change() {
        // The transcript (and so the fingerprint) is as it was; a subagent
        // or tool-result file is not.
        let dir = tempfile::tempdir().unwrap();
        let (s, t) = synced(dir.path(), AgentKind::ClaudeCode);
        let moved = World { hub: Some("c"), now: ("c", "other files") };
        let (r, archived) = step_in(moved, Some(&s), false, true, Some(&t), REV);
        assert_eq!((r.code, archived), (Code::ChangedSinceMove, false), "{r:?}");
        let moved = World { hub: Some("c"), now: ("other conversation", "") };
        let (r, archived) = step_in(moved, Some(&s), false, true, Some(&t), REV);
        assert_eq!((r.code, archived), (Code::ChangedSinceMove, false), "{r:?}");
    }

    #[test]
    fn an_older_archived_copy_in_the_way_is_said_before_anything_is_touched() {
        let dir = tempfile::tempdir().unwrap();
        let (s, t) = synced(dir.path(), AgentKind::ClaudeCode);
        let (r, archived) = step_of(Some(&s), true, true, Some(&t), REV);
        assert_eq!((r.code, archived), (Code::NotArchivable, false));
        assert_eq!(
            r.detail.as_deref(),
            Some("an older archived copy of this session is in asm's archive on alpha and in the way: remove that copy from the archive if you no longer need it, then retry")
        );
    }

    #[test]
    fn nothing_is_archived_unless_the_hub_still_holds_the_copy_that_was_sent() {
        let dir = tempfile::tempdir().unwrap();
        let (s, t) = synced(dir.path(), AgentKind::ClaudeCode);
        for hub in [None, Some("another conversation")] {
            let (r, archived) = step_in(World { hub, now: ("c", "") }, Some(&s), false, true, Some(&t), REV);
            assert_eq!((r.code, archived), (Code::ChangedSinceMove, false), "{hub:?}");
            assert_eq!(r.detail.as_deref(), Some("the hub no longer holds the copy that was sent; nothing was archived"));
        }
    }

    #[test]
    fn a_copy_archived_on_the_destination_is_not_a_pull_that_worked() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _) = synced(dir.path(), AgentKind::OpenCode);
        // Listed, flagged archived (OpenCode's way).
        s.status = SessionStatus::Archived;
        let r = archived_here(Some(&s), false).expect("blocked");
        assert_eq!(r.code, Code::ArchivedHere);
        assert!(!r.code.succeeded());
        assert_eq!(
            r.detail.as_deref(),
            Some("this machine has that session archived; asm unarchive it there, then retry. Nothing else was changed.")
        );
        assert_eq!(serde_json::to_string(&r.code).unwrap(), "\"archived_here\"");
        // Not listed, held by asm's archive store (Claude's way).
        assert_eq!(archived_here(None, true).map(|r| r.code), Some(Code::ArchivedHere));
        // Not here at all, or here and visible: a pull goes ahead.
        assert!(archived_here(None, false).is_none());
        s.status = SessionStatus::Idle;
        assert!(archived_here(Some(&s), false).is_none());
        assert!(archived_here(Some(&s), true).is_none(), "a live copy beside an old archived one is the archive step's concern");
    }

    #[test]
    fn only_the_session_with_that_exact_id_is_ever_acted_on() {
        let dir = tempfile::tempdir().unwrap();
        let (mut longer, _) = synced(dir.path(), AgentKind::ClaudeCode);
        longer.handle.native_id = "s10".into();
        let (exact, _) = synced(dir.path(), AgentKind::ClaudeCode);
        // `s1` is a prefix of `s10`: a prefix lookup would take either (or refuse both).
        assert_eq!(pick(vec![longer.clone(), exact.clone()], "s1").unwrap().handle.native_id, "s1");
        assert!(pick(vec![longer], "s1").is_none(), "a prefix is not the session");
        assert!(pick(vec![], "s1").is_none());
    }

    #[test]
    fn which_agents_can_be_moved_does_not_depend_on_what_is_installed() {
        assert!(can_archive(AgentKind::ClaudeCode) && can_archive(AgentKind::OpenCode) && can_archive(AgentKind::JCode));
        assert!(!can_archive(AgentKind::Codex) && !can_archive(AgentKind::Antigravity));
    }

    #[test]
    fn a_machine_says_why_it_will_not_run_a_command() {
        let work = |op| Work { id: "i".into(), op, agent: AgentKind::ClaudeCode, session: "s1".into(), rev: None, exact: None };
        let on = Config { enabled: true, ..Config::default() };
        assert!(refusal(&work(Op::Push), &on).is_none());
        let r = refusal(&work(Op::Archive), &on).unwrap();
        assert_eq!(r.code, Code::Unsupported, "on, but not allowed that: not 'off'");
        assert_eq!(
            r.detail.as_deref(),
            Some("this machine does not allow that kind of command: asm control enable --allow push,pull,archive")
        );
        let off = Config { enabled: false, ..Config::default() };
        assert_eq!(refusal(&work(Op::Push), &off).unwrap().code, Code::RemoteOff);
    }

    #[test]
    fn it_is_off_unless_the_owner_turned_it_on() {
        let c = Config::default();
        assert!(!c.enabled);
        assert!(!c.allows(Op::Push) && !c.allows(Op::Pull));
        assert!(c.caps().ops.is_empty(), "off reports no operations");
        let on = Config { enabled: true, allow: vec!["pull".into()] };
        assert!(on.allows(Op::Pull) && !on.allows(Op::Push));
        assert_eq!(on.caps().ops, vec!["pull".to_string()]);
    }

    #[test]
    fn a_push_result_becomes_the_right_code() {
        let rev = Some("r".repeat(64));
        let ok = classify_push(&ItemOutcome::Ok { note: "pushed 2 KB (2 KB uploaded)".into() }, rev.clone());
        assert_eq!((ok.code, ok.rev.is_some()), (Code::Ok, true));
        assert_eq!(classify_push(&ItemOutcome::Ok { note: "in sync".into() }, rev.clone()).code, Code::InSync);
        assert_eq!(classify_push(&ItemOutcome::Ok { note: "pushed".into() }, None).code, Code::Failed, "no revision, no success");
        let skipped = ItemOutcome::Skipped { reason: "beta has a newer copy; `asm pull` it".into() };
        assert_eq!(classify_push(&skipped, None).code, Code::HubNewer);
        let diverged = ItemOutcome::Failed { error: "diverged: this machine and beta both continued it".into() };
        assert_eq!(classify_push(&diverged, None).code, Code::Diverged);
        let raced = ItemOutcome::Failed { error: "another machine pushed it while this one was; run push again".into() };
        assert_eq!(classify_push(&raced, None).code, Code::Conflict);
        assert_eq!(classify_push(&ItemOutcome::Failed { error: "boom".into() }, None).code, Code::Failed);
    }

    /// The sentences the codes depend on still exist where they are written.
    #[test]
    fn the_messages_the_codes_depend_on_are_still_what_other_modules_say() {
        let actions = include_str!("actions.rs");
        for phrase in [said::NEWER, said::DIVERGED, said::RACE, said::UNKNOWN_HISTORY] {
            assert!(actions.contains(phrase), "actions.rs no longer says {phrase:?}");
        }
        assert!(include_str!("bundle.rs").contains(said::NO_DIR));
        assert!(actions.contains(said::CANNOT_RESTORE));
    }

    #[test]
    fn errors_map_to_codes() {
        let live = CoreError::SessionLive { id: "x".into(), pid: Some(1) };
        assert_eq!(classify_error(&live).code, Code::Live);
        let nodir = invalid("/x/y does not exist on this machine; pass --project-dir to put the session somewhere else");
        assert_eq!(classify_error(&nodir).code, Code::NoDir);
        assert_eq!(classify_error(&invalid("whatever")).code, Code::Failed);
    }

    #[test]
    fn an_unattended_install_stays_under_home_and_out_of_hidden_folders() {
        let home = std::env::var_os("HOME").map(PathBuf::from).expect("HOME");
        assert!(unattended_dir_ok(&home.join("code/app")));
        assert!(!unattended_dir_ok(&home.join(".ssh")), "hidden at the top");
        assert!(!unattended_dir_ok(&home.join("code/.git/hooks")), "hidden below");
        assert!(!unattended_dir_ok(std::path::Path::new("/etc")));
        assert!(!unattended_dir_ok(std::path::Path::new("/tmp/x")));
    }

    #[test]
    fn a_failing_hub_is_backed_off_and_an_old_hub_is_left_alone_for_an_hour() {
        // The schedule only: no hub is called.
        let wait = |failures: u32| (TICK_SECS << failures.min(6)).min(BACKOFF_MAX);
        assert_eq!(wait(1), 10);
        assert_eq!(wait(20), 300);
        assert_eq!(OLD_HUB_WAIT, 3600);
    }
}
