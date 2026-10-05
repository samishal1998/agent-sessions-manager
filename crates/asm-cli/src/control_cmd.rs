//! `asm control`: ask other machines to push or pull a session through the
//! hub, send a session from one machine to another, and decide whether this
//! one may be asked.
//!
//! Two sides, one namespace. `enable` / `disable` / `status` are about this
//! machine (its daemon does what the hub asks only after `enable`). The rest
//! create and follow commands, with a commands token — this machine need not
//! be the one asked, or even have an agent.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use clap::Subcommand;

use asm_core::hub::client::{self, Control};
use asm_core::hub::commands::{Code, Command as Job, NewCommand, NewPlan, Op, Plan, State};
use asm_core::hub::control as local;
use asm_core::hub::daemon::{self, DaemonState};
use asm_core::model::AgentKind;

#[derive(Subcommand)]
pub enum ControlCommand {
    /// Let the hub ask this machine to push and pull sessions. Off until you
    /// run this; needs the daemon (`asm daemon install`) to be running.
    Enable {
        /// Allow only these operations (comma separated): push, pull.
        #[arg(long, value_delimiter = ',', default_value = "push,pull")]
        allow: Vec<String>,
    },
    /// Stop doing what the hub asks. Anything waiting for this machine is
    /// refused the next time it asks.
    Disable,
    /// Whether this machine may be asked, and whether its daemon is there to
    /// answer.
    Status,
    /// Ask a machine to push a session to the hub.
    Push {
        /// The machine to ask (id, or its name when unique).
        machine: String,
        /// The session: `agent:id`, or anything on the hub that names it.
        r#ref: String,
        /// Have it read the whole session and settle for nothing less than the
        /// hub holding exactly that copy.
        #[arg(long)]
        exact: bool,
        /// Wait for the result (up to ten minutes). Exit status: 0 ok, 1 not
        /// ok, 2 still waiting, 3 could not reach the hub.
        #[arg(long)]
        wait: bool,
    },
    /// Ask a machine to pull a session from the hub, exactly as the hub holds
    /// it now.
    Pull {
        /// The machine to ask (id, or its name when unique).
        machine: String,
        /// The session on the hub (id, prefix, `agent:prefix`).
        r#ref: String,
        /// The machine that pushed the copy to install. Default: whoever
        /// pushed the hub's current copy.
        #[arg(long)]
        from: Option<String>,
        /// Wait for the result (up to ten minutes). Exit status: 0 ok, 1 not
        /// ok, 2 still waiting, 3 could not reach the hub.
        #[arg(long)]
        wait: bool,
    },
    /// Copy a session from one machine to another: the first pushes it, and
    /// only once that succeeded does the second pull exactly that copy. The
    /// first machine keeps its own copy; nothing is archived or deleted.
    Send {
        /// The session: `agent:id`, or anything on the hub that names it.
        r#ref: String,
        /// The machine that pushes (id or name). Default: whoever pushed the
        /// hub's current copy (so name it if that is the machine you send to).
        #[arg(long)]
        from: Option<String>,
        /// The machine that pulls (id or name).
        #[arg(long)]
        to: String,
        /// Have the first machine read the whole session and settle for
        /// nothing less than the hub holding exactly that copy.
        #[arg(long)]
        exact: bool,
        /// Follow the plan until every step has ended (up to ten minutes).
        /// Exit status: 0 plan ok, 1 blocked, cancelled or expired, 2 still
        /// waiting, 3 could not reach the hub.
        #[arg(long)]
        wait: bool,
    },
    /// Recent commands and plans, newest first.
    Jobs {
        /// How many commands to list. A plan's steps count as commands, and
        /// the hub keeps a plan's steps together, so the list can run a step
        /// or two past it.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// One plan (a timeline of its steps) or one command, in full.
    Show { id: String },
    /// Stop a command or a whole plan that has not run (a step already
    /// running may finish).
    Cancel { id: String },
    /// Queue a blocked, expired or cancelled command again, or a plan from
    /// the step that did not succeed.
    Retry { id: String },
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The lowercase word a serde enum serializes as.
fn word<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

fn client() -> anyhow::Result<Control> {
    let token = std::env::var("ASM_HUB_COMMANDS_TOKEN")
        .or_else(|_| std::env::var("ASM_HUB_ADMIN_TOKEN"))
        .context("set ASM_HUB_COMMANDS_TOKEN to the token `asm hub commands-token` prints on the hub")?;
    let url = match std::env::var("ASM_HUB_URL") {
        Ok(url) => url,
        Err(_) => client::load().context("no hub to talk to: join one, or set ASM_HUB_URL")?.url,
    };
    Ok(Control::new(&url, &token)?)
}

/// What the session is called, from what the person typed: looked up on the
/// hub when this machine has joined one, else `agent:id` taken as it is.
fn session(query: &str) -> anyhow::Result<(AgentKind, String)> {
    if let Ok(remote) = client::load()
        && let Ok(heads) = remote.heads()
        && let Ok(head) = asm_core::hub::actions::resolve_head(&heads, query)
    {
        return Ok((head.manifest.agent, head.manifest.id.clone()));
    }
    match query.split_once(':') {
        Some((agent, id)) if !id.is_empty() => match AgentKind::parse(agent) {
            Some(agent) => Ok((agent, id.to_string())),
            None => bail!("{agent:?} is not an agent"),
        },
        _ => bail!("name the session as agent:id (this machine cannot look it up on a hub)"),
    }
}

/// What to do about a command that ended the way `code` says. The one place
/// the CLI words these; the admin page has the same table (commands.js).
fn hint(code: Code, op: Op) -> Option<&'static str> {
    Some(match (code, op) {
        // A force push from the target would replace the hub's current copy with the target's divergent one.
        (Code::Diverged, Op::Pull) => "The target machine has changes the sent copy does not. Decide which to keep there, then retry.",
        (Code::Diverged, Op::Push) => "Both machines changed this session. Resolve it on the machine, or push with --force there.",
        (Code::Ahead, _) => "That machine's copy is newer than the one being pulled.",
        (Code::Live, _) => "The session is running on that machine. Close it and retry.",
        (Code::NoDir, _) => "The project folder does not exist on that machine. Pull it there once with --project-dir.",
        (Code::NotRestorable, _) => "This agent's sessions cannot be restored onto a machine yet.",
        (Code::HubNewer, _) => "The hub has a newer copy from another machine. Pull it first.",
        (Code::Conflict, _) => "Another machine pushed at the same time. Retry.",
        (Code::RemoteOff, _) => "Remote control is off on that machine (`asm control enable` there).",
        (Code::Unsupported, _) => "That machine's asm is too old for this command.",
        (Code::Expired, _) => "The machine did not pick this up in time. Retry when it is online.",
        _ => return None,
    })
}

fn line(job: &Job) -> String {
    let short: String = job.session.chars().take(8).collect();
    let who = match &job.from {
        Some(from) => format!("{} <- {}", job.machine.name, from.name),
        None => job.machine.name.clone(),
    };
    let age = {
        let secs = (now() - job.created.as_second()).max(0) as u64;
        daemon::ago(secs)
    };
    let result = match (&job.code, &job.detail) {
        (Some(code), Some(detail)) => format!("{}: {detail}", word(code)),
        (Some(code), None) => word(code),
        _ => String::new(),
    };
    format!("{}  {:<9} {:<4} {}:{short}  on {who}  {age}  {result}", job.id, word(&job.state), word(&job.op), job.agent)
}

fn show(job: &Job, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(job)?);
        return Ok(());
    }
    println!("{}", line(job));
    if let Some(h) = job.code.and_then(|c| hint(c, job.op)).filter(|_| job.state.is_final()) {
        println!("  {h}");
    }
    if job.cancel_requested && job.state == State::Running {
        println!("  cancel requested; it may still finish");
    }
    if job.attempts > 1 {
        println!("  attempts: {}", job.attempts);
    }
    Ok(())
}

