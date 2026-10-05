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
//!
//! A *plan* is a short ordered chain of commands (`send`: push on one machine,
//! then pull on another). A step with a `needs` parent is `pending` — never
//! offered to a machine — until that parent succeeds; the hub then copies the
//! parent's (verified) revision into it and queues it. A step that ends
//! without success cancels the steps waiting on it (`skipped`), and a retry
//! brings them back. All the steps of one plan count as one command for the
//! one-command-per-session rule.

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
/// A title copied from a manifest (unbounded there) into the list every poll reads.
const TITLE_MAX: usize = 200;
/// Stored commands, finished ones included: past this, nothing new is made
/// until pruning (or cancelling) makes room, so `commands.json` stays small.
const MAX_STORED: usize = 2000;

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
    /// Waiting for the step it `needs`; never offered to a machine.
    Pending,
    Queued,
    Running,
    Ok,
    Blocked,
    Cancelled,
    Expired,
}

impl State {
    pub fn is_final(self) -> bool {
        !matches!(self, State::Pending | State::Queued | State::Running)
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
    /// The plan this is a step of (its steps share the id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// 1-based position in its plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    /// The step that must succeed before this one is queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<String>,
    /// Cancelled only because an earlier step did not succeed.
    #[serde(default, skip_serializing_if = "no")]
    pub skipped: bool,
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

fn no(b: &bool) -> bool {
    !*b
}

impl Command {
    /// A queued command with nothing else set.
    fn new(op: Op, agent: AgentKind, session: String, machine: MachineRef, by: &str) -> Result<Command, CoreError> {
        let t = now();
        Ok(Command {
            id: random_hex(8)?,
            op,
            agent,
            session,
            title: None,
            machine,
            from: None,
            rev: None,
            args: Args::default(),
            plan: None,
            step: None,
            needs: None,
            skipped: false,
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
        })
    }
}

/// A request to send a session from one machine to another: push on `from`,
/// then pull on `to`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewPlan {
    /// Only `send` exists so far.
    pub kind: String,
    pub agent: String,
    pub session: String,
    /// Machine id or name.
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact: Option<bool>,
}

/// A chain of commands, steps in order. Its state is derived from them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub kind: String,
    pub state: State,
    pub created: Timestamp,
    pub updated: Timestamp,
    pub steps: Vec<Command>,
}

impl Plan {
    pub fn new(id: &str, mut steps: Vec<Command>) -> Plan {
        steps.sort_by_key(|c| (c.step.unwrap_or(0), c.created));
        let kind = match steps.iter().map(|c| c.op).collect::<Vec<_>>().as_slice() {
            [Op::Push, Op::Pull] => "send",
            _ => "plan",
        };
        Plan {
            id: id.to_string(),
            kind: kind.to_string(),
            state: Self::derive(&steps),
            created: steps.iter().map(|c| c.created).min().unwrap_or_else(now),
            updated: steps.iter().map(|c| c.updated).max().unwrap_or_else(now),
            steps,
        }
    }

