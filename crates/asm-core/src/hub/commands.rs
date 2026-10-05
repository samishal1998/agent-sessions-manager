//! Remote control, the hub's half: a queue of small typed commands, one list
//! per target machine.
//!
//! The hub never calls into a machine (they are behind NAT, and a hub that
//! could run things on its machines would be a command-and-control server).
//! A machine's daemon *asks*: it polls `inbox`, `claim`s a command, runs it
//! with its own credential and `report`s the result. What a command can be
//! is a closed enum with typed arguments — never a path, an argv or a shell.
//! The machine validates everything again and only runs what its owner
//! allowed (`control.rs`); the hub is where an administrator *asks*.
//!
//! ```text
//! <root>/commands.json   [ Command ]   0600, one mutex, atomic rewrite
//! <root>/commands.log    one JSON line per state change, never the detail
//! ```
//!
//! A command is `queued` until a machine claims it (a 10-minute lease, so a
//! machine that dies mid-command gives it back), `running` while claimed, and
//! ends `ok`, `blocked` (the machine refused or failed, with a code and a
//! sentence), `cancelled` or `expired` (nobody picked it up for a week).
//! Only a lost lease, a lost race or a network error retries on its own, at
//! most three times; a refusal is never retried automatically.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::manifest::{MachineRef, valid_id, valid_sha};
use super::store::{Hub, HubError, Machine, MachineRecord, ct_eq, decode_hex, hex, random_hex, sha256};
use crate::CoreError;
use crate::model::AgentKind;

/// Wire version of the inbox protocol.
pub const PROTOCOL: u32 = 1;
const TTL_SECS: i64 = 7 * 24 * 3600;
const LEASE_SECS: i64 = 600;
const MAX_ATTEMPTS: u32 = 3;
/// Commands waiting for one machine, so a runaway script cannot bury it.
const QUEUE_DEPTH: usize = 20;
/// Finished commands are kept this long, for the history on the admin page.
const PRUNE_SECS: i64 = 14 * 24 * 3600;
pub const DETAIL_MAX: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Push,
    Pull,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Push => "push",
            Op::Pull => "pull",
        }
    }
}

/// Every operation this build can run. A machine reports these.
pub const OPS: [Op; 2] = [Op::Push, Op::Pull];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Running,
    Ok,
    Blocked,
    Cancelled,
    Expired,
}

impl State {
    pub fn is_final(self) -> bool {
        !matches!(self, State::Queued | State::Running)
    }
}

/// Why a command ended the way it did. The sentence beside it is the
/// machine's; the meaning of each code is fixed, so a UI can say what to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    Ok,
    /// Pushed nothing: the hub already held exactly this copy.
    InSync,
    /// Pulled nothing: this machine already had that revision.
    AlreadyApplied,
    Diverged,
    Ahead,
    Live,
    NoDir,
    NotRestorable,
    HubNewer,
    Conflict,
    Failed,
    RemoteOff,
    Unsupported,
    Expired,
    Cancelled,
}

impl Code {
    pub fn succeeded(self) -> bool {
        matches!(self, Code::Ok | Code::InSync | Code::AlreadyApplied)
    }
}

/// What a machine says it can and may do, sent with every poll.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    pub v: u32,
    pub ops: Vec<String>,
    pub enabled: bool,
}

/// What an administrator sees of a machine's remote control.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteStatus {
    pub enabled: bool,
    pub ops: Vec<String>,
    pub v: u32,
    /// When the machine last asked for its commands.
    pub polled_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Args {
    /// Push: read the whole session and settle for nothing less than the hub
    /// holding exactly this copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Command {
    pub id: String,
    pub op: Op,
    pub agent: AgentKind,
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The machine that runs it.
    pub machine: MachineRef,
    /// Pull: the machine whose push this installs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<MachineRef>,
    /// Pull: the exact revision to install, fixed when the command was made,
    /// so a third machine pushing in between cannot change what arrives.
    /// Push: the revision it produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    #[serde(default)]
    pub args: Args,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Code>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub attempts: u32,
    pub created: Timestamp,
    pub updated: Timestamp,
    pub expires: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lease_until: Option<Timestamp>,
    /// The machine that claimed it last: only it may report the result, even
    /// after its lease ran out and the command went back in the queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claimer: Option<String>,
    #[serde(default)]
    pub cancel_requested: bool,
    pub created_by: String,
}