/// How long `--wait` follows a command, and how it ends: the exit status a script can act on.
const WAIT_SECS: i64 = 600;
const EXIT_NOT_OK: i32 = 1;
const EXIT_STILL_WAITING: i32 = 2;
const EXIT_NO_HUB: i32 = 3;
/// A poll that fails (network, a hub restarting) is tried again this many times, this far apart.
const POLL_TRIES: u32 = 5;
const POLL_GAP: Duration = Duration::from_secs(2);

/// An error worth asking again after: the hub was not reached, or it failed.
fn transient(e: &asm_core::CoreError) -> bool {
    let text = e.to_string();
    text.contains("could not reach the hub") || text.contains("hub answered 5")
}

/// Ask the hub, trying again a few times: a blip should not end a wait that is minutes in.
/// Exits with `EXIT_NO_HUB` when the hub stays out of reach (the command itself is not affected).
fn poll<T>(id: &str, mut ask: impl FnMut() -> Result<T, asm_core::CoreError>) -> T {
    let mut tries = 0;
    loop {
        match ask() {
            Ok(v) => return v,
            // A refusal (a bad token, a plan that is gone) will not change by
            // asking again: say so now instead of ten seconds later.
            Err(e) if !transient(&e) => {
                eprintln!("{e}");
                std::process::exit(1)
            }
            Err(e) if tries + 1 >= POLL_TRIES => {
                eprintln!("could not talk to the hub after {POLL_TRIES} tries: {e}\nThe command is not affected. `asm control show {id}` follows it.");
                std::process::exit(EXIT_NO_HUB)
            }
            Err(e) => {
                tries += 1;
                eprintln!("the hub did not answer ({e}); trying again");
                std::thread::sleep(POLL_GAP);
            }
        }
    }
}