    /// Still going while any step is; `ok` only when every step is. A step
    /// skipped because an earlier one failed does not say why the plan ended:
    /// that one does.
    fn derive(steps: &[Command]) -> State {
        if steps.iter().all(|c| c.state == State::Ok) {
            return State::Ok;
        }
        for s in [State::Running, State::Queued, State::Pending] {
            if steps.iter().any(|c| c.state == s) {
                return s;
            }
        }
        for s in [State::Blocked, State::Cancelled, State::Expired] {
            if steps.iter().any(|c| c.state == s && !c.skipped) {
                return s;
            }
        }
        State::Cancelled
    }
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

/// Cut to a limit on a character boundary, every control character (newline
/// included) turned into a space: this is text a machine or a manifest wrote,
/// printed by the CLI and shown in the UI, so it must not carry escapes.
fn clean(text: &str, max: usize) -> String {
    text.chars().take(max).map(|c| if c.is_control() { ' ' } else { c }).collect()
}

fn clip(text: &str) -> String {
    clean(text, DETAIL_MAX)
}

/// What `sweep` and `propagate` moved, for the log: the event and the
/// command as it is now.
type Events = Vec<(&'static str, Command)>;

fn find<'a>(machines: &'a [MachineRecord], who: &str) -> Result<&'a MachineRecord, HubError> {
    // An id is unique; a name may not be, so it only counts when no id matches.
    if let Some(m) = machines.iter().find(|m| m.id == who) {
        return Ok(m);
    }
    let hits: Vec<&MachineRecord> = machines.iter().filter(|m| m.name == who).collect();
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
    /// text and has no business crowding the administrator's own log). `by`
    /// is who caused it: `admin`, `commands-token`, `machine:<id>` or `hub`
    /// (a timeout, or a step following the one it waited for).
    fn command_log(&self, event: &str, c: &Command, by: &str) {
        let line = serde_json::json!({
            "at": now(), "event": event, "by": by, "id": c.id, "op": c.op,
            "machine": c.machine.id, "agent": c.agent, "session": c.session, "code": c.code,
            "plan": c.plan, "step": c.step, "rev": c.rev,
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
    /// about to be read or changed. Says whether anything moved, and what
    /// (for the caller to log once the list is written).
    fn sweep(commands: &mut Vec<Command>) -> (bool, Events) {
        let t = now();
        let mut events = Events::new();
        for c in commands.iter_mut() {
            let lease_lost = c.state == State::Running && c.lease_until.is_some_and(|l| l < t);
            if c.cancel_requested && (lease_lost || c.state == State::Queued) {
                // Asked to stop and not running any more: it would never be
                // offered again (the inbox skips it), so it ends here.
                c.state = State::Cancelled;
                c.code = Some(Code::Cancelled);
                c.detail = Some("cancelled".into());
                c.lease_until = None;
                c.updated = t;
                events.push(("cancel", c.clone()));
            } else if c.state == State::Queued && c.expires < t {
                c.state = State::Expired;
                c.code = Some(Code::Expired);
                c.detail = Some("the machine did not pick this up in time".into());
                c.updated = t;
                events.push(("expire", c.clone()));
            } else if lease_lost {
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
                events.push(("lease", c.clone()));
            }
        }
        // A step waiting for one that just ended is skipped. (A pending step
        // never expires by itself: its parent's end, or its own plan's
        // cancel, resolves it.)
        events.extend(Self::propagate(commands));
        let before = commands.len();
        let cutoff = t.as_second() - PRUNE_SECS;
        let old = |c: &Command| c.state.is_final() && c.updated.as_second() < cutoff;
        // A plan is kept or dropped whole.
        let keep: std::collections::HashSet<String> =
            commands.iter().filter(|c| !old(c)).filter_map(|c| c.plan.clone()).collect();
        commands.retain(|c| !old(c) || c.plan.as_ref().is_some_and(|p| keep.contains(p)));
        (!events.is_empty() || commands.len() != before, events)
    }

    /// Move pending steps along according to the step each needs: queued
    /// (with the parent's revision) once it succeeded, cancelled as skipped
    /// once it ended without success or was asked to stop. Repeats, so a
    /// longer chain follows. Returns what moved, for the log.
    fn propagate(commands: &mut [Command]) -> Events {
        let mut moved = Vec::new();
        loop {
            let mut again = false;
            for i in 0..commands.len() {
                if commands[i].state != State::Pending {
                    continue;
                }
                let parent = commands
                    .iter()
                    .find(|p| Some(&p.id) == commands[i].needs.as_ref())
                    .map(|p| (p.state, p.cancel_requested, p.rev.clone(), p.step));
                let t = now();
                let c = &mut commands[i];
                match parent {
                    // A pull is never queued without the revision it installs.
                    Some((State::Ok, _, rev, _)) if !(c.op == Op::Pull && rev.is_none()) => {
                        c.state = State::Queued;
                        c.rev = rev;
                        // A week from when it can run, not from when it was planned.
                        c.expires = after(TTL_SECS);
                        moved.push(("advance", c.clone()));
                    }
                    Some((State::Queued | State::Pending, ..) | (State::Running, false, ..)) => continue,
                    other => {
                        c.state = State::Cancelled;
                        c.skipped = true;
                        c.code = Some(Code::Cancelled);
                        c.detail = Some(match other {
                            Some((State::Ok, _, _, Some(n))) => format!("step {n} produced no revision to install"),
                            Some((State::Running, true, _, Some(n))) => format!("cancelled with the plan before step {n} finished"),
                            Some((_, _, _, Some(n))) => format!("step {n} did not succeed"),
                            _ => "an earlier step did not succeed".into(),
                        });
                        moved.push(("skip", c.clone()));
                    }
                }
                c.updated = t;
                again = true;
            }
            if !again {
                return moved;
            }
        }
    }

    /// Log what the hub did by itself, after the list was written.
    fn log_events(&self, events: Events) {
        for (event, c) in events {
            self.command_log(event, &c, "hub");
        }
    }

    /// The step that holds `session` for someone else, if any. Steps of
    /// `plan` do not count: a plan's own steps are one command.
    fn ensure_free(commands: &[Command], agent: AgentKind, session: &str, plan: Option<&str>) -> Result<(), HubError> {
        match commands.iter().find(|c| {
            !c.state.is_final() && c.agent == agent && c.session == session && !(plan.is_some() && c.plan.as_deref() == plan)
        }) {
            Some(busy) => Err(HubError::Busy(format!(
                "another command for this session is still {} on {}",
                if busy.state == State::Running { "running" } else { "waiting" },
                busy.machine.name
            ))),
            None => Ok(()),
        }
    }

    /// Room for `extra` more waiting commands (pending ones count: they will
    /// be queued there) on a machine.
    fn ensure_room(commands: &[Command], machine: &MachineRef, extra: usize) -> Result<(), HubError> {
        let waiting = commands.iter().filter(|c| !c.state.is_final() && c.machine.id == machine.id).count();
        if waiting + extra > QUEUE_DEPTH {
            return Err(HubError::Busy(format!("{} already has {waiting} commands waiting", machine.name)));
        }
        Ok(())
    }

    /// Room for one more. At the cap the oldest finished commands go first
    /// (a plan only once it has ended as a whole), so a runaway script cannot
    /// lock remote control out for the two weeks pruning would take; only
    /// 2000 commands all still open refuse.
    fn make_room(commands: &mut Vec<Command>) -> Result<(), HubError> {
        if commands.len() < MAX_STORED {
            return Ok(());
        }
        let open_plans: std::collections::HashSet<String> =
            commands.iter().filter(|c| !c.state.is_final()).filter_map(|c| c.plan.clone()).collect();
        let mut evictable: Vec<(Timestamp, String)> = commands
            .iter()
            .filter(|c| c.state.is_final() && !c.plan.as_ref().is_some_and(|p| open_plans.contains(p)))
            .map(|c| (c.updated, c.id.clone()))
            .collect();
        if evictable.is_empty() {
            return Err(HubError::Busy(format!("{MAX_STORED} commands are open at once; cancel some or wait for them to finish")));
        }
        evictable.sort();
        // A tenth at a time, so the next creations do not each evict one.
        let drop: std::collections::HashSet<String> =
            evictable.into_iter().take(MAX_STORED / 10).map(|(_, id)| id).collect();
        commands.retain(|c| !drop.contains(&c.id));
        Ok(())
    }

    /// The commands, newest first. A plan that is listed at all is listed
    /// whole, so the limit may be passed by a step or two.
    pub fn commands(&self, limit: usize) -> Result<Vec<Command>, CoreError> {
        let _guard = self.lock.lock().unwrap();
        let mut all = self.read_commands()?;
        let (changed, events) = Self::sweep(&mut all);
        if changed {
            self.write_commands(&all)?;
        }
        self.log_events(events);
        all.sort_by_key(|c| std::cmp::Reverse(c.created));
        let rest = all.split_off(limit.min(all.len()));
        let shown: std::collections::HashSet<String> = all.iter().filter_map(|c| c.plan.clone()).collect();
        all.extend(rest.into_iter().filter(|c| c.plan.as_ref().is_some_and(|p| shown.contains(p))));
        all.sort_by_key(|c| std::cmp::Reverse(c.created));
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
        let (_, events) = Self::sweep(&mut commands);
        Self::make_room(&mut commands)?;
        Self::ensure_free(&commands, agent, &request.session, None)?;
        Self::ensure_room(&commands, &mref(target), 1)?;
        let mut command = Command::new(request.op, agent, request.session, mref(target), by)?;
        command.title = title.map(|t| clean(&t, TITLE_MAX));
        command.from = from;
        command.rev = rev;
        command.args.exact = (request.op == Op::Push).then_some(request.exact.unwrap_or(false));
        commands.push(command.clone());
        self.write_commands(&commands)?;
        self.log_events(events);
        self.command_log("enqueue", &command, by);
        Ok(command)
    }

    /// Ask for a session to be sent from one machine to another: a push on
    /// `from`, and — once that succeeded — a pull on `to` of exactly the
    /// revision it produced.
    pub fn create_plan(&self, request: NewPlan, by: &str) -> Result<Plan, HubError> {
        if request.kind != "send" {
            return Err(bad(format!("{:?} is not a kind of plan (send)", request.kind)));
        }
        let agent = AgentKind::parse(&request.agent).ok_or_else(|| bad(format!("unknown agent {:?}", request.agent)))?;
        if !valid_id(&request.session) {
            return Err(bad("that is not a session id"));
        }
        let _guard = self.lock.lock().unwrap();
        let machines = self.read_machines()?;
        let (source, target) = (find(&machines, &request.from)?, find(&machines, &request.to)?);
        if source.id == target.id {
            return Err(bad(format!("{} is both ends of the send; name two machines", source.name)));
        }
        Self::check_willing(source, Op::Push)?;
        Self::check_willing(target, Op::Pull)?;

        let mut commands = self.read_commands()?;
        let (_, events) = Self::sweep(&mut commands);
        Self::make_room(&mut commands)?;
        Self::ensure_free(&commands, agent, &request.session, None)?;
        Self::ensure_room(&commands, &mref(source), 1)?;
        Self::ensure_room(&commands, &mref(target), 1)?;

        let title = self.history(agent.as_str(), &request.session).ok().and_then(|h| h.manifest.title.clone()).map(|t| clean(&t, TITLE_MAX));
        let plan = random_hex(8)?;
        let mut push = Command::new(Op::Push, agent, request.session.clone(), mref(source), by)?;
        push.args.exact = Some(request.exact.unwrap_or(false));
        let mut pull = Command::new(Op::Pull, agent, request.session, mref(target), by)?;
        pull.from = Some(mref(source));
        pull.state = State::Pending;
        pull.needs = Some(push.id.clone());
        let steps: Vec<Command> = [push, pull]
            .into_iter()
            .enumerate()
            .map(|(n, mut c)| {
                c.plan = Some(plan.clone());
                c.step = Some(n as u32 + 1);
                c.title = title.clone();
                c
            })
            .collect();
        commands.extend(steps.iter().cloned());
        self.write_commands(&commands)?;
        self.log_events(events);
        for c in &steps {
            self.command_log("enqueue", c, by);
        }
        Ok(Plan::new(&plan, steps))
    }

    /// The plans, newest first, each with its steps.
    pub fn plans(&self, limit: usize) -> Result<Vec<Plan>, HubError> {
        let mut by_plan: std::collections::HashMap<String, Vec<Command>> = std::collections::HashMap::new();
        for c in self.commands(usize::MAX)? {
            if let Some(id) = c.plan.clone() {
                by_plan.entry(id).or_default().push(c);
            }
        }
        let mut plans: Vec<Plan> = by_plan.into_iter().map(|(id, steps)| Plan::new(&id, steps)).collect();
        plans.sort_by_key(|p| std::cmp::Reverse(p.created));
        plans.truncate(limit);
        Ok(plans)
    }

    pub fn plan(&self, id: &str) -> Result<Plan, HubError> {
        let steps: Vec<Command> = self.commands(usize::MAX)?.into_iter().filter(|c| c.plan.as_deref() == Some(id)).collect();
        if steps.is_empty() {
            return Err(HubError::NotFound);
        }
        Ok(Plan::new(id, steps))
    }

    /// Whether anything is still going on for a session (a plan's steps
    /// included): a revision such a command is about to install must not be
    /// deleted from under it. Reads without taking the lock, for a caller
    /// that holds it.
    pub(super) fn session_in_flight(&self, agent: AgentKind, session: &str) -> Result<bool, CoreError> {
        let mut commands = self.read_commands()?;
        let _ = Self::sweep(&mut commands);
        Ok(commands.iter().any(|c| !c.state.is_final() && c.agent == agent && c.session == session))
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
        let (mut changed, mut events) = Self::sweep(&mut commands);
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
                events.push(("refused", c.clone()));
            }
        }
        // A step refused above takes the steps waiting on it with it.
        let moved = Self::propagate(&mut commands);
        changed |= !moved.is_empty();
        events.extend(moved);
        if changed {
            self.write_commands(&commands)?;
        }
        self.log_events(events);
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
        let (swept, events) = Self::sweep(&mut commands);
        let c = commands.iter_mut().find(|c| c.id == id && c.machine.id == machine.id).ok_or(HubError::NotFound)?;
        if c.state != State::Queued || c.cancel_requested {
            if swept {
                self.write_commands(&commands)?;
                self.log_events(events);
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
        self.log_events(events);
        self.command_log("claim", &snapshot, &format!("machine:{}", machine.id));
        Ok(work)
    }

    /// A machine's result. Idempotent: the last claimer may repeat it, and a
    /// result arriving after the lease ran out (the command went back in the
    /// queue meanwhile) is still accepted — the work happened.
    pub fn report(&self, machine: &Machine, id: &str, report: Report) -> Result<Command, HubError> {
        let _guard = self.lock.lock().unwrap();
        let mut commands = self.read_commands()?;
        let (_, events) = Self::sweep(&mut commands);
        let c = commands.iter_mut().find(|c| c.id == id).ok_or(HubError::NotFound)?;
        if c.claimer.as_deref() != Some(machine.id.as_str()) {
            return Err(HubError::NotFound);
        }
        if c.state.is_final() {
            // A repeat of a result already recorded — or a real result that
            // arrived after the command was cancelled or expired. The record
            // stands, and the log says what the machine reported.
            let snapshot = c.clone();
            if snapshot.code != Some(report.code) {
                let mut late = snapshot.clone();
                late.code = Some(report.code);
                self.command_log("late-result-ignored", &late, &format!("machine:{}", machine.id));
            }
            return Ok(snapshot);
        }
        // A push that says it succeeded must name a revision that is really
        // on the hub, for this session: the head, or one this machine made
        // since the command was asked for (an older one of its own, from an
        // earlier push, is not what this command did). "Already in sync"
        // can only mean the head.
        let mut rev = None;
        if c.op == Op::Push && report.code.succeeded() {
            let named = report.rev.as_deref().filter(|r| valid_sha(r)).ok_or_else(|| bad("a successful push names the revision it produced"))?;
            let manifest = self
                .revision(c.agent.as_str(), &c.session, named)
                .map_err(|_| bad("that revision is not on the hub"))?;
            let head = self.history(c.agent.as_str(), &c.session)?.head;
            let fresh = manifest.machine.as_ref().is_some_and(|m| m.id == machine.id)
                && manifest.pushed_at.is_some_and(|p| p >= c.created);
            let allowed = head == named || (report.code == Code::Ok && fresh);
            if !allowed {
                return Err(bad("that revision is not the hub's head, and was not pushed by this machine for this command"));
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
        // A step waiting on this one is queued with the revision just
        // verified, or cancelled.
        let moved = Self::propagate(&mut commands);
        self.write_commands(&commands)?;
        self.log_events(events);
        self.command_log("result", &snapshot, &format!("machine:{}", machine.id));
        self.log_events(moved);
        Ok(snapshot)
    }

    /// Stop one step: one not yet claimed never runs; one that is running
    /// may still finish, and its real result is recorded when it does.
    fn cancel_at(c: &mut Command) -> Result<(), HubError> {
        match c.state {
            State::Queued | State::Pending => {
                c.state = State::Cancelled;
                c.code = Some(Code::Cancelled);
                c.detail = Some("cancelled before it ran".into());
            }
            State::Running => c.cancel_requested = true,
            _ => return Err(bad("that command has already finished")),
        }
        c.updated = now();
        Ok(())
    }

    /// Stop a command. The steps of a plan waiting on it are cancelled too.
    pub fn cancel_command(&self, id: &str, by: &str) -> Result<Command, HubError> {
        let _guard = self.lock.lock().unwrap();
        let mut commands = self.read_commands()?;
        let (_, events) = Self::sweep(&mut commands);
        let c = commands.iter_mut().find(|c| c.id == id).ok_or(HubError::NotFound)?;
        Self::cancel_at(c)?;
        let snapshot = c.clone();
        let moved = Self::propagate(&mut commands);
        self.write_commands(&commands)?;
        self.log_events(events);
        self.command_log("cancel", &snapshot, by);
        self.log_events(moved);
        Ok(snapshot)
    }

    /// Stop every step of a plan that has not finished.
    pub fn cancel_plan(&self, id: &str, by: &str) -> Result<Plan, HubError> {
        let _guard = self.lock.lock().unwrap();
        let mut commands = self.read_commands()?;
        let (_, mut events) = Self::sweep(&mut commands);
        let mut cancelled = Vec::new();
        let mut steps: Vec<&Command> = commands.iter().filter(|c| c.plan.as_deref() == Some(id)).collect();
        if steps.is_empty() {
            return Err(HubError::NotFound);
        }
        if steps.iter().all(|c| c.state.is_final()) {
            return Err(bad("that plan has already finished"));
        }
        steps.sort_by_key(|c| c.step);
        let ids: Vec<String> = steps.iter().map(|c| c.id.clone()).collect();
        for step in ids {
            // Cancelling one step may already have skipped the ones after it.
            let Some(c) = commands.iter_mut().find(|c| c.id == step && !c.state.is_final()) else { continue };
            Self::cancel_at(c)?;
            cancelled.push(c.clone());
            events.extend(Self::propagate(&mut commands));
        }
        self.write_commands(&commands)?;
        self.log_events(events);
        for c in &cancelled {
            self.command_log("cancel", c, by);
        }
        Ok(Plan::new(id, commands.into_iter().filter(|c| c.plan.as_deref() == Some(id)).collect()))
    }

    /// Queue a finished-without-success step again, checking everything that
    /// was checked when it was made, and bring back the steps that were
    /// skipped because of it. All or nothing. Returns what changed, the
    /// retried step first.
    fn retry_at(&self, commands: &mut Vec<Command>, machines: &[MachineRecord], id: &str) -> Result<Vec<Command>, HubError> {
        let c = commands.iter().find(|c| c.id == id).ok_or(HubError::NotFound)?.clone();
        if !matches!(c.state, State::Blocked | State::Expired | State::Cancelled) {
            return Err(bad("only a command that was blocked, expired or cancelled can be retried"));
        }
        // Where it goes: straight to the queue, or back to waiting for its parent.
        let (state, rev) = match c.needs.as_deref().map(|n| commands.iter().find(|p| p.id == n)) {
            None => (State::Queued, c.rev.clone()),
            Some(Some(p)) if p.state == State::Ok => (State::Queued, p.rev.clone()),
            Some(Some(p)) if p.state == State::Running && p.cancel_requested => {
                return Err(bad(format!("step {} is being cancelled; wait for it to finish", p.step.unwrap_or(1))));
            }
            Some(Some(p)) if !p.state.is_final() => (State::Pending, None),
            Some(Some(p)) => {
                return Err(bad(format!("step {} did not succeed; retry that step first", p.step.unwrap_or(1))));
            }
            Some(None) => return Err(bad("the step before it is gone; start a new plan")),
        };
        if c.op == Op::Pull && state == State::Queued {
            // Never queued without a revision, and not one that was pruned meanwhile.
            let gone = || bad("that revision is gone; start a new plan");
            self.revision(c.agent.as_str(), &c.session, rev.as_deref().ok_or_else(gone)?).map_err(|_| gone())?;
        }
        Self::check_willing(find(machines, &c.machine.id)?, c.op)?;
        Self::ensure_free(commands, c.agent, &c.session, c.plan.as_deref())?;

        let mut next = commands.clone();
        let fresh = |e: &mut Command, state: State, rev: Option<String>| {
            e.state = state;
            e.rev = rev;
            e.skipped = false;
            // A person asking again gets a fresh set of attempts.
            e.attempts = 0;
            e.code = None;
            e.detail = None;
            e.cancel_requested = false;
            e.claimed_at = None;
            e.lease_until = None;
            e.claimer = None;
            e.updated = now();
            // A new ask: a revision this machine pushed before it is not this
            // push's result (report() compares pushed_at with this).
            e.created = now();
            e.expires = after(TTL_SECS);
        };
        let at = next.iter().position(|e| e.id == id).ok_or(HubError::NotFound)?;
        fresh(&mut next[at], state, rev);
        let mut changed = vec![at];
        // The steps cancelled for want of this one wait for it again.
        loop {
            let waiting = next.iter().position(|e| {
                e.skipped
                    && e.state == State::Cancelled
                    && e.needs.as_ref().is_some_and(|n| changed.iter().any(|&i| &next[i].id == n))
            });
            let Some(i) = waiting else { break };
            Self::check_willing(find(machines, &next[i].machine.id)?, next[i].op)?;
            fresh(&mut next[i], State::Pending, None);
            changed.push(i);
        }
        for &i in &changed {
            Self::ensure_room(&next, &next[i].machine, 0)?;
        }
        let snapshots = changed.iter().map(|&i| next[i].clone()).collect();
        *commands = next;
        Ok(snapshots)
    }

    pub fn retry_command(&self, id: &str, by: &str) -> Result<Command, HubError> {
        let _guard = self.lock.lock().unwrap();
        let machines = self.read_machines()?;
        let mut commands = self.read_commands()?;
        let (_, events) = Self::sweep(&mut commands);
        let changed = self.retry_at(&mut commands, &machines, id)?;
        self.write_commands(&commands)?;
        self.log_events(events);
        for c in &changed {
            self.command_log("retry", c, by);
        }
        Ok(changed.into_iter().next().unwrap())
    }

    /// Retry a plan from its first step that did not succeed.
    pub fn retry_plan(&self, id: &str, by: &str) -> Result<Plan, HubError> {
        let _guard = self.lock.lock().unwrap();
        let machines = self.read_machines()?;
        let mut commands = self.read_commands()?;
        let (_, events) = Self::sweep(&mut commands);
        let mut steps: Vec<&Command> = commands.iter().filter(|c| c.plan.as_deref() == Some(id)).collect();
        steps.sort_by_key(|c| c.step);
        if steps.is_empty() {
            return Err(HubError::NotFound);
        }
        // Not one that was skipped for an earlier step's sake: that step is
        // the one to retry (unless it succeeded after all, and the skip was
        // a cancel that arrived too late).
        let pick = steps
            .iter()
            .find(|c| {
                matches!(c.state, State::Blocked | State::Expired | State::Cancelled)
                    && c.needs.as_deref().is_none_or(|n| steps.iter().any(|p| p.id == n && p.state == State::Ok))
            })
            .map(|c| c.id.clone())
            .ok_or_else(|| bad("nothing in that plan can be retried"))?;
        let changed = self.retry_at(&mut commands, &machines, &pick)?;
        self.write_commands(&commands)?;
        self.log_events(events);
        for c in &changed {
            self.command_log("retry", c, by);
        }
        Ok(Plan::new(id, commands.into_iter().filter(|c| c.plan.as_deref() == Some(id)).collect()))
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
        let again = hub.retry_command(&made.id, "admin").unwrap();
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
        assert_eq!(hub.retry_command(&made.id, "admin").unwrap().state, State::Queued);
        assert!(hub.retry_command(&made.id, "admin").is_err(), "only a finished command is retried");
    }

    #[test]
    fn cancel_stops_a_waiting_command_and_only_asks_a_running_one() {
        let (_d, hub, a, _b) = hub_with_two();
        let first = hub.enqueue(push_request("alpha"), "admin").unwrap();
        assert_eq!(hub.cancel_command(&first.id, "admin").unwrap().state, State::Cancelled);
        assert!(hub.cancel_command(&first.id, "admin").is_err());
        let second = hub.enqueue(push_request("alpha"), "admin").unwrap();
        hub.claim(&a, &second.id).unwrap();
        let c = hub.cancel_command(&second.id, "admin").unwrap();
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
        put_copy_of(hub, by, "s1", parent)
    }

    fn put_copy_for(hub: &Hub, by: &Machine, session: &str) -> String {
        put_copy_of(hub, by, session, None)
    }

    fn put_copy_of(hub: &Hub, by: &Machine, session: &str, parent: Option<String>) -> String {
        put_copy_titled(hub, by, session, parent, "A title")
    }

    fn put_copy_titled(hub: &Hub, by: &Machine, session: &str, parent: Option<String>, title: &str) -> String {
        let blob = b"hello";
        let sha = crate::fsutil::sha256_hex(blob);
        hub.put_blob(&sha, &blob[..], 100).unwrap();
        let m = Manifest {
            schema: crate::hub::manifest::SCHEMA,
            agent: AgentKind::ClaudeCode,
            id: session.into(),
            title: Some(title.into()),
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
        hub.put_revision("claude-code", session, &serde_json::to_vec(&m).unwrap(), by).unwrap()
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
        hub.cancel_command(&made.id, "admin").unwrap();

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

    // --- plans ---

    fn send(session: &str) -> NewPlan {
        NewPlan { kind: "send".into(), agent: "claude-code".into(), session: session.into(), from: "alpha".into(), to: "beta".into(), exact: None }
    }

    fn done(code: Code, rev: Option<&str>) -> Report {
        Report { code, detail: None, rev: rev.map(String::from) }
    }

    fn state_of(hub: &Hub, id: &str) -> State {
        hub.command(id).unwrap().state
    }

    /// Alpha takes the plan's push and reports it done with `rev`.
    fn push_ok(hub: &Hub, a: &Machine, plan: &Plan, rev: &str) {
        hub.claim(a, &plan.steps[0].id).unwrap();
        hub.report(a, &plan.steps[0].id, done(Code::Ok, Some(rev))).unwrap();
    }

    #[test]
    fn a_plan_is_made_only_when_both_ends_could_do_their_part() {
        let (_d, hub, a, b) = hub_with_two();
        let refused = |hub: &Hub, edit: &dyn Fn(&mut NewPlan)| {
            let mut p = send("s1");
            edit(&mut p);
            hub.create_plan(p, "admin").unwrap_err().to_string()
        };
        assert!(refused(&hub, &|p| p.kind = "move".into()).contains("not a kind of plan"));
        assert!(refused(&hub, &|p| p.agent = "nope".into()).contains("unknown agent"));
        assert!(refused(&hub, &|p| p.session = "../x".into()).contains("not a session id"));
        assert!(refused(&hub, &|p| p.to = "alpha".into()).contains("both ends"));
        assert!(refused(&hub, &|p| p.to = "nobody".into()).contains("no machine"));

        // Beta allows only pushes: the pull step could never run.
        let push_only = Caps { v: PROTOCOL, ops: vec!["push".into()], enabled: true };
        hub.inbox(&b, push_only).unwrap();
        assert!(refused(&hub, &|_| {}).contains("beta does not allow pull"));
        hub.inbox(&b, caps(false)).unwrap();
        assert!(refused(&hub, &|_| {}).contains("beta has not turned remote control on"));
        hub.inbox(&b, caps(true)).unwrap();
        hub.inbox(&a, caps(false)).unwrap();
        assert!(refused(&hub, &|_| {}).contains("alpha has not turned remote control on"));
        hub.inbox(&a, caps(true)).unwrap();
        assert!(hub.commands(10).unwrap().is_empty(), "a refused plan leaves nothing behind");

        let plan = hub.create_plan(send("s1"), "admin").unwrap();
        assert_eq!((plan.kind.as_str(), plan.state), ("send", State::Queued));
        let [push, pull] = plan.steps.as_slice() else { panic!("two steps") };
        assert_eq!((push.op, push.step, push.state, push.machine.name.as_str()), (Op::Push, Some(1), State::Queued, "alpha"));
        assert_eq!((pull.op, pull.step, pull.state, pull.machine.name.as_str()), (Op::Pull, Some(2), State::Pending, "beta"));
        assert_eq!(pull.needs.as_deref(), Some(push.id.as_str()));
        assert_eq!(pull.from.as_ref().map(|m| m.name.as_str()), Some("alpha"));
        assert_eq!((push.plan.as_ref(), pull.plan.as_ref()), (Some(&plan.id), Some(&plan.id)));
        assert_eq!(pull.rev, None, "no revision until the push has made one");
        assert_eq!(push.args.exact, Some(false));
        assert_eq!(hub.plan(&plan.id).unwrap().steps.len(), 2);
        assert!(matches!(hub.plan("nope"), Err(HubError::NotFound)));
    }

    #[test]
    fn the_pull_is_invisible_until_the_push_succeeded_then_installs_exactly_its_revision() {
        let (_d, hub, a, b) = hub_with_two();
        let plan = hub.create_plan(send("s1"), "admin").unwrap();
        let (push, pull) = (plan.steps[0].id.clone(), plan.steps[1].id.clone());

        assert!(hub.inbox(&b, caps(true)).unwrap().is_empty(), "a pending step is not offered");
        assert!(matches!(hub.claim(&b, &pull), Err(HubError::Busy(_))), "nor can it be claimed");
        assert_eq!(hub.inbox(&a, caps(true)).unwrap().len(), 1);
        hub.claim(&a, &push).unwrap();
        assert!(hub.inbox(&b, caps(true)).unwrap().is_empty(), "still nothing while the push runs");
        assert_eq!(hub.plan(&plan.id).unwrap().state, State::Running);

        // A push that names a revision that is not on the hub does not move the plan.
        assert!(hub.report(&a, &push, done(Code::Ok, Some(&"0".repeat(64)))).is_err());
        assert_eq!(state_of(&hub, &pull), State::Pending);

        let rev = put_copy(&hub, &a, None);
        hub.report(&a, &push, done(Code::Ok, Some(&rev))).unwrap();
        let queued = hub.command(&pull).unwrap();
        assert_eq!((queued.state, queued.rev.as_deref()), (State::Queued, Some(rev.as_str())), "R was copied");
        assert_eq!(hub.command(&push).unwrap().rev, queued.rev, "and is the one the hub verified");
        assert_eq!(hub.plan(&plan.id).unwrap().state, State::Queued);

        let work = hub.inbox(&b, caps(true)).unwrap();
        assert_eq!(work.len(), 1);
        assert_eq!((work[0].op, work[0].rev.as_deref()), (Op::Pull, Some(rev.as_str())), "a pull is always offered with its revision");
        // A third machine pushing on top in between changes nothing about what arrives.
        let token = hub.join_token().unwrap();
        let c = hub.join(&token, "gamma").unwrap().machine;
        put_copy(&hub, &c, Some(rev.clone()));
        assert_eq!(hub.inbox(&b, caps(true)).unwrap()[0].rev.as_deref(), Some(rev.as_str()));
        hub.claim(&b, &pull).unwrap();
        assert_eq!(hub.plan(&plan.id).unwrap().state, State::Running);
        hub.report(&b, &pull, done(Code::Ok, None)).unwrap();
        let finished = hub.plan(&plan.id).unwrap();
        assert_eq!(finished.state, State::Ok);
        assert!(finished.steps.iter().all(|s| s.state == State::Ok && !s.skipped));
    }

    #[test]
    fn a_step_that_does_not_succeed_cancels_the_steps_waiting_for_it() {
        let (_d, hub, a, _b) = hub_with_two();
        let plan = hub.create_plan(send("s1"), "admin").unwrap();
        hub.claim(&a, &plan.steps[0].id).unwrap();
        hub.report(&a, &plan.steps[0].id, done(Code::Diverged, None)).unwrap();
        let after = hub.plan(&plan.id).unwrap();
        assert_eq!(after.state, State::Blocked);
        let pull = &after.steps[1];
        assert_eq!((pull.state, pull.skipped, pull.code), (State::Cancelled, true, Some(Code::Cancelled)));
        assert_eq!(pull.detail.as_deref(), Some("step 1 did not succeed"));

        // The machine turning remote control off blocks the push, and the pull with it.
        let plan = hub.create_plan(send("s2"), "admin").unwrap();
        assert!(hub.inbox(&a, caps(false)).unwrap().is_empty());
        let after = hub.plan(&plan.id).unwrap();
        assert_eq!(after.state, State::Blocked);
        assert_eq!((after.steps[0].code, after.steps[1].skipped), (Some(Code::RemoteOff), true));
    }

    #[test]
    fn cancelling_a_plan_stops_every_step_and_asks_a_running_one() {
        let (_d, hub, a, _b) = hub_with_two();
        let plan = hub.create_plan(send("s1"), "admin").unwrap();
        let cancelled = hub.cancel_plan(&plan.id, "admin").unwrap();
        assert_eq!(cancelled.state, State::Cancelled);
        assert_eq!((cancelled.steps[0].skipped, cancelled.steps[1].skipped), (false, true));
        assert!(hub.cancel_plan(&plan.id, "admin").is_err(), "nothing left to cancel");
        assert!(matches!(hub.cancel_plan("nope", "admin"), Err(HubError::NotFound)));

        let plan = hub.create_plan(send("s1"), "admin").unwrap();
        hub.claim(&a, &plan.steps[0].id).unwrap();
        let mid = hub.cancel_plan(&plan.id, "admin").unwrap();
        assert_eq!(mid.state, State::Running, "the running push may still finish");
        assert!(mid.steps[0].cancel_requested);
        assert_eq!((mid.steps[1].state, mid.steps[1].skipped), (State::Cancelled, true), "the pull will not follow it");
        let rev = put_copy(&hub, &a, None);
        hub.report(&a, &plan.steps[0].id, done(Code::Ok, Some(&rev))).unwrap();
        let end = hub.plan(&plan.id).unwrap();
        assert_eq!(end.steps[0].state, State::Ok, "its real result is recorded");
        assert_eq!((end.steps[1].state, end.state), (State::Cancelled, State::Cancelled), "and the plan is not ok");

        // Through the single-command route, a step's dependants go with it.
        let plan = hub.create_plan(send("s3"), "admin").unwrap();
        hub.cancel_command(&plan.steps[0].id, "admin").unwrap();
        let end = hub.plan(&plan.id).unwrap();
        assert_eq!((end.steps[1].state, end.steps[1].skipped), (State::Cancelled, true));
        // A pending step cancelled by itself is a plain cancel.
        let plan = hub.create_plan(send("s4"), "admin").unwrap();
        let alone = hub.cancel_command(&plan.steps[1].id, "admin").unwrap();
        assert_eq!((alone.state, alone.skipped), (State::Cancelled, false));
    }

    #[test]
    fn a_step_that_expires_takes_the_waiting_ones_with_it() {
        let (_d, hub, _a, _b) = hub_with_two();
        let plan = hub.create_plan(send("s1"), "admin").unwrap();
        let mut all = hub.read_commands().unwrap();
        all[0].expires = after(-1);
        hub.write_commands(&all).unwrap();
        let end = hub.plan(&plan.id).unwrap();
        assert_eq!((end.steps[0].state, end.steps[1].state, end.steps[1].skipped), (State::Expired, State::Cancelled, true));
        assert_eq!(end.state, State::Expired);

        // A plan whose steps all lapse together ends with the first one.
        let plan = hub.create_plan(send("s2"), "admin").unwrap();
        let mut all = hub.read_commands().unwrap();
        all.iter_mut().filter(|c| c.plan.as_deref() == Some(plan.id.as_str())).for_each(|c| c.expires = after(-1));
        hub.write_commands(&all).unwrap();
        let end = hub.plan(&plan.id).unwrap();
        assert_eq!((end.steps[0].state, end.steps[1].state, end.steps[1].skipped), (State::Expired, State::Cancelled, true));
        // A pending step never expires by itself while its parent legitimately runs.
        let (_d, hub, a, _b) = hub_with_two();
        let plan = hub.create_plan(send("s1"), "admin").unwrap();
        hub.claim(&a, &plan.steps[0].id).unwrap();
        let mut all = hub.read_commands().unwrap();
        all[1].expires = after(-1);
        hub.write_commands(&all).unwrap();
        assert_eq!(state_of(&hub, &plan.steps[1].id), State::Pending);
        let rev = put_copy(&hub, &a, None);
        hub.report(&a, &plan.steps[0].id, done(Code::Ok, Some(&rev))).unwrap();
        let pull = hub.command(&plan.steps[1].id).unwrap();
        assert_eq!(pull.state, State::Queued, "and it is queued, with a fresh week, when its parent ends");
        assert!(pull.expires > now());
    }

    #[test]
    fn retrying_a_plan_brings_the_skipped_steps_back_and_the_whole_thing_can_finish() {
        let (_d, hub, a, b) = hub_with_two();
        assert!(matches!(hub.retry_plan("nope", "admin"), Err(HubError::NotFound)));
        let plan = hub.create_plan(send("s1"), "admin").unwrap();
        assert!(hub.retry_plan(&plan.id, "admin").is_err(), "nothing has failed yet");
        hub.claim(&a, &plan.steps[0].id).unwrap();
        hub.report(&a, &plan.steps[0].id, done(Code::Failed, None)).unwrap();
        assert!(hub.retry_command(&plan.steps[1].id, "admin").unwrap_err().to_string().contains("step 1 did not succeed"));

        let again = hub.retry_plan(&plan.id, "admin").unwrap();
        assert_eq!(again.state, State::Queued);
        let [push, pull] = again.steps.as_slice() else { panic!() };
        assert_eq!((push.state, push.attempts, push.code), (State::Queued, 0, None));
        assert_eq!((pull.state, pull.skipped, pull.code, pull.detail.clone(), pull.rev.clone()), (State::Pending, false, None, None, None));
        assert!(hub.retry_plan(&plan.id, "admin").is_err(), "nothing is waiting to be retried now");

        let rev = put_copy(&hub, &a, None);
        push_ok(&hub, &a, &again, &rev);
        hub.claim(&b, &pull.id).unwrap();
        // The pull is refused by its machine: retrying it alone needs only itself.
        hub.report(&b, &pull.id, done(Code::Live, None)).unwrap();
        assert_eq!(hub.plan(&plan.id).unwrap().state, State::Blocked);
        let one = hub.retry_command(&pull.id, "admin").unwrap();
        assert_eq!((one.state, one.rev.as_deref()), (State::Queued, Some(rev.as_str())), "same revision as before");
        hub.claim(&b, &pull.id).unwrap();
        hub.report(&b, &pull.id, done(Code::Ok, None)).unwrap();
        assert_eq!(hub.plan(&plan.id).unwrap().state, State::Ok);

        // A cancel that arrived after the push finished: the plan can still be retried.
        let plan = hub.create_plan(send("s2"), "admin").unwrap();
        hub.claim(&a, &plan.steps[0].id).unwrap();
        hub.cancel_plan(&plan.id, "admin").unwrap();
        let rev = put_copy_for(&hub, &a, "s2");
        hub.report(&a, &plan.steps[0].id, done(Code::Ok, Some(&rev))).unwrap();
        assert_eq!(hub.plan(&plan.id).unwrap().state, State::Cancelled);
        let back = hub.retry_plan(&plan.id, "admin").unwrap();
        assert_eq!((back.steps[1].state, back.steps[1].rev.as_deref()), (State::Queued, Some(rev.as_str())));
    }

    #[test]
    fn a_retry_that_would_break_a_rule_changes_nothing() {
        let (_d, hub, a, _b) = hub_with_two();
        let plan = hub.create_plan(send("s1"), "admin").unwrap();
        hub.claim(&a, &plan.steps[0].id).unwrap();
        hub.report(&a, &plan.steps[0].id, done(Code::Failed, None)).unwrap();
        // Someone else takes the session meanwhile.
        let other = hub.enqueue(push_request("alpha"), "admin").unwrap();
        let err = hub.retry_plan(&plan.id, "admin").unwrap_err();
        assert!(matches!(err, HubError::Busy(_)), "{err}");
        assert_eq!(hub.plan(&plan.id).unwrap().state, State::Blocked);
        hub.cancel_command(&other.id, "admin").unwrap();
        // Beta turned remote control off: the pull step could not run, so nothing is queued.
        let mut machines = hub.read_machines().unwrap();
        machines.iter_mut().find(|m| m.name == "beta").unwrap().caps.as_mut().unwrap().enabled = false;
        hub.write_machines(&machines).unwrap();
        let err = hub.retry_plan(&plan.id, "admin").unwrap_err().to_string();
        assert!(err.contains("beta has not turned remote control on"), "{err}");
        assert_eq!(hub.plan(&plan.id).unwrap().steps[0].state, State::Blocked, "the push was not queued either");
    }

    #[test]
    fn all_the_steps_of_a_plan_are_one_command_for_the_session() {
        let (_d, hub, a, _b) = hub_with_two();
        let plan = hub.create_plan(send("s1"), "admin").unwrap();
        // Nothing else may touch the session while any step is not final...
        assert!(matches!(hub.create_plan(send("s1"), "admin"), Err(HubError::Busy(_))));
        assert!(matches!(hub.enqueue(push_request("beta"), "admin"), Err(HubError::Busy(_))));
        // ...even once the push is done and only the pull is left.
        let rev = put_copy(&hub, &a, None);
        push_ok(&hub, &a, &plan, &rev);
        let err = hub.create_plan(send("s1"), "admin").unwrap_err();
        assert!(matches!(err, HubError::Busy(_)), "{err}");
        // A different session is free.
        hub.create_plan(send("other"), "admin").unwrap();
        // The plan's own pending step does not block its own steps (the pull got queued).
        assert_eq!(hub.plan(&plan.id).unwrap().steps[1].state, State::Queued);
        hub.cancel_plan(&plan.id, "admin").unwrap();
        hub.create_plan(send("s1"), "admin").unwrap();
    }

    #[test]
    fn a_pending_step_counts_against_its_machines_queue() {
        let (_d, hub, _a, _b) = hub_with_two();
        // Every plan puts a queued push on alpha and a pending pull on beta.
        for n in 0..QUEUE_DEPTH {
            hub.create_plan(send(&format!("q{n}")), "admin").unwrap();
        }
        let err = hub.create_plan(send("one-too-many"), "admin").unwrap_err();
        assert!(matches!(err, HubError::Busy(_)) && err.to_string().contains("already has 20 commands waiting"), "{err}");
        // Beta holds only pending steps and is as full as alpha.
        let mut r = push_request("beta");
        r.session = "alone".into();
        let err = hub.enqueue(r, "admin").unwrap_err();
        assert!(err.to_string().contains("beta already has 20"), "{err}");
        assert_eq!(hub.plans(5).unwrap().len(), 5);
        assert_eq!(hub.plans(100).unwrap().len(), QUEUE_DEPTH);
        assert!(hub.plans(100).unwrap().windows(2).all(|w| w[0].created >= w[1].created), "newest first");
        assert_eq!(hub.commands(100).unwrap().len(), 2 * QUEUE_DEPTH, "steps are listed as commands too");
    }

    #[test]
    fn a_session_cannot_be_deleted_while_a_plan_holds_its_revision() {
        let (_d, hub, a, b) = hub_with_two();
        let rev = put_copy(&hub, &a, None);
        let plan = hub.create_plan(send("s1"), "admin").unwrap();
        let err = hub.delete_session("claude-code", "s1").unwrap_err();
        assert!(matches!(err, HubError::Busy(_)) && err.to_string().contains("still in flight"), "{err}");
        push_ok(&hub, &a, &plan, &rev);
        assert!(hub.delete_session("claude-code", "s1").is_err(), "the pull is still to come");
        hub.claim(&b, &plan.steps[1].id).unwrap();
        assert!(hub.delete_session("claude-code", "s1").is_err(), "and while it runs");
        hub.report(&b, &plan.steps[1].id, done(Code::Ok, None)).unwrap();
        assert_eq!(hub.delete_session("claude-code", "s1").unwrap(), 1);
    }

    // --- review fixes ---

    fn log_lines(hub: &Hub) -> Vec<serde_json::Value> {
        let text = fs::read_to_string(hub.root().join("commands.log")).unwrap_or_default();
        text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    #[test]
    fn a_push_may_not_claim_an_older_revision_of_its_own() {
        let (_d, hub, a, b) = hub_with_two();
        let old = put_copy(&hub, &a, None);
        let on_top = put_copy(&hub, &b, Some(old.clone()));
        // The command is asked for after both: alpha's `old` is neither the head nor made for it.
        let made = hub.enqueue(push_request("alpha"), "admin").unwrap();
        hub.claim(&a, &made.id).unwrap();
        let err = hub.report(&a, &made.id, done(Code::Ok, Some(&old))).unwrap_err();
        assert!(matches!(err, HubError::BadRequest(_)), "{err}");
        // What it pushes for this command counts even when someone pushes on top before it reports.
        let mine = put_copy(&hub, &a, Some(on_top));
        let theirs = put_copy(&hub, &b, Some(mine.clone()));
        assert_eq!(hub.report(&a, &made.id, done(Code::Ok, Some(&mine))).unwrap().rev.as_deref(), Some(mine.as_str()));
        // "Already in sync" can only name the head.
        let next = hub.enqueue(push_request("alpha"), "admin").unwrap();
        hub.claim(&a, &next.id).unwrap();
        assert!(hub.report(&a, &next.id, done(Code::InSync, Some(&mine))).is_err(), "mine is not the head any more");
        assert!(hub.report(&a, &next.id, done(Code::InSync, Some(&theirs))).is_ok());
    }

    #[test]
    fn a_running_step_asked_to_stop_ends_when_its_lease_does_and_cannot_be_claimed() {
        let (_d, hub, a, _b) = hub_with_two();
        let plan = hub.create_plan(send("s1"), "admin").unwrap();
        hub.claim(&a, &plan.steps[0].id).unwrap();
        let mid = hub.cancel_plan(&plan.id, "admin").unwrap();
        assert_eq!(
            mid.steps[1].detail.as_deref(),
            Some("cancelled with the plan before step 1 finished"),
            "the pull says why it did not follow"
        );
        // The machine never reports: the lease runs out.
        let mut all = hub.read_commands().unwrap();
        all[0].lease_until = Some(after(-5));
        hub.write_commands(&all).unwrap();
        let end = hub.plan(&plan.id).unwrap();
        assert_eq!((end.steps[0].state, end.steps[0].code, end.steps[0].detail.as_deref()), (State::Cancelled, Some(Code::Cancelled), Some("cancelled")));
        assert_eq!(end.state, State::Cancelled);
        assert!(hub.create_plan(send("s1"), "admin").is_ok(), "the session is free again");

        // A queued command that was asked to stop is never claimed.
        let one = hub.enqueue(push_request("alpha"), "admin").unwrap_err();
        assert!(matches!(one, HubError::Busy(_)));
        let lone = {
            let mut r = push_request("alpha");
            r.session = "s9".into();
            hub.enqueue(r, "admin").unwrap()
        };
        let mut all = hub.read_commands().unwrap();
        all.iter_mut().find(|c| c.id == lone.id).unwrap().cancel_requested = true;
        hub.write_commands(&all).unwrap();
        assert!(matches!(hub.claim(&a, &lone.id), Err(HubError::Busy(_))));
        assert_eq!(state_of(&hub, &lone.id), State::Cancelled);
    }

    #[test]
    fn a_retry_is_refused_while_the_step_before_it_is_being_cancelled() {
        let (_d, hub, a, _b) = hub_with_two();
        let plan = hub.create_plan(send("s1"), "admin").unwrap();
        hub.claim(&a, &plan.steps[0].id).unwrap();
        hub.cancel_plan(&plan.id, "admin").unwrap();
        let err = hub.retry_command(&plan.steps[1].id, "admin").unwrap_err();
        assert!(matches!(err, HubError::BadRequest(_)) && err.to_string().contains("step 1 is being cancelled; wait for it to finish"), "{err}");
        assert_eq!(state_of(&hub, &plan.steps[1].id), State::Cancelled, "nothing changed");
    }

    #[test]
    fn a_pull_is_never_queued_without_a_revision_that_exists() {
        let (_d, hub, a, _b) = hub_with_two();
        // A pull whose pinned revision is gone cannot be retried.
        put_copy(&hub, &a, None);
        let pull = NewCommand { op: Op::Pull, machine: "beta".into(), agent: "claude-code".into(), session: "s1".into(), from: Some("alpha".into()), exact: None };
        let made = hub.enqueue(pull, "admin").unwrap();
        hub.cancel_command(&made.id, "admin").unwrap();
        let mut all = hub.read_commands().unwrap();
        all[0].rev = Some("0".repeat(64));
        hub.write_commands(&all).unwrap();
        let err = hub.retry_command(&made.id, "admin").unwrap_err();
        assert!(matches!(err, HubError::BadRequest(_)) && err.to_string().contains("that revision is gone; start a new plan"), "{err}");

        // A push that is ok but left no revision: the pull behind it is skipped, and cannot be retried.
        let plan = hub.create_plan(send("s2"), "admin").unwrap();
        let mut all = hub.read_commands().unwrap();
        let push = all.iter_mut().find(|c| c.id == plan.steps[0].id).unwrap();
        push.state = State::Ok;
        push.code = Some(Code::Ok);
        hub.write_commands(&all).unwrap();
        let pull = hub.command(&plan.steps[1].id).unwrap();
        assert_eq!((pull.state, pull.skipped), (State::Cancelled, true), "not queued without a revision");
        assert!(pull.detail.unwrap().contains("produced no revision"));
        let err = hub.retry_command(&plan.steps[1].id, "admin").unwrap_err();
        assert!(err.to_string().contains("start a new plan"), "{err}");
    }

    #[test]
    fn titles_are_clipped_and_cleaned_where_they_are_copied() {
        let (_d, hub, a, _b) = hub_with_two();
        let dirty = format!("\x1b[31mred\n{}", "t".repeat(5000));
        put_copy_titled(&hub, &a, "s1", None, &dirty);
        let pull = NewCommand { op: Op::Pull, machine: "beta".into(), agent: "claude-code".into(), session: "s1".into(), from: Some("alpha".into()), exact: None };
        let made = hub.enqueue(pull, "admin").unwrap();
        let title = made.title.unwrap();
        assert_eq!(title.chars().count(), TITLE_MAX);
        assert!(title.starts_with(" [31mred "), "{title:?}");
        hub.cancel_command(&made.id, "admin").unwrap();
        let plan = hub.create_plan(send("s1"), "admin").unwrap();
        assert!(plan.steps.iter().all(|s| s.title.as_deref().unwrap().chars().count() == TITLE_MAX));
    }

    #[test]
    fn a_machines_detail_carries_no_control_characters() {
        let (_d, hub, a, _b) = hub_with_two();
        let made = hub.enqueue(push_request("alpha"), "admin").unwrap();
        hub.claim(&a, &made.id).unwrap();
        let r = Report { code: Code::Failed, detail: Some("a\nstep 2: ok\x1b[2J\r\tb\u{85}c".into()), rev: None };
        let detail = hub.report(&a, &made.id, r).unwrap().detail.unwrap();
        assert_eq!(detail, "a step 2: ok [2J  b c");
        assert!(!detail.chars().any(char::is_control));
    }

    #[test]
    fn every_state_change_is_logged_after_it_was_stored_with_who_and_which_revision() {
        let (_d, hub, a, _b) = hub_with_two();
        let plan = hub.create_plan(send("s1"), "commands-token").unwrap();
        hub.claim(&a, &plan.steps[0].id).unwrap();
        let rev = put_copy(&hub, &a, None);
        let r = Report { code: Code::Ok, detail: Some("SECRET-DETAIL".into()), rev: Some(rev.clone()) };
        hub.report(&a, &plan.steps[0].id, r).unwrap();
        let find = |lines: &[serde_json::Value], event: &str, id: &str| lines.iter().find(|l| l["event"] == event && l["id"] == id).cloned();
        let lines = log_lines(&hub);
        assert_eq!(find(&lines, "enqueue", &plan.steps[0].id).unwrap()["by"], "commands-token");
        assert_eq!(find(&lines, "claim", &plan.steps[0].id).unwrap()["by"], format!("machine:{}", a.id));
        let result = find(&lines, "result", &plan.steps[0].id).unwrap();
        assert_eq!((result["rev"].as_str(), result["by"].as_str()), (Some(rev.as_str()), Some(format!("machine:{}", a.id).as_str())));
        let advance = find(&lines, "advance", &plan.steps[1].id).unwrap();
        assert_eq!((advance["rev"].as_str(), advance["by"].as_str()), (Some(rev.as_str()), Some("hub")));
        let text = fs::read_to_string(hub.root().join("commands.log")).unwrap();
        assert!(!text.contains("SECRET-DETAIL"), "the detail is not logged");

        // What a timeout does, sweep included: expiry of a step and the skip it causes.
        let plan = hub.create_plan(send("s2"), "admin").unwrap();
        let mut all = hub.read_commands().unwrap();
        all.iter_mut().find(|c| c.id == plan.steps[0].id).unwrap().expires = after(-1);
        hub.write_commands(&all).unwrap();
        hub.commands(10).unwrap();
        let lines = log_lines(&hub);
        assert_eq!(find(&lines, "expire", &plan.steps[0].id).unwrap()["by"], "hub");
        assert_eq!(find(&lines, "skip", &plan.steps[1].id).unwrap()["by"], "hub");
        // And a lost lease past the attempts.
        let plan = hub.create_plan(send("s3"), "admin").unwrap();
        hub.claim(&a, &plan.steps[0].id).unwrap();
        let mut all = hub.read_commands().unwrap();
        let push = all.iter_mut().find(|c| c.id == plan.steps[0].id).unwrap();
        push.attempts = MAX_ATTEMPTS;
        push.lease_until = Some(after(-5));
        hub.write_commands(&all).unwrap();
        hub.inbox(&a, caps(true)).unwrap();
        let lines = log_lines(&hub);
        assert_eq!(find(&lines, "lease", &plan.steps[0].id).unwrap()["code"], "failed");
        assert!(find(&lines, "skip", &plan.steps[1].id).is_some());

        // A plan cancel logs each step it cancelled, by the actor, and the skips it caused once.
        let plan = hub.create_plan(send("s4"), "admin").unwrap();
        hub.cancel_plan(&plan.id, "commands-token").unwrap();
        let lines = log_lines(&hub);
        let cancels: Vec<_> = lines.iter().filter(|l| l["event"] == "cancel" && l["plan"] == plan.id.as_str()).collect();
        assert_eq!(cancels.len(), 1, "the pull was skipped, not cancelled on its own");
        assert_eq!(cancels[0]["by"], "commands-token");
        assert!(find(&lines, "skip", &plan.steps[1].id).is_some());
        // Retried by someone.
        hub.retry_plan(&plan.id, "admin").unwrap();
        assert_eq!(find(&log_lines(&hub), "retry", &plan.steps[0].id).unwrap()["by"], "admin");
    }

    #[test]
    fn a_full_store_makes_room_from_the_oldest_finished_and_refuses_only_when_all_are_open() {
        let (_d, hub, _a, _b) = hub_with_two();
        let made = hub.enqueue(push_request("alpha"), "admin").unwrap();
        hub.cancel_command(&made.id, "admin").unwrap();
        let template = hub.read_commands().unwrap().remove(0);
        // Full of finished commands: the oldest tenth goes, the new one is made.
        let mut all: Vec<Command> = (0..MAX_STORED).map(|n| Command { id: format!("{n:016x}"), ..template.clone() }).collect();
        for (n, c) in all.iter_mut().enumerate() {
            c.updated = Timestamp::from_second(now().as_second() - 10_000 + n as i64).unwrap();
        }
        hub.write_commands(&all).unwrap();
        let fresh = hub.enqueue(push_request("alpha"), "admin").unwrap();
        let after = hub.read_commands().unwrap();
        assert_eq!(after.len(), MAX_STORED - MAX_STORED / 10 + 1);
        assert!(after.iter().any(|c| c.id == fresh.id));
        assert!(!after.iter().any(|c| c.id == format!("{:016x}", 0)), "the oldest went");
        assert!(after.iter().any(|c| c.id == format!("{:016x}", MAX_STORED - 1)), "the newest stayed");
        // Full of open ones: nothing can be dropped, so it refuses.
        let open: Vec<Command> = (0..MAX_STORED)
            .map(|n| Command { id: format!("{n:016x}"), state: State::Queued, code: None, session: format!("s{n}"), ..template.clone() })
            .collect();
        hub.write_commands(&open).unwrap();
        let err = hub.enqueue(push_request("beta"), "admin").unwrap_err();
        assert!(matches!(err, HubError::Busy(_)) && err.to_string().contains("open at once"), "{err}");
        assert!(matches!(hub.create_plan(send("s1"), "admin"), Err(HubError::Busy(_))));
    }

    #[test]
    fn a_retried_step_is_a_new_ask() {
        let (_d, hub, a, _b) = hub_with_two();
        let made = hub.enqueue(push_request("alpha"), "admin").unwrap();
        hub.claim(&a, &made.id).unwrap();
        hub.report(&a, &made.id, Report { code: Code::Failed, detail: None, rev: None }).unwrap();
        let mut all = hub.read_commands().unwrap();
        let old = after(-100);
        all[0].created = old;
        hub.write_commands(&all).unwrap();
        assert!(hub.retry_command(&made.id, "admin").unwrap().created > old);
    }

    #[test]
    fn a_machine_is_found_by_id_before_by_name() {
        let (_d, hub, a, b) = hub_with_two();
        // A name equal to another machine's id cannot be taken.
        let token = hub.join_token().unwrap();
        let err = hub.join(&token, &a.id).unwrap_err();
        assert!(matches!(err, HubError::BadRequest(_)), "{err}");
        // One that already is (older hub) does not shadow the id.
        let mut machines = hub.read_machines().unwrap();
        machines.iter_mut().find(|m| m.id == b.id).unwrap().name = a.id.clone();
        hub.write_machines(&machines).unwrap();
        let made = hub.enqueue(push_request(&a.id), "admin").unwrap();
        assert_eq!(made.machine.id, a.id);
        // Duplicate names still need the id.
        let token = hub.join_token().unwrap();
        hub.join(&token, "alpha").unwrap();
        assert!(hub.enqueue(push_request("alpha"), "admin").unwrap_err().to_string().contains("use the machine's id"));
    }

    #[test]
    fn a_limit_never_cuts_a_plan_to_its_pull_step() {
        let (_d, hub, _a, _b) = hub_with_two();
        hub.create_plan(send("p1"), "admin").unwrap();
        let newest = hub.create_plan(send("p2"), "admin").unwrap();
        let listed = hub.commands(1).unwrap();
        let ids: Vec<&str> = listed.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, [newest.steps[1].id.as_str(), newest.steps[0].id.as_str()], "both steps of the newest plan, newest first");
        assert_eq!(hub.commands(3).unwrap().len(), 4, "a plan cut in the middle comes whole");
        let plans = hub.plans(1).unwrap();
        assert_eq!((plans.len(), plans[0].steps.len()), (1, 2));
        // A command outside any plan is just one.
        let mut r = push_request("alpha");
        r.session = "solo".into();
        hub.enqueue(r, "admin").unwrap();
        assert_eq!(hub.commands(1).unwrap().len(), 1);
    }
}
