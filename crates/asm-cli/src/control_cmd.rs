//! `asm control`: ask other machines to push or pull a session through the
//! hub, and decide whether this one may be asked.
//!
//! Two sides, one namespace. `enable` / `disable` / `status` are about this
//! machine (its daemon does what the hub asks only after `enable`). The rest
//! create and follow commands, with a commands token — this machine need not
//! be the one asked, or even have an agent.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use clap::Subcommand;

use asm_core::hub::client::{self, Control};
use asm_core::hub::commands::{Command as Job, NewCommand, Op, State};
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
        /// Wait for the result.
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
        /// Wait for the result.
        #[arg(long)]
        wait: bool,
    },
    /// Recent commands, newest first.
    Jobs {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// One command, in full.
    Show { id: String },
    /// Stop a command that has not run (one already running may finish).
    Cancel { id: String },
    /// Queue a blocked, expired or cancelled command again.
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
    if job.cancel_requested && job.state == State::Running {
        println!("  cancel requested; it may still finish");
    }
    if job.attempts > 1 {
        println!("  attempts: {}", job.attempts);
    }
    Ok(())
}

/// Poll until the command ends (or two minutes pass), then say how it went.
fn wait_for(ctl: &Control, id: &str, json: bool) -> anyhow::Result<()> {
    let deadline = now() + 120;
    let mut job = ctl.get(id)?;
    while !job.state.is_final() && now() < deadline {
        std::thread::sleep(Duration::from_secs(2));
        job = ctl.get(id)?;
    }
    show(&job, json)?;
    match job.state {
        State::Ok => Ok(()),
        s if s.is_final() => std::process::exit(1),
        _ => {
            eprintln!("still waiting: the machine asks every few seconds while its daemon runs. `asm control show {id}` follows it.");
            std::process::exit(2)
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
        ControlCommand::Jobs { limit } => {
            let jobs = client()?.list(limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&jobs)?);
                return Ok(());
            }
            if jobs.is_empty() {
                println!("No commands yet. `asm control push|pull <machine> <session>` asks a machine.");
            }
            for job in &jobs {
                println!("{}", line(job));
            }
        }
        ControlCommand::Show { id } => show(&client()?.get(&id)?, json)?,
        ControlCommand::Cancel { id } => show(&client()?.cancel(&id)?, json)?,
        ControlCommand::Retry { id } => show(&client()?.retry(&id)?, json)?,
    }
    Ok(())
}

fn queued(job: &Job, json: bool, wait: bool, ctl: &Control) -> anyhow::Result<()> {
    if !json {
        println!("queued {} on {}: it runs when that machine's daemon next asks (every few seconds)", job.id, job.machine.name);
    }
    if wait { wait_for(ctl, &job.id, json) } else { show(job, json) }
}