/// The step (or lone command) a wait that ran out is still on, and what that means.
fn still_waiting(jobs: &[Job], id: &str) -> String {
    let Some(job) = jobs.iter().find(|j| !j.state.is_final()) else {
        return format!("still waiting. `asm control show {id}` follows it.");
    };
    let what = match job.step {
        Some(n) => format!("step {n} ({} on {})", word(&job.op), job.machine.name),
        None => format!("the {} on {}", word(&job.op), job.machine.name),
    };
    let state = match job.state {
        State::Running => format!("{what} is still running."),
        State::Pending => format!("{what} is waiting for the step before it."),
        _ => format!("{what} is still queued: it stays queued until {}'s daemon asks the hub, and expires after 7 days.", job.machine.name),
    };
    format!("still waiting after {} minutes: {state} `asm control show {id}` follows it.", WAIT_SECS / 60)
}

/// Poll until the command ends (or ten minutes pass), then say how it went.
fn wait_for(ctl: &Control, id: &str, json: bool) -> anyhow::Result<()> {
    let deadline = now() + WAIT_SECS;
    let mut job = poll(id, || ctl.get(id));
    let mut last = job.state;
    while !job.state.is_final() && now() < deadline {
        std::thread::sleep(POLL_GAP);
        job = poll(id, || ctl.get(id));
        if !json && job.state != last && !job.state.is_final() {
            eprintln!("{}: {}", job.id, word(&job.state));
        }
        last = job.state;
    }
    show(&job, json)?;
    match job.state {
        State::Ok => Ok(()),
        s if s.is_final() => std::process::exit(EXIT_NOT_OK),
        _ => {
            eprintln!("{}", still_waiting(std::slice::from_ref(&job), id));
            std::process::exit(EXIT_STILL_WAITING)
        }
    }
}

/// One step of a plan, indented under it.
fn step_line(job: &Job) -> String {
    let n = job.step.map_or("-".into(), |n| n.to_string());
    let skipped = if job.skipped { " (skipped)" } else { "" };
    format!("  {n}. {}{skipped}", line(job))
}

fn plan_head(plan: &Plan) -> String {
    let session = plan.steps.first().map_or(String::new(), |s| format!("{}:{}", s.agent, s.session.chars().take(8).collect::<String>()));
    let (from, to) = match plan.steps.as_slice() {
        [first, .., last] => (first.machine.name.as_str(), last.machine.name.as_str()),
        _ => ("", ""),
    };
    let age = daemon::ago((now() - plan.created.as_second()).max(0) as u64);
    format!("{}  plan {:<9} {} {session}  {from} -> {to}  {age}", plan.id, word(&plan.state), plan.kind)
}

fn show_plan(plan: &Plan, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(plan)?);
        return Ok(());
    }
    println!("{}", plan_head(plan));
    for step in &plan.steps {
        println!("{}", step_line(step));
        let when = daemon::ago((now() - step.updated.as_second()).max(0) as u64);
        let note = match (step.state, step.attempts, step.cancel_requested) {
            (State::Pending, ..) => "waiting for the step before it".to_string(),
            (State::Running, _, true) => format!("cancel requested; it may still finish ({when})"),
            (_, n, _) if n > 1 => format!("{n} attempts, last change {when}"),
            _ => format!("last change {when}"),
        };
        println!("       {note}");
    }
    if let Some(line) = stopped(plan) {
        println!("{line}");
    }
    Ok(())
}