/// A command as the machine that must run it sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Work {
    pub id: String,
    pub op: Op,
    pub agent: AgentKind,
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact: Option<bool>,
}

impl From<&Command> for Work {
    fn from(c: &Command) -> Self {
        Work { id: c.id.clone(), op: c.op, agent: c.agent, session: c.session.clone(), rev: c.rev.clone(), exact: c.args.exact }
    }
}

/// A request to run something on a machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCommand {
    pub op: Op,
    /// Machine id, or its name when that is unique.
    pub machine: String,
    pub agent: String,
    pub session: String,
    /// Pull: the machine that pushed the copy to install (id or name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact: Option<bool>,
}

/// A machine's result.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Report {
    pub code: Code,
    #[serde(default)]
    pub detail: Option<String>,
    /// Push: the revision now on the hub.
    #[serde(default)]
    pub rev: Option<String>,
}

fn now() -> Timestamp {
    Timestamp::now()
}

fn after(secs: i64) -> Timestamp {
    Timestamp::from_second(now().as_second() + secs).unwrap_or_else(|_| now())
}

fn bad(msg: impl Into<String>) -> HubError {
    HubError::BadRequest(msg.into())
}

/// Cut to a limit on a character boundary.
fn clip(text: &str) -> String {
    text.chars().take(DETAIL_MAX).collect()
}

fn find<'a>(machines: &'a [MachineRecord], who: &str) -> Result<&'a MachineRecord, HubError> {
    let hits: Vec<&MachineRecord> = machines.iter().filter(|m| m.id == who || m.name == who).collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(bad(format!("no machine on this hub is called {who:?}"))),
        many => Err(bad(format!("{who:?} names {} machines; use the machine's id", many.len()))),
    }
}

fn mref(m: &MachineRecord) -> MachineRef {
    MachineRef { id: m.id.clone(), name: m.name.clone() }
}

impl Hub {
    fn commands_file(&self) -> PathBuf {
        self.root().join("commands.json")
    }

