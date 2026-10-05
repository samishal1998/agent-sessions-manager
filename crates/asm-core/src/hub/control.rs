//! Remote control, a machine's half: ask the hub for commands, run the ones
//! this machine's owner allowed, say how they went.
//!
//! Nothing here trusts the hub more than a pull already does. A command is
//! one of two verbs (`push`, `pull`) about one session; the hub supplies no
//! path, argument vector or shell, and a pull installs where this machine's
//! own rules put it (the pusher's project path under this home, or nowhere:
//! a command never picks a directory). It is off until `asm control enable`
//! writes `control.json`, which only a person at this machine can do.
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
use super::commands::{Caps, Code, OPS, Op, PROTOCOL, Report, Work};
use super::manifest::{valid_id, valid_sha};
use super::store::Head;
use crate::adapter::SessionFilter;
use crate::bulk::ItemOutcome;
use crate::hub::bundle::InstallOutcome;
use crate::model::SessionStatus;
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
    /// Operations that may run here, a subset of `push`, `pull`.
    pub allow: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config { enabled: false, allow: OPS.iter().map(|o| o.as_str().to_string()).collect() }
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

fn find_local(agent: crate::model::AgentKind, id: &str) -> Option<crate::model::Session> {
    ops::resolve_ref(&key(agent, id), &SessionFilter { include_children: true, ..Default::default() }).ok()
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
    if find_local(work.agent, &work.session).is_none() {
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
    Ok(match pulled.installed.outcome {
        InstallOutcome::New => report(Code::Ok, format!("installed at {}", pulled.installed.project_root.display())),
        InstallOutcome::FastForward { .. } => report(Code::Ok, "brought up to date"),
        InstallOutcome::Replaced => report(Code::Ok, "overwritten with the hub's copy; the old one was backed up first"),
        InstallOutcome::InSync => report(Code::AlreadyApplied, "this machine already had that revision"),
        InstallOutcome::Ahead => report(Code::Ahead, "this machine has more than the revision being pulled"),
        InstallOutcome::Diverged => report(Code::Diverged, "both machines continued the session; nothing was changed"),
    })
}

/// Run one command and say how it went. Never panics on what the hub sent.
pub fn execute(remote: &Remote, work: &Work, config: &Config) -> Report {
    if !valid_id(&work.session) {
        return report(Code::Failed, "that is not a session id");
    }
    if !config.allows(work.op) {
        return report(Code::RemoteOff, "remote control does not allow that here");
    }
    let result = match work.op {
        Op::Push => run_push(remote, work),
        Op::Pull => run_pull(remote, work),
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