/// The step a plan stopped at (the first that did not succeed, preferring one that really ended
/// over one that was only skipped) and what to do about it, or `None` while the plan is going or ok.
fn stopped(plan: &Plan) -> Option<String> {
    if plan.state == State::Ok || !plan.steps.iter().all(|s| s.state.is_final()) {
        return None;
    }
    let step = plan.steps.iter().find(|s| s.state != State::Ok && !s.skipped).or_else(|| plan.steps.iter().find(|s| s.state != State::Ok))?;
    let n = step.step.unwrap_or(1);
    let mut out = if n > 1 {
        format!("step {n} stopped; earlier steps stay done. `asm control retry {}` queues step {n} again.", plan.id)
    } else {
        format!("step {n} stopped. `asm control retry {}` queues it again.", plan.id)
    };
    if let Some(h) = step.code.and_then(|c| hint(c, step.op)) {
        out.push(' ');
        out.push_str(h);
    }
    Some(out)
}

/// Poll the plan until every step has ended (or ten minutes pass), printing
/// each step's result as it comes and, on stderr, each step that starts. The exit status says how it went.
fn wait_for_plan(ctl: &Control, id: &str, json: bool) -> anyhow::Result<()> {
    let deadline = now() + WAIT_SECS;
    let mut seen = std::collections::HashSet::new();
    let mut last = std::collections::HashMap::new();
    loop {
        let plan = poll(id, || ctl.plan(id));
        for step in &plan.steps {
            let was = last.insert(step.id.clone(), (step.state, step.skipped));
            if json {
                continue;
            }
            if step.state.is_final() {
                if seen.insert(step.id.clone()) {
                    println!("{}", step_line(step));
                }
            } else if was.is_some_and(|w| w != (step.state, step.skipped)) {
                eprintln!("step {} ({} on {}): {}", step.step.unwrap_or(0), word(&step.op), step.machine.name, word(&step.state));
            }
        }
        if plan.steps.iter().all(|s| s.state.is_final()) {
            if json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                println!("plan {}", word(&plan.state));
                if let Some(line) = stopped(&plan) {
                    println!("{line}");
                }
            }
            match plan.state {
                State::Ok => return Ok(()),
                _ => std::process::exit(EXIT_NOT_OK),
            }
        }
        if now() >= deadline {
            eprintln!("{}", still_waiting(&plan.steps, id));
            std::process::exit(EXIT_STILL_WAITING);
        }
        std::thread::sleep(POLL_GAP);
    }
}

/// A plan if `id` names one, else a command.
fn plan_named(ctl: &Control, id: &str) -> Option<Plan> {
    match ctl.plan(id) {
        Ok(plan) => Some(plan),
        // Not a plan id: it may name a single command.
        Err(e) if e.to_string().contains("answered 404") => None,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1)
        }
    }
}