    fn read_commands(&self) -> Result<Vec<Command>, CoreError> {
        let path = self.commands_file();
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| CoreError::Invalid { msg: format!("{} is unreadable: {e}", path.display()) }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(CoreError::io(&path, e)),
        }
    }

    fn write_commands(&self, commands: &[Command]) -> Result<(), CoreError> {
        self.write_private(&self.commands_file(), &serde_json::to_vec_pretty(commands).unwrap())
    }

    /// One line per state change, without the detail (it is machine-written
    /// text and has no business crowding the administrator's own log).
    fn command_log(&self, event: &str, c: &Command) {
        let line = serde_json::json!({
            "at": now(), "event": event, "id": c.id, "op": c.op,
            "machine": c.machine.id, "agent": c.agent, "session": c.session, "code": c.code,
        });
        let path = self.root().join("commands.log");
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(file, "{line}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
            }
        }
    }

    /// Mint the commands token (replacing any earlier one). It opens only the
    /// commands API — not the admin one, which can delete sessions.
    pub fn rotate_commands_token(&self) -> Result<String, CoreError> {
        let _guard = self.lock.lock().unwrap();
        let _flock = self.hub_file_lock()?;
        let mut file = self.read_hub_file()?;
        let token = format!("asmk_{}", random_hex(32)?);
        file.commands_token_sha256 = Some(hex(&sha256(&token)));
        self.write_hub_file(&file)?;
        self.audit("mint commands token", "");
        Ok(token)
    }

    /// Which of the two tokens that may use the commands API this is (named
    /// for the record): the commands token, or the admin token, which can
    /// already do everything this can.
    pub fn check_commands(&self, token: &str) -> Result<&'static str, HubError> {
        let file = self.read_hub_file()?;
        let presented = sha256(token);
        let ok = |stored: &Option<String>| stored.as_ref().is_some_and(|s| ct_eq(&presented, &decode_hex(s)));
        // Both compared, so timing does not say which token matched.
        let (c, a) = (ok(&file.commands_token_sha256), ok(&file.admin_token_sha256));
        match (c, a) {
            (true, _) => Ok("commands-token"),
            (_, true) => Ok("admin"),
            _ => Err(HubError::Unauthorized),
        }
    }

    /// Whether commands can be created at all on this hub.
    pub fn commands_enabled(&self) -> bool {
        self.read_hub_file().is_ok_and(|f| f.commands_token_sha256.is_some() || f.admin_token_sha256.is_some())
    }

    /// What a machine last said about remote control.
    pub fn remote_status(&self, machine_id: &str) -> Option<RemoteStatus> {
        let caps = self.read_machines().ok()?.into_iter().find(|m| m.id == machine_id)?.caps?;
        let polled_at = self.polled.lock().unwrap().get(machine_id).copied();
        Some(RemoteStatus { enabled: caps.enabled, ops: caps.ops, v: caps.v, polled_at })
    }

    /// Lease timeouts, expiry and pruning, applied whenever the list is
    /// about to be read or changed. True if anything moved.
    fn sweep(commands: &mut Vec<Command>) -> bool {
        let t = now();
        let mut changed = false;
        for c in commands.iter_mut() {
            match c.state {
                State::Queued if c.expires < t => {
                    c.state = State::Expired;
                    c.code = Some(Code::Expired);
                    c.detail = Some("the machine did not pick this up in time".into());
                    c.updated = t;
                    changed = true;
                }
                State::Running if c.lease_until.is_some_and(|l| l < t) => {
                    if c.attempts >= MAX_ATTEMPTS {
                        c.state = State::Blocked;
                        c.code = Some(Code::Failed);
                        c.detail = Some(format!("no result after {MAX_ATTEMPTS} attempts"));
                    } else {
                        // Given back. `claimer` stays: a late result from the
                        // machine that did the work is still accepted.
                        c.state = State::Queued;
                    }
                    c.lease_until = None;
                    c.updated = t;
                    changed = true;
                }
                _ => {}
            }
        }
        let before = commands.len();
        let cutoff = t.as_second() - PRUNE_SECS;
        commands.retain(|c| !(c.state.is_final() && c.updated.as_second() < cutoff));
        changed || commands.len() != before
    }

    /// The commands, newest first.
    pub fn commands(&self, limit: usize) -> Result<Vec<Command>, CoreError> {
        let _guard = self.lock.lock().unwrap();
        let mut all = self.read_commands()?;
        if Self::sweep(&mut all) {
            self.write_commands(&all)?;
        }
        all.sort_by_key(|c| std::cmp::Reverse(c.created));
        all.truncate(limit);
        Ok(all)
    }

    pub fn command(&self, id: &str) -> Result<Command, HubError> {
        self.commands(usize::MAX)?.into_iter().find(|c| c.id == id).ok_or(HubError::NotFound)
    }

    /// Check that a machine is willing and able to run `op`.
    fn check_willing(target: &MachineRecord, op: Op) -> Result<(), HubError> {
        let name = &target.name;
        let Some(caps) = &target.caps else {
            return Err(bad(format!(
                "{name} has not reported remote control: it needs `asm control enable` and a running \
                 `asm daemon` (version 0.10 or later)"
            )));
        };
        if !caps.enabled {
            return Err(bad(format!("{name} has not turned remote control on (`asm control enable` there)")));
        }
        if !caps.ops.iter().any(|o| o == op.as_str()) {
            return Err(bad(format!("{name} does not allow {} commands", op.as_str())));
        }
        Ok(())
    }

    /// Ask a machine to do something. `by` says who asked, for the record.
    pub fn enqueue(&self, request: NewCommand, by: &str) -> Result<Command, HubError> {
        let agent = AgentKind::parse(&request.agent).ok_or_else(|| bad(format!("unknown agent {:?}", request.agent)))?;
        if !valid_id(&request.session) {
            return Err(bad("that is not a session id"));
        }
        let _guard = self.lock.lock().unwrap();
        let machines = self.read_machines()?;
        let target = find(&machines, &request.machine)?;
        Self::check_willing(target, request.op)?;

        let (from, rev, title) = match request.op {
            Op::Push => (None, None, None),
            Op::Pull => {
                let who = request.from.as_deref().ok_or_else(|| bad("a pull names the machine whose copy to install (from)"))?;
                let from = find(&machines, who)?;
                let history = self
                    .history(agent.as_str(), &request.session)
                    .map_err(|e| if matches!(e, HubError::NotFound) { bad("that session is not on the hub") } else { e })?;
                let pushed_by = history.manifest.machine.as_ref().map(|m| m.id.as_str());
                if pushed_by != Some(from.id.as_str()) {
                    let actual = history.manifest.machine.as_ref().map_or("an unknown machine", |m| m.name.as_str());
                    return Err(bad(format!(
                        "the hub's current copy was pushed by {actual}, not {}; push it from {} first",
                        from.name, from.name
                    )));
                }
                if from.id == target.id {
                    return Err(bad("that machine pushed this copy itself"));
                }
                (Some(mref(from)), Some(history.head.clone()), history.manifest.title.clone())
            }
        };

        let mut commands = self.read_commands()?;
        Self::sweep(&mut commands);
        if let Some(busy) = commands
            .iter()
            .find(|c| !c.state.is_final() && c.agent == agent && c.session == request.session)
        {
            return Err(HubError::Busy(format!(
                "another command for this session is still {} on {}",
                if busy.state == State::Running { "running" } else { "waiting" },
                busy.machine.name
            )));
        }
        let waiting = commands.iter().filter(|c| !c.state.is_final() && c.machine.id == target.id).count();
        if waiting >= QUEUE_DEPTH {
            return Err(HubError::Busy(format!("{} already has {waiting} commands waiting", target.name)));
        }
        let t = now();
        let command = Command {
            id: random_hex(8)?,
            op: request.op,
            agent,
            session: request.session,
            title,
            machine: mref(target),
            from,
            rev,
            args: Args { exact: (request.op == Op::Push).then_some(request.exact.unwrap_or(false)) },
            state: State::Queued,
            code: None,
            detail: None,
            attempts: 0,
            created: t,
            updated: t,
            expires: after(TTL_SECS),
            claimed_at: None,
            lease_until: None,
            claimer: None,
            cancel_requested: false,
            created_by: by.to_string(),
        };
        commands.push(command.clone());
        self.write_commands(&commands)?;
        self.command_log("enqueue", &command);
        Ok(command)
    }

    /// A machine asks for its work, saying what it can do. Records that it
    /// asked, and refuses what it just said it will not do.
    pub fn inbox(&self, machine: &Machine, caps: Caps) -> Result<Vec<Work>, HubError> {
        self.polled.lock().unwrap().insert(machine.id.clone(), now());
        let _guard = self.lock.lock().unwrap();
        // Capabilities are stored only when they change: the poll is every
        // few seconds, the answer almost never.
        let mut machines = self.read_machines()?;
        if let Some(record) = machines.iter_mut().find(|m| m.id == machine.id)
            && record.caps.as_ref() != Some(&caps)
        {
            record.caps = Some(caps.clone());
            self.write_machines(&machines)?;
        }
        let mut commands = self.read_commands()?;
        let mut changed = Self::sweep(&mut commands);
        let t = now();
        for c in commands.iter_mut().filter(|c| c.machine.id == machine.id && c.state == State::Queued) {
            let refused = !caps.enabled || !caps.ops.iter().any(|o| o == c.op.as_str());
            if refused {
                c.state = State::Blocked;
                c.code = Some(if caps.enabled { Code::Unsupported } else { Code::RemoteOff });
                c.detail = Some(if caps.enabled {
                    "that machine does not run this kind of command".into()
                } else {
                    "remote control is off on that machine".into()
                });
                c.updated = t;
                changed = true;
                let snapshot = c.clone();
                self.command_log("refused", &snapshot);
            }
        }
        if changed {
            self.write_commands(&commands)?;
        }
        let mut work: Vec<&Command> = commands
            .iter()
            .filter(|c| c.machine.id == machine.id && c.state == State::Queued && !c.cancel_requested)
            .collect();
        work.sort_by_key(|c| c.created);
        Ok(work.into_iter().map(Work::from).collect())
    }

    /// A machine takes a command: it is theirs for ten minutes.
    pub fn claim(&self, machine: &Machine, id: &str) -> Result<Work, HubError> {
        let _guard = self.lock.lock().unwrap();
        let mut commands = self.read_commands()?;
        let swept = Self::sweep(&mut commands);
        let c = commands.iter_mut().find(|c| c.id == id && c.machine.id == machine.id).ok_or(HubError::NotFound)?;
        if c.state != State::Queued {
            if swept {
                self.write_commands(&commands)?;
            }
            return Err(HubError::Busy("that command is not waiting any more".into()));
        }
        c.state = State::Running;
        c.attempts += 1;
        c.claimed_at = Some(now());
        c.lease_until = Some(after(LEASE_SECS));
        c.claimer = Some(machine.id.clone());
        c.updated = now();
        let (work, snapshot) = (Work::from(&*c), c.clone());
        self.write_commands(&commands)?;
        self.command_log("claim", &snapshot);
        Ok(work)
    }

    /// A machine's result. Idempotent: the last claimer may repeat it, and a
    /// result arriving after the lease ran out (the command went back in the
    /// queue meanwhile) is still accepted — the work happened.
    pub fn report(&self, machine: &Machine, id: &str, report: Report) -> Result<Command, HubError> {
        let _guard = self.lock.lock().unwrap();
        let mut commands = self.read_commands()?;
        Self::sweep(&mut commands);
        let c = commands.iter_mut().find(|c| c.id == id).ok_or(HubError::NotFound)?;
        if c.claimer.as_deref() != Some(machine.id.as_str()) {
            return Err(HubError::NotFound);
        }
        if c.state.is_final() {
            // A repeat of a result already recorded.
            return Ok(c.clone());
        }
        // A push that says it succeeded must name a revision that is really
        // on the hub, for this session; one this machine made, or the head.
        let mut rev = None;
        if c.op == Op::Push && report.code.succeeded() {
            let named = report.rev.as_deref().filter(|r| valid_sha(r)).ok_or_else(|| bad("a successful push names the revision it produced"))?;
            let manifest = self
                .revision(c.agent.as_str(), &c.session, named)
                .map_err(|_| bad("that revision is not on the hub"))?;
            let head = self.history(c.agent.as_str(), &c.session)?.head;
            let theirs = manifest.machine.as_ref().is_some_and(|m| m.id == machine.id);
            if !theirs && head != named {
                return Err(bad("that revision was not pushed by this machine and is not the hub's head"));
            }
            rev = Some(named.to_string());
        }
        c.state = if report.code.succeeded() { State::Ok } else { State::Blocked };
        c.code = Some(report.code);
        c.detail = report.detail.as_deref().map(clip);
        if rev.is_some() {
            c.rev = rev;
        }
        c.lease_until = None;
        c.updated = now();
        let snapshot = c.clone();
        self.write_commands(&commands)?;
        self.command_log("result", &snapshot);
        Ok(snapshot)
    }

    /// Stop a command. One not yet claimed never runs; one that is running
    /// may still finish, and its real result is recorded when it does.
    pub fn cancel_command(&self, id: &str) -> Result<Command, HubError> {
        let _guard = self.lock.lock().unwrap();
        let mut commands = self.read_commands()?;
        Self::sweep(&mut commands);
        let c = commands.iter_mut().find(|c| c.id == id).ok_or(HubError::NotFound)?;
        match c.state {
            State::Queued => {
                c.state = State::Cancelled;
                c.code = Some(Code::Cancelled);
                c.detail = Some("cancelled before it ran".into());
            }
            State::Running => c.cancel_requested = true,
            _ => return Err(bad("that command has already finished")),
        }
        c.updated = now();
        let snapshot = c.clone();
        self.write_commands(&commands)?;
        self.command_log("cancel", &snapshot);
        Ok(snapshot)
    }

    /// Queue a finished-without-success command again, checking everything
    /// that was checked when it was made.
    pub fn retry_command(&self, id: &str) -> Result<Command, HubError> {
        let _guard = self.lock.lock().unwrap();
        let machines = self.read_machines()?;
        let mut commands = self.read_commands()?;
        Self::sweep(&mut commands);
        let at = commands.iter().position(|c| c.id == id).ok_or(HubError::NotFound)?;
        let c = commands[at].clone();
        if !matches!(c.state, State::Blocked | State::Expired | State::Cancelled) {
            return Err(bad("only a command that was blocked, expired or cancelled can be retried"));
        }
        let target = find(&machines, &c.machine.id)?;
        Self::check_willing(target, c.op)?;
        if commands.iter().any(|o| o.id != c.id && !o.state.is_final() && o.agent == c.agent && o.session == c.session) {
            return Err(HubError::Busy("another command for this session is still in flight".into()));
        }
        let waiting = commands.iter().filter(|o| !o.state.is_final() && o.machine.id == target.id).count();
        if waiting >= QUEUE_DEPTH {
            return Err(HubError::Busy(format!("{} already has {waiting} commands waiting", target.name)));
        }
        let e = &mut commands[at];
        e.state = State::Queued;
        // A person asking again gets a fresh set of attempts.
        e.attempts = 0;
        e.code = None;
        e.detail = None;
        e.cancel_requested = false;
        e.claimed_at = None;
        e.lease_until = None;
        e.claimer = None;
        e.updated = now();
        e.expires = after(TTL_SECS);
        let snapshot = e.clone();
        self.write_commands(&commands)?;
        self.command_log("retry", &snapshot);
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::manifest::Manifest;

    fn caps(enabled: bool) -> Caps {
        Caps { v: PROTOCOL, ops: vec!["push".into(), "pull".into()], enabled }
    }

    /// A hub with two machines that have both said remote control is on.
    fn hub_with_two() -> (tempfile::TempDir, Hub, Machine, Machine) {
        let dir = tempfile::tempdir().unwrap();
        let hub = Hub::open(dir.path()).unwrap();
        let token = hub.join_token().unwrap();
        let a = hub.join(&token, "alpha").unwrap().machine;
        let b = hub.join(&token, "beta").unwrap().machine;
        hub.inbox(&a, caps(true)).unwrap();
        hub.inbox(&b, caps(true)).unwrap();
        (dir, hub, a, b)
    }

    fn push_request(machine: &str) -> NewCommand {
        NewCommand { op: Op::Push, machine: machine.into(), agent: "claude-code".into(), session: "s1".into(), from: None, exact: None }
    }

    #[test]
    fn a_command_goes_queued_running_ok_and_a_repeat_changes_nothing() {
        let (_d, hub, a, _b) = hub_with_two();
        let made = hub.enqueue(push_request("alpha"), "admin").unwrap();
        assert_eq!(made.state, State::Queued);
        assert_eq!(made.args.exact, Some(false));

        let work = hub.inbox(&a, caps(true)).unwrap();
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].id, made.id);

        hub.claim(&a, &made.id).unwrap();
        assert!(hub.inbox(&a, caps(true)).unwrap().is_empty(), "claimed work is not offered again");
        assert!(matches!(hub.claim(&a, &made.id), Err(HubError::Busy(_))), "a second claim is refused");

        let done = Report { code: Code::Failed, detail: Some("x".repeat(5000)), rev: None };
        let first = hub.report(&a, &made.id, done.clone()).unwrap();
        assert_eq!(first.state, State::Blocked);
        assert_eq!(first.detail.as_ref().unwrap().chars().count(), DETAIL_MAX, "detail is capped");
        let again = hub.report(&a, &made.id, Report { code: Code::Ok, detail: None, rev: None }).unwrap();
        assert_eq!(again.state, State::Blocked, "a recorded result is not overwritten");
    }

    #[test]
    fn only_the_target_machine_sees_and_reports_a_command() {
        let (_d, hub, a, b) = hub_with_two();
        let made = hub.enqueue(push_request("alpha"), "admin").unwrap();
        assert!(hub.inbox(&b, caps(true)).unwrap().is_empty());
        assert!(matches!(hub.claim(&b, &made.id), Err(HubError::NotFound)));
        hub.claim(&a, &made.id).unwrap();
        let r = Report { code: Code::Failed, detail: None, rev: None };
        assert!(matches!(hub.report(&b, &made.id, r), Err(HubError::NotFound)));
    }

    #[test]
    fn a_machine_that_is_off_or_never_reported_cannot_be_asked() {
        let (_d, hub, a, _b) = hub_with_two();
        hub.inbox(&a, caps(false)).unwrap();
        let err = hub.enqueue(push_request("alpha"), "admin").unwrap_err();
        assert!(err.to_string().contains("not turned remote control on"), "{err}");

        let token = hub.join_token().unwrap();
        hub.join(&token, "gamma").unwrap();
        let err = hub.enqueue(push_request("gamma"), "admin").unwrap_err();
        assert!(err.to_string().contains("has not reported"), "{err}");
        assert!(hub.enqueue(push_request("nobody"), "admin").is_err());
    }

    #[test]
    fn turning_it_off_blocks_what_was_waiting() {
        let (_d, hub, a, _b) = hub_with_two();
        let made = hub.enqueue(push_request("alpha"), "admin").unwrap();
        assert!(hub.inbox(&a, caps(false)).unwrap().is_empty());
        let c = hub.command(&made.id).unwrap();
        assert_eq!((c.state, c.code), (State::Blocked, Some(Code::RemoteOff)));
    }

    #[test]
    fn one_command_per_session_and_a_bounded_queue() {
        let (_d, hub, _a, _b) = hub_with_two();
        hub.enqueue(push_request("alpha"), "admin").unwrap();
        let err = hub.enqueue(push_request("beta"), "admin").unwrap_err();
        assert!(matches!(err, HubError::Busy(_)), "{err}");
        for n in 0..QUEUE_DEPTH - 1 {
            let mut r = push_request("alpha");
            r.session = format!("q{n}");
            hub.enqueue(r, "admin").unwrap();
        }
        let mut r = push_request("alpha");
        r.session = "one-too-many".into();
        assert!(matches!(hub.enqueue(r, "admin"), Err(HubError::Busy(_))));
    }

    #[test]
    fn a_lost_lease_gives_the_command_back_and_a_late_result_still_counts() {
        let (_d, hub, a, _b) = hub_with_two();
        let made = hub.enqueue(push_request("alpha"), "admin").unwrap();
        hub.claim(&a, &made.id).unwrap();
        // Age the lease past its end.
        let mut all = hub.read_commands().unwrap();
        all[0].lease_until = Some(after(-5));
        hub.write_commands(&all).unwrap();
        assert_eq!(hub.inbox(&a, caps(true)).unwrap().len(), 1, "offered again");
        assert_eq!(hub.command(&made.id).unwrap().state, State::Queued);
        let r = Report { code: Code::Failed, detail: Some("late".into()), rev: None };
        assert_eq!(hub.report(&a, &made.id, r).unwrap().state, State::Blocked);
        // After three claims that never reported, it is blocked, not retried forever.
        let again = hub.retry_command(&made.id).unwrap();
        assert_eq!(again.state, State::Queued);
        for _ in 0..MAX_ATTEMPTS {
            hub.claim(&a, &made.id).unwrap();
            let mut all = hub.read_commands().unwrap();
            all[0].lease_until = Some(after(-5));
            hub.write_commands(&all).unwrap();
            hub.commands(10).unwrap();
        }
        let c = hub.command(&made.id).unwrap();
        assert_eq!((c.state, c.code), (State::Blocked, Some(Code::Failed)));
    }

    #[test]
    fn an_unclaimed_command_expires_and_can_be_retried() {
        let (_d, hub, _a, _b) = hub_with_two();
        let made = hub.enqueue(push_request("alpha"), "admin").unwrap();
        let mut all = hub.read_commands().unwrap();
        all[0].expires = after(-1);
        hub.write_commands(&all).unwrap();
        let c = hub.command(&made.id).unwrap();
        assert_eq!((c.state, c.code), (State::Expired, Some(Code::Expired)));
        assert_eq!(hub.retry_command(&made.id).unwrap().state, State::Queued);
        assert!(hub.retry_command(&made.id).is_err(), "only a finished command is retried");
    }

    #[test]
    fn cancel_stops_a_waiting_command_and_only_asks_a_running_one() {
        let (_d, hub, a, _b) = hub_with_two();
        let first = hub.enqueue(push_request("alpha"), "admin").unwrap();
        assert_eq!(hub.cancel_command(&first.id).unwrap().state, State::Cancelled);
        assert!(hub.cancel_command(&first.id).is_err());
        let second = hub.enqueue(push_request("alpha"), "admin").unwrap();
        hub.claim(&a, &second.id).unwrap();
        let c = hub.cancel_command(&second.id).unwrap();
        assert_eq!((c.state, c.cancel_requested), (State::Running, true));
    }

    #[test]
    fn the_commands_token_opens_commands_and_the_admin_token_does_too() {
        let (_d, hub, a, _b) = hub_with_two();
        assert!(hub.check_commands("asmk_nope").is_err());
        let token = hub.rotate_commands_token().unwrap();
        assert!(hub.check_commands(&token).is_ok());
        assert!(hub.check_admin(&token).is_err(), "it must not open the admin API");
        let admin = hub.rotate_admin_token().unwrap();
        assert!(hub.check_commands(&admin).is_ok());
        let _ = a;
    }

    fn put_copy(hub: &Hub, by: &Machine, parent: Option<String>) -> String {
        let blob = b"hello";
        let sha = crate::fsutil::sha256_hex(blob);
        hub.put_blob(&sha, &blob[..], 100).unwrap();
        let m = Manifest {
            schema: crate::hub::manifest::SCHEMA,
            agent: AgentKind::ClaudeCode,
            id: "s1".into(),
            title: Some("A title".into()),
            slug: None,
            project_root: "/home/a/x".into(),
            project_root_portable: "${HOME}/x".into(),
            git_origin: None,
            git_branch: None,
            agent_version: None,
            created: None,
            updated: None,
            machine: None,
            pushed_at: None,
            canonical: "c".into(),
            parent_rev: parent,
            files: vec![crate::hub::manifest::FileEntry {
                path: "transcript.jsonl".into(),
                sha256: Some(sha),
                size: 5,
                symlink: None,
            }],
            extra: serde_json::Value::Null,
        };
        hub.put_revision("claude-code", "s1", &serde_json::to_vec(&m).unwrap(), by).unwrap()
    }

    #[test]
    fn a_pull_is_pinned_to_the_copy_one_named_machine_pushed() {
        let (_d, hub, a, b) = hub_with_two();
        let head = put_copy(&hub, &a, None);

        let mut pull = NewCommand {
            op: Op::Pull,
            machine: "beta".into(),
            agent: "claude-code".into(),
            session: "s1".into(),
            from: Some("alpha".into()),
            exact: None,
        };
        let made = hub.enqueue(pull.clone(), "admin").unwrap();
        assert_eq!(made.rev.as_deref(), Some(head.as_str()), "pinned at creation");
        assert_eq!(made.from.as_ref().map(|m| m.name.as_str()), Some("alpha"));
        assert_eq!(made.title.as_deref(), Some("A title"));
        hub.cancel_command(&made.id).unwrap();

        pull.from = Some("beta".into());
        let err = hub.enqueue(pull.clone(), "admin").unwrap_err().to_string();
        assert!(err.contains("pushed by alpha, not beta"), "{err}");
        pull.from = None;
        assert!(hub.enqueue(pull.clone(), "admin").is_err(), "a pull must name its source");

        // After beta pushes on top, alpha's copy is no longer the head.
        put_copy(&hub, &b, Some(head));
        pull.from = Some("alpha".into());
        pull.machine = "alpha".into();
        assert!(hub.enqueue(pull, "admin").is_err());
    }

    #[test]
    fn a_pushed_revision_must_be_real_and_this_machines() {
        let (_d, hub, a, b) = hub_with_two();
        let rev = put_copy(&hub, &a, None);
        let made = hub.enqueue(push_request("alpha"), "admin").unwrap();
        hub.claim(&a, &made.id).unwrap();
        let said = |rev: Option<&str>| Report { code: Code::Ok, detail: None, rev: rev.map(String::from) };
        assert!(hub.report(&a, &made.id, said(None)).is_err(), "a push names its revision");
        assert!(hub.report(&a, &made.id, said(Some(&"0".repeat(64)))).is_err(), "and it must exist");
        let done = hub.report(&a, &made.id, said(Some(&rev))).unwrap();
        assert_eq!((done.state, done.rev.as_deref()), (State::Ok, Some(rev.as_str())));

        // A revision another machine made is not this machine's to claim
        // unless it is the head (the content was already there).
        let second = put_copy(&hub, &b, Some(rev.clone()));
        let head = put_copy(&hub, &b, Some(second.clone()));
        let next = {
            let mut r = push_request("alpha");
            r.session = "s1".into();
            hub.enqueue(r, "admin").unwrap()
        };
        hub.claim(&a, &next.id).unwrap();
        assert!(hub.report(&a, &next.id, said(Some(&second))).is_err(), "someone else's old revision");
        assert!(hub.report(&a, &next.id, said(Some(&head))).is_ok(), "the head, already in sync");
    }
}