pub fn run(command: ControlCommand, json: bool) -> anyhow::Result<()> {
    match command {
        ControlCommand::Enable { allow } => {
            let ops: Vec<Op> = allow
                .iter()
                .map(|a| match a.as_str() {
                    "push" => Ok(Op::Push),
                    "pull" => Ok(Op::Pull),
                    other => Err(anyhow::anyhow!("{other:?} is not an operation (push, pull)")),
                })
                .collect::<Result<_, _>>()?;
            let remote = client::load()?;
            let config = local::enable(&remote.url, &ops)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&config)?);
                return Ok(());
            }
            println!("Remote control is on: {} may ask this machine to {}.", remote.url, config.allow.join(" and "));
            if daemon::status()?.state != DaemonState::Running {
                println!("Nothing will happen until its daemon runs: `asm daemon install` (or `asm daemon start`).");
            }
            println!("`asm control disable` turns it off.");
        }
        ControlCommand::Disable => {
            local::disable()?;
            println!("Remote control is off. The hub can no longer ask this machine to do anything.");
        }
        ControlCommand::Status => {
            let config = local::Config::load();
            let running = daemon::status()?.state == DaemonState::Running;
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "enabled": config.enabled, "allow": config.allow, "daemon_running": running })
                );
                return Ok(());
            }
            if config.enabled {
                println!("Remote control: on ({})", config.allow.join(", "));
            } else {
                println!("Remote control: off (`asm control enable` turns it on)");
            }
            println!("Daemon: {}", if running { "running" } else { "not running: it is what answers the hub" });
        }
        ControlCommand::Push { machine, r#ref, exact, wait } => {
            let (agent, id) = session(&r#ref)?;
            let ctl = client()?;
            let job = ctl.create(&NewCommand {
                op: Op::Push,
                machine,
                agent: agent.as_str().into(),
                session: id,
                from: None,
                exact: exact.then_some(true),
            })?;
            queued(&job, json, wait, &ctl)?;
        }
        ControlCommand::Pull { machine, r#ref, from, wait } => {
            let (agent, id) = session(&r#ref)?;
            let remote = client::load().context("a pull is resolved against the hub, so this machine must have joined it")?;
            let history = remote
                .history(agent.as_str(), &id)?
                .with_context(|| format!("{agent}:{id} is not on the hub"))?;
            let from = match from {
                Some(from) => from,
                None => history
                    .manifest
                    .machine
                    .as_ref()
                    .map(|m| m.id.clone())
                    .context("the hub does not know which machine pushed that copy; pass --from")?,
            };
            let ctl = client()?;
            let job = ctl.create(&NewCommand {
                op: Op::Pull,
                machine,
                agent: agent.as_str().into(),
                session: id,
                from: Some(from),
                exact: None,
            })?;
            queued(&job, json, wait, &ctl)?;
        }
        ControlCommand::Send { r#ref, from, to, exact, wait } => {
            let (agent, id) = session(&r#ref)?;
            let from = match from {
                Some(from) => from,
                None => {
                    let remote = client::load().context("name the pushing machine with --from (this machine has not joined the hub, so it cannot look up who pushed the current copy)")?;
                    let history = remote
                        .history(agent.as_str(), &id)?
                        .with_context(|| format!("{agent}:{id} is not on the hub; name the machine to push it with --from"))?;
                    let pusher = history.manifest.machine.as_ref().context("the hub does not know which machine pushed that copy; pass --from")?;
                    if pusher.id == to || pusher.name == to {
                        bail!("{} pushed the hub's current copy, so --from defaults to it; name the machine that has the newer copy with --from.", pusher.name);
                    }
                    pusher.id.clone()
                }
            };
            let ctl = client()?;
            let plan = ctl.create_plan(&NewPlan {
                kind: "send".into(),
                agent: agent.as_str().into(),
                session: id,
                from,
                to,
                exact: exact.then_some(true),
            })?;
            if !json {
                println!("planned {}: push on {}, then pull on {} ({} keeps its copy). It runs as those machines' daemons ask (every few seconds).", plan.id, plan.steps[0].machine.name, plan.steps[1].machine.name, plan.steps[0].machine.name);
            }
            if wait { wait_for_plan(&ctl, &plan.id, json)? } else { show_plan(&plan, json)? }
        }
        ControlCommand::Jobs { limit } => {
            let jobs = client()?.list(limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&jobs)?);
                return Ok(());
            }
            if jobs.is_empty() {
                println!("No commands yet. `asm control push|pull <machine> <session>` asks a machine; `asm control send` moves a copy between two.");
            }
            // Newest first; a plan is shown once, at its newest step, with all its steps.
            let mut shown = std::collections::HashSet::new();
            for job in &jobs {
                match &job.plan {
                    None => println!("{}", line(job)),
                    Some(plan_id) if shown.insert(plan_id.clone()) => {
                        let steps = jobs.iter().filter(|j| j.plan.as_ref() == Some(plan_id)).cloned().collect();
                        let plan = Plan::new(plan_id, steps);
                        println!("{}", plan_head(&plan));
                        for step in &plan.steps {
                            println!("{}", step_line(step));
                        }
                    }
                    Some(_) => {}
                }
            }
        }
        ControlCommand::Show { id } => {
            let ctl = client()?;
            match plan_named(&ctl, &id) {
                Some(plan) => show_plan(&plan, json)?,
                None => show(&ctl.get(&id)?, json)?,
            }
        }
        ControlCommand::Cancel { id } => {
            let ctl = client()?;
            match plan_named(&ctl, &id) {
                Some(_) => show_plan(&ctl.cancel_plan(&id)?, json)?,
                None => show(&ctl.cancel(&id)?, json)?,
            }
        }
        ControlCommand::Retry { id } => {
            let ctl = client()?;
            match plan_named(&ctl, &id) {
                Some(_) => show_plan(&ctl.retry_plan(&id)?, json)?,
                None => show(&ctl.retry(&id)?, json)?,
            }
        }
    }
    Ok(())
}

fn queued(job: &Job, json: bool, wait: bool, ctl: &Control) -> anyhow::Result<()> {
    if !json {
        println!("queued {} on {}: it runs when that machine's daemon next asks (every few seconds)", job.id, job.machine.name);
    }
    if wait { wait_for(ctl, &job.id, json) } else { show(job, json) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(n: u32, op: &str, state: &str, more: serde_json::Value) -> Job {
        let mut v = serde_json::json!({
            "id": format!("c{n}"), "op": op, "agent": "claude-code", "session": "s1", "plan": "p1", "step": n,
            "machine": { "id": format!("m{n}"), "name": if n == 1 { "laptop" } else { "server" } },
            "state": state, "attempts": 1, "created": "2026-01-01T00:00:00Z", "updated": "2026-01-01T00:00:00Z",
            "expires": "2026-01-08T00:00:00Z", "created_by": "t",
        });
        v.as_object_mut().unwrap().extend(more.as_object().unwrap().clone());
        serde_json::from_value(v).unwrap()
    }
    fn plan(a: Job, b: Job) -> Plan {
        Plan::new("p1", vec![a, b])
    }

    #[test]
    fn a_diverged_pull_never_suggests_a_force_push() {
        assert!(hint(Code::Diverged, Op::Push).unwrap().contains("--force"));
        let pull = hint(Code::Diverged, Op::Pull).unwrap();
        assert!(!pull.contains("force") && pull.contains("Decide which to keep"));
        assert!(hint(Code::Ok, Op::Push).is_none());
        for code in [Code::NoDir, Code::Live, Code::HubNewer] {
            assert!(hint(code, Op::Pull).is_some());
        }
    }

    #[test]
    fn a_stopped_plan_names_the_step_and_the_way_on() {
        let blocked = plan(step(1, "push", "ok", serde_json::json!({})), step(2, "pull", "blocked", serde_json::json!({ "code": "diverged" })));
        let line = stopped(&blocked).unwrap();
        assert!(line.starts_with("step 2 stopped; earlier steps stay done. `asm control retry p1` queues step 2 again."), "{line}");
        assert!(line.ends_with(hint(Code::Diverged, Op::Pull).unwrap()));
        // Cancelled while the push ran: the skipped step is where it stopped.
        let cut = plan(step(1, "push", "ok", serde_json::json!({})), step(2, "pull", "cancelled", serde_json::json!({ "skipped": true, "code": "cancelled" })));
        assert!(stopped(&cut).unwrap().starts_with("step 2 stopped"));
        // A failed push comes before its skipped pull.
        let first = plan(step(1, "push", "blocked", serde_json::json!({ "code": "live" })), step(2, "pull", "cancelled", serde_json::json!({ "skipped": true })));
        assert!(stopped(&first).unwrap().contains("The session is running on that machine"));
        assert!(stopped(&plan(step(1, "push", "ok", serde_json::json!({})), step(2, "pull", "ok", serde_json::json!({})))).is_none());
        assert!(stopped(&plan(step(1, "push", "ok", serde_json::json!({})), step(2, "pull", "queued", serde_json::json!({})))).is_none());
    }

    #[test]
    fn a_wait_that_runs_out_names_the_open_step() {
        let jobs = [step(1, "push", "ok", serde_json::json!({})), step(2, "pull", "queued", serde_json::json!({}))];
        let said = still_waiting(&jobs, "p1");
        assert!(said.contains("step 2 (pull on server) is still queued"), "{said}");
        assert!(said.contains("until server's daemon asks") && said.contains("expires after 7 days"));
        let running = [step(1, "push", "running", serde_json::json!({})), step(2, "pull", "pending", serde_json::json!({}))];
        assert!(still_waiting(&running, "p1").contains("step 1 (push on laptop) is still running"));
    }
}
