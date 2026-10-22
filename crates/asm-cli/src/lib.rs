//! Command-line frontend: clap definitions and handlers over `asm_core::ops`.

/// `return Err(..)` with a typed error: `fail!(Usage, "say {what}")`. The
/// code is what `--json` reports and what the exit status says; see
/// `asm_core::contract`.
macro_rules! fail {
    ($code:ident, $($arg:tt)*) => {
        return Err($crate::coded(asm_core::contract::ErrorCode::$code, format!($($arg)*)))
    };
}

mod control_cmd;
mod daemon_ctl;
mod format;
mod update;

use std::path::PathBuf;

use anyhow::Context;
use clap::{CommandFactory, Parser, Subcommand};

use asm_core::adapter::SessionFilter;
use asm_core::contract::{self, CodedError, ErrorCode};
use asm_core::live::LiveEvent;
use asm_core::model::{AgentKind, Session};
use asm_core::ops::{self, ListQuery};

pub(crate) fn coded(code: ErrorCode, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(CodedError::new(code, message))
}

/// Whether failures are printed as a JSON object. Set once the arguments
/// are parsed (or found to be unparseable under `--json`).
static JSON_ERRORS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Say why the command failed and give the exit status for it. With `--json`
/// that is one `{"error": {...}}` object on stderr and nothing else; without
/// it, the text anyhow always printed. The status is the error's code in
/// `asm_core::contract::ErrorCode::exit_code`.
pub fn report_error(error: &anyhow::Error) -> i32 {
    let mut body = contract::classify(error.as_ref());
    if JSON_ERRORS.load(std::sync::atomic::Ordering::Relaxed) {
        // The whole chain, so a command's context is not lost; a cause the
        // message already says (an I/O error quotes its own source) is not
        // said twice.
        let mut message = error.to_string();
        for cause in error.chain().skip(1) {
            let cause = cause.to_string();
            if !message.contains(&cause) {
                message = format!("{message}: {cause}");
            }
        }
        body.message = message;
        eprintln!("{}", body.envelope());
    } else {
        eprintln!("Error: {error:?}");
    }
    body.code.exit_code()
}

/// Say a failure that ends the process by some other status than the error's
/// own (`control --wait` keeps its exit codes): the JSON object under
/// `--json`, the plain text otherwise.
pub(crate) fn say_failure(code: ErrorCode, message: String) {
    if JSON_ERRORS.load(std::sync::atomic::Ordering::Relaxed) {
        eprintln!("{}", contract::ErrorBody::new(code, message).envelope());
    } else {
        eprintln!("{message}");
    }
}

/// `ASM_JSON=1` (or true, yes, on) is `--json`.
fn json_from_env() -> bool {
    std::env::var("ASM_JSON").is_ok_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

/// A command line clap rejected. Help and version are clap's to print; a
/// real mistake under `--json` becomes a `usage` error like any other,
/// and otherwise clap prints and exits (2) as it always did.
fn parse_failure(e: clap::Error) -> anyhow::Error {
    let wants_json = std::env::args_os().any(|a| a == "--json") || json_from_env();
    if !e.use_stderr() || !wants_json {
        e.exit();
    }
    JSON_ERRORS.store(true, std::sync::atomic::Ordering::Relaxed);
    // What clap says before it gets to `Usage:` and the hints.
    let rendered = e.render().to_string();
    let said: Vec<&str> = rendered.lines().take_while(|l| !l.starts_with("Usage:")).map(str::trim).filter(|l| !l.is_empty()).collect();
    coded(ErrorCode::Usage, said.join(" ").trim_start_matches("error: "))
}

#[derive(Parser)]
#[command(name = "asm", version, about = "Cross-agent session manager")]
pub struct Cli {
    /// Emit machine-readable JSON instead of tables. A failure is then one
    /// `{"error": {"code", "message"}}` object on stderr. `ASM_JSON=1` is the
    /// same flag.
    #[arg(long, global = true)]
    json: bool,

    /// Restrict to one agent: claude-code, opencode, jcode, codex or antigravity.
    #[arg(long, global = true)]
    agent: Option<String>,

    /// Restrict to sessions of this project directory.
    #[arg(long, global = true)]
    project: Option<PathBuf>,

    /// Include subagent/child sessions (hidden by default). `list` shows them
    /// nested under their parent.
    #[arg(long = "include-children", global = true)]
    include_children: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List sessions across all agents (default command).
    ///
    /// Subagent sessions are folded behind their parent: a session that has
    /// some ends its row with `+N` in the status column. With
    /// `--include-children` they are listed under it, indented, with the
    /// parent's id in a PARENT column. `--json` is an array of sessions in
    /// listing order, each with its `parent` (null for a top-level session).
    List(ListOpts),
    /// The sessions under a session (subagents, spawned threads), at any
    /// depth, in tree order. `--json` is a flat array of sessions, each with
    /// its `depth` (1 for a direct child) and `parent`.
    Children {
        /// Session to act on: a native id, a unique id prefix, or `agent:id-prefix`.
        r#ref: String,
        #[command(flatten)]
        select: Select,
    },
    /// A session's messages, one JSON object per line: `index`, `role`,
    /// `ts`, `parts` (the IR's parts). Read through the same export `asm
    /// export` uses, so every agent that exports has them.
    Messages {
        /// Session to act on: a native id, a unique id prefix, or `agent:id-prefix`.
        r#ref: String,
        /// Only this role. `tool` is a message holding a tool call or result.
        #[arg(long, value_parser = ops::MessageQuery::ROLES)]
        role: Option<String>,
        /// Only the last N messages (after the other filters).
        #[arg(long, value_name = "N")]
        tail: Option<usize>,
        /// Only messages whose index is in A..B (end excluded; `A..` and
        /// `..B` are open).
        #[arg(long, value_name = "A..B")]
        range: Option<String>,
        /// Keep only text: drop tool calls, reasoning and files, and the
        /// messages that have no text left.
        #[arg(long)]
        text: bool,
        /// ndjson: one message per line. json: one array.
        #[arg(long, default_value = "ndjson", value_parser = ["ndjson", "json"])]
        format: String,
    },
    /// Follow the sessions on disk: a JSON line `{"event":"ready"}`, then
    /// `{"event":"added|changed|removed","session":{...}}` as they change.
    /// Polls; runs until stopped.
    Events {
        /// Only sessions in this state.
        #[arg(long, value_parser = ["live", "idle", "archived"])]
        status: Option<String>,
        /// Seconds between looks at the stores (at least 0.5).
        #[arg(long, default_value = "2", value_parser = parse_interval)]
        interval: f64,
    },
    /// What this asm and each agent's store can do, as JSON a program can
    /// plan from: versions, contract version, per-agent capabilities.
    Capabilities,
    /// List projects with session counts. A project is a git repository —
    /// every worktree of it, plus sessions started in subdirectories — not
    /// a single directory.
    Projects {
        /// Also list each repository's worktrees.
        #[arg(long)]
        worktrees: bool,
    },
    /// Show one session's metadata. REF is a native id, a unique id prefix,
    /// or `agent:id-prefix`.
    Show {
        /// Session to act on: a native id, a unique id prefix, or `agent:id-prefix`.
        r#ref: String,
    },
    /// Resume a session interactively in its native agent.
    Resume {
        /// Session to act on: a native id, a unique id prefix, or `agent:id-prefix`.
        r#ref: String,
    },
    /// Set a session's title (uses the agent's native mechanism).
    Rename {
        /// Session to act on: a native id, a unique id prefix, or `agent:id-prefix`.
        r#ref: String,
        /// The new title.
        title: String,
    },
    /// Move a session to another project directory / worktree.
    Move {
        /// Session to act on: a native id, a unique id prefix, or `agent:id-prefix`.
        r#ref: String,
        /// Directory (or worktree) to move the session to.
        new_dir: PathBuf,
    },
    /// Archive a session (OpenCode: native flag; Claude: moved into asm's
    /// archive store).
    Archive {
        /// Session to act on: a native id, a unique id prefix, or `agent:id-prefix`.
        r#ref: String,
    },
    /// Restore an archived session.
    Unarchive {
        /// Session to act on: a native id, a unique id prefix, or `agent:id-prefix`.
        r#ref: String,
    },
    /// Delete a session and all its sidecars (backed up first).
    ///
    /// OpenCode deletes a session's whole tree, so the sessions under it go
    /// with it; the prompt names them, and the backup holds them all. jcode
    /// deletes the one session, and its children stay as top-level sessions.
    /// `--yes` skips the prompt.
    Delete {
        /// Session to act on: a native id, a unique id prefix, or `agent:id-prefix`.
        r#ref: String,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Send a message into an existing session and stream the reply.
    Send {
        /// Session to act on: a native id, a unique id prefix, or `agent:id-prefix`.
        r#ref: String,
        /// The message. Use `-` to read it from stdin.
        message: String,
    },
    /// Import a session into another agent (the flagship).
    Import {
        /// Session to act on: a native id, a unique id prefix, or `agent:id-prefix`.
        r#ref: String,
        /// Target agent (claude-code | opencode).
        #[arg(long)]
        to: String,
        /// Target project directory (defaults to the session's own project).
        #[arg(long)]
        project_dir: Option<PathBuf>,
        /// full: translate the whole transcript so it resumes in place.
        /// seed: condense into a handoff document seeding a fresh session.
        #[arg(long, default_value = "full")]
        mode: String,
        /// Generate the target document and loss report without writing.
        #[arg(long)]
        dry_run: bool,
        /// Show the tool-name mapping table for the target and exit.
        #[arg(long)]
        show_toolmap: bool,
    },
    /// Export a session as Session IR (versioned JSON).
    Export {
        /// Session to act on: a native id, a unique id prefix, or `agent:id-prefix`.
        r#ref: String,
        /// Write to a file instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// List git worktrees of a repo and the sessions living in each.
    Worktrees {
        /// Repository path (defaults to the current directory).
        repo: Option<PathBuf>,
    },
    /// Full-text search across every session's conversation.
    Search {
        /// Words (AND-ed), or a "quoted phrase".
        query: Vec<String>,
        /// Maximum number of results.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Search the existing index without bringing it up to date.
        #[arg(long)]
        no_refresh: bool,
        #[command(flatten)]
        select: Select,
    },
    /// Rebuild the search index and report on it.
    Index {
        /// Only report; do not refresh.
        #[arg(long)]
        stats: bool,
    },
    /// Check agent stores for problems (duplicate ids, stale locks, ...).
    Doctor,
    /// Version the archive store with git (transport stays yours).
    #[command(subcommand)]
    Sync(SyncCommand),
    /// Replace this binary with the latest release.
    Update {
        /// Report what is available and exit without changing anything.
        #[arg(long)]
        check: bool,
        /// Install even when this is already the latest version.
        #[arg(long)]
        force: bool,
        /// Install a specific tag instead of the latest release.
        #[arg(long)]
        version: Option<String>,
    },
    /// Run or manage a hub that machines sync sessions through.
    #[command(subcommand)]
    Hub(HubCommand),
    /// Join this machine to a hub, with the token `asm hub serve` printed.
    /// The token is read from $ASM_JOIN_TOKEN, which keeps it out of the
    /// process list.
    Join {
        /// The hub's URL, e.g. https://hub.example.ts.net
        url: String,
        /// `-` reads the token from stdin. A token given here is visible to
        /// every user of this machine while asm runs.
        #[arg(long)]
        token: Option<String>,
        /// How this machine is named to the others (default: its hostname).
        #[arg(long)]
        name: Option<String>,
        /// Allow plain HTTP to an address outside loopback, a private LAN
        /// or a VPN. Every transcript would cross the network unencrypted.
        #[arg(long)]
        insecure_http: bool,
    },
    /// Upload sessions to the hub. A session is sent only if it changed.
    Push {
        /// Sessions to push (ids, prefixes, `agent:prefix`).
        refs: Vec<String>,
        /// Push every session on this machine that the filters match
        /// (--agent, --project, --include-children).
        #[arg(long)]
        all: bool,
        /// Make this machine's copy the hub's head even when another
        /// machine's newer copy is there. The replaced copy stays on the
        /// hub as a revision.
        #[arg(long)]
        force: bool,
        /// Archive each session here once the hub has it (`asm unarchive`
        /// brings it back), to continue it on another machine with `asm pull`.
        #[arg(long = "move")]
        move_away: bool,
        /// Send one session straight to this peer (`asm peer add`) instead
        /// of the hub. It is installed there like a pull, and refused if both
        /// copies changed. Not with --all, --force or --move.
        #[arg(long, value_name = "PEER")]
        to: Option<String>,
        /// With --to: where to put the session on the peer, when this
        /// machine's path does not exist there.
        #[arg(long, requires = "to")]
        project_dir: Option<PathBuf>,
    },
    /// Keep pushing this machine's sessions to the hub, each once it has been
    /// still for an interval. Pulls only what `asm control enable` lets the hub
    /// ask for. Runs until stopped.
    #[command(args_conflicts_with_subcommands = true)]
    Daemon {
        #[command(subcommand)]
        action: Option<DaemonCommand>,
        #[command(flatten)]
        opts: DaemonOpts,
    },
    /// Bring a session from the hub onto this machine.
    Pull {
        /// The session on the hub (id, prefix, `agent:prefix`).
        #[arg(conflicts_with = "all")]
        r#ref: Option<String>,
        /// Pull every session on the hub that the filters match
        /// (--agent, --project), each at its own place on this machine.
        #[arg(long, conflicts_with = "project_dir")]
        all: bool,
        /// Where to put it, when the pushing machine's path does not exist
        /// here.
        #[arg(long)]
        project_dir: Option<PathBuf>,
        /// Take the session from this peer (`asm peer add`) instead of the
        /// hub. Not with --all.
        #[arg(long, value_name = "PEER")]
        from: Option<String>,
    },
    /// See what is on the hub, beside what is here.
    #[command(subcommand)]
    Remote(RemoteCommand),
    /// Other machines to move a session to or from directly, with no hub
    /// between: over ssh, or an `asm hub serve --peer`.
    #[command(subcommand)]
    Peer(PeerCommand),
    /// Ask other machines to push or pull a session through the hub, and let
    /// this one be asked (off until `asm control enable`).
    #[command(subcommand)]
    Control(control_cmd::ControlCommand),
    /// Open the interactive TUI browser.
    Tui,
    /// Serve the web UI (loopback by default).
    Serve {
        /// Port to listen on.
        #[arg(long, default_value_t = 7433)]
        port: u16,
        /// Address to bind. An IP or hostname; `0.0.0.0` / `::` bind every
        /// interface. Anything outside loopback exposes an unauthenticated
        /// API to your network.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Require `Authorization: Bearer <token>` on every `/api/` request,
        /// the token being this file's contents (or $ASM_WEB_TOKEN). The
        /// bundled UI sends none: this is for programs calling the API.
        #[arg(long, value_name = "PATH")]
        token_file: Option<PathBuf>,
    },
}

#[derive(clap::Args, Clone, Copy)]
struct DaemonOpts {
    /// Seconds between passes; a changed session goes up after one quiet
    /// pass.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
    interval: u64,
    /// Push only sessions that are live or changed in the last N minutes
    /// while the daemon runs: the ones being worked in, not every older
    /// session that differs from the hub. Default: everything that
    /// differs.
    #[arg(long, value_name = "MINUTES", value_parser = clap::value_parser!(u64).range(1..))]
    active_within: Option<u64>,
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Run the daemon in the background, logging to the data directory.
    Start(#[command(flatten)] DaemonOpts),
    /// Stop the background daemon.
    Stop,
    /// Install it as a user service (systemd, or launchd on macOS) so it
    /// starts at login and restarts if it dies.
    Install {
        #[command(flatten)]
        opts: DaemonOpts,
        /// Write the unit but do not enable or start it.
        #[arg(long)]
        no_start: bool,
    },
    /// Remove the user service.
    Uninstall,
    /// Whether a daemon is running here, when it last pushed, and what it
    /// is waiting on or could not push.
    Status,
}

impl From<DaemonOpts> for daemon_ctl::Opts {
    fn from(o: DaemonOpts) -> Self {
        Self { interval: o.interval, active_within: o.active_within }
    }
}

#[derive(Subcommand)]
enum SyncCommand {
    /// Make the archive a git repository (optionally with a remote).
    Init {
        /// Git remote URL to use as `origin`.
        #[arg(long)]
        remote: Option<String>,
    },
    /// Show what is archived and the repository's state.
    Status,
    /// Commit the current archive contents locally.
    Commit {
        /// Commit message (default: "asm archive snapshot <timestamp>").
        #[arg(short, long)]
        message: Option<String>,
    },
}

#[derive(Subcommand)]
enum HubCommand {
    /// Serve a hub from this machine (loopback by default).
    Serve {
        /// Port to listen on.
        #[arg(long, default_value_t = 7434)]
        port: u16,
        /// Address to bind. Plain HTTP: bind a network you trust, or keep
        /// loopback and put HTTPS in front of it.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Largest single file the hub accepts, in MiB.
        #[arg(long, default_value_t = 4096)]
        max_file_mb: u64,
        /// Also be a peer: let joined machines `asm push --to` sessions
        /// straight into this machine's own agent stores, and `asm pull
        /// --from` them. Off by default, because a hub is an archive and
        /// should not write into its host's agents unless told to.
        #[arg(long)]
        peer: bool,
    },
    /// The receiving end of `asm push --to` over ssh: a bundle on stdin,
    /// installed here, the outcome as JSON on stdout.
    #[command(hide = true)]
    Receive {
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// The sending end of `asm pull --from` over ssh: the session as a
    /// bundle on stdout.
    #[command(hide = true)]
    Send {
        r#ref: String,
    },
    /// Print the command that joins another machine to this hub.
    Token {
        /// Replace the token; the old one stops working. Joined machines
        /// keep their access.
        #[arg(long)]
        rotate: bool,
    },
    /// Mint the token for the hub's admin page and API (`/admin`), replacing
    /// any earlier one. It is shown once: only its hash is kept. Until one
    /// exists, the hub serves no admin page or API at all.
    AdminToken,
    /// Mint the token that creates remote-control commands (`asm control push
    /// ...`), replacing any earlier one. Shown once. It opens that API and
    /// nothing else: not the admin page, not any machine's sessions.
    CommandsToken,
    /// Machines that have joined this hub.
    Machines,
    /// Remove a machine's access, by id or unique name.
    Revoke {
        /// Machine id or unique name.
        machine: String,
    },
}

#[derive(Subcommand)]
enum RemoteCommand {
    /// Sessions here and on the hub, grouped by project.
    List,
    /// The machines on the hub this one joined.
    Machines,
}

#[derive(Subcommand)]
enum PeerCommand {
    /// Remember a peer. `ssh://[user@]host` runs asm there over ssh (keys
    /// loaded, host known: nothing is prompted). An http(s):// URL is an
    /// `asm hub serve --peer`, joined with the token `asm hub token` prints
    /// there, read from $ASM_JOIN_TOKEN.
    Add {
        /// What to call it in `--to` and `--from`.
        name: String,
        /// ssh://[user@]host, or http(s)://host:port.
        address: String,
        /// `-` reads the join token from stdin. A token given here is
        /// visible to every user of this machine while asm runs.
        #[arg(long)]
        token: Option<String>,
        /// How this machine is named to an HTTP peer (default: its hostname).
        #[arg(long = "as", value_name = "MACHINE")]
        machine: Option<String>,
        /// Allow plain HTTP to an address outside loopback, a private LAN
        /// or a VPN. Every transcript would cross the network unencrypted.
        #[arg(long)]
        insecure_http: bool,
    },
    /// Forget a peer. An HTTP peer keeps the credential it issued until
    /// `asm hub revoke` there.
    Remove {
        /// The name given to `asm peer add`.
        name: String,
    },
    /// The peers this machine knows.
    List,
    /// The sessions on a peer, as `asm list` there would show them
    /// (--agent narrows them).
    Sessions {
        /// The name given to `asm peer add`.
        peer: String,
    },
}

/// The command tree, for tests that must not drift from it.
pub fn command() -> clap::Command {
    Cli::command()
}

/// What to do with a listing beyond showing it. Shared by every command that
/// lists sessions or hits.
#[derive(clap::Args, Clone, Default)]
struct Select {
    /// Only sessions in this state.
    #[arg(long, value_parser = ["live", "idle", "archived"])]
    status: Option<String>,
    /// Print only session ids, one per line (`--json` does not apply).
    #[arg(long)]
    ids_only: bool,
    /// `--ids-only`, with each id ended by NUL instead of a newline, for
    /// `xargs -0`.
    #[arg(short = '0', long)]
    null: bool,
}

#[derive(clap::Args, Clone, Default)]
struct ListOpts {
    #[command(flatten)]
    select: Select,
    /// Keep running and print each change as JSON lines instead of the
    /// listing: the same as `asm events`.
    #[arg(long)]
    follow: bool,
    /// With --follow: seconds between looks at the stores (at least 0.5).
    #[arg(long, default_value = "2", value_parser = parse_interval, requires = "follow")]
    interval: f64,
}

fn parse_interval(text: &str) -> Result<f64, String> {
    let secs: f64 = text.parse().map_err(|_| format!("'{text}' is not a number of seconds"))?;
    if secs.is_finite() && secs >= 0.5 { Ok(secs) } else { Err("the interval is at least 0.5 seconds".into()) }
}

/// A frontend the binary should launch after argument parsing (the CLI
/// crate stays free of TUI/web dependencies).
pub enum Frontend {
    Tui,
    Serve { host: String, port: u16, token: Option<String> },
    Hub { host: String, port: u16, max_file_bytes: u64, peer: bool },
}

pub fn run() -> anyhow::Result<Option<Frontend>> {
    let mut cli = Cli::try_parse().map_err(parse_failure)?;
    cli.json |= json_from_env();
    JSON_ERRORS.store(cli.json, std::sync::atomic::Ordering::Relaxed);
    let filter = build_filter(&cli)?;

    // Bare `asm` on a terminal opens the interactive browser; piped or
    // scripted invocations get the plain listing.
    let default_command = if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        Command::Tui
    } else {
        Command::List(ListOpts::default())
    };

    match cli.command.unwrap_or(default_command) {
        Command::Tui => return Ok(Some(Frontend::Tui)),
        Command::Serve { port, host, token_file } => {
            let token = match token_file {
                Some(path) => Some(
                    std::fs::read_to_string(&path)
                        .map_err(|e| anyhow::Error::new(asm_core::CoreError::io(&path, e)))?
                        .trim()
                        .to_string(),
                ),
                None => std::env::var("ASM_WEB_TOKEN").ok(),
            };
            if token.as_deref() == Some("") {
                fail!(Usage, "the web token is empty; give --token-file a file with the token in it");
            }
            return Ok(Some(Frontend::Serve { host, port, token }));
        }
        Command::Hub(HubCommand::Serve { port, host, max_file_mb, peer }) => {
            let max_file_bytes = max_file_mb.saturating_mul(1024 * 1024);
            return Ok(Some(Frontend::Hub { host, port, max_file_bytes, peer }));
        }
        // The ssh ends of a peer transfer run on machines that may never
        // have been a hub: dispatched before `hub()` opens a store here.
        Command::Hub(HubCommand::Receive { project_dir }) => hub_receive(project_dir.as_deref()),
        Command::Hub(HubCommand::Send { r#ref }) => hub_send(&r#ref),
        Command::Hub(command) => hub(command, cli.json),
        Command::Join { url, token, name, insecure_http } => {
            join(&url, token.as_deref(), name.as_deref(), insecure_http, cli.json)
        }
        Command::Push { refs, all, force, move_away, to: Some(peer), project_dir } => {
            if all || force || move_away {
                fail!(Usage, "--to sends one session to a peer as it is; --all, --force and --move are for the hub");
            }
            push_to(&peer, &refs, project_dir.as_deref(), &filter, cli.json)
        }
        Command::Push { refs, all, force, move_away, to: None, .. } => {
            push(&refs, all, force, move_away, &filter, cli.json)
        }
        Command::Daemon { action: Some(action), .. } => match action {
            DaemonCommand::Status => daemon_status(cli.json),
            DaemonCommand::Start(o) => daemon_ctl::start(o.into(), cli.json),
            DaemonCommand::Stop => daemon_ctl::stop(cli.json),
            DaemonCommand::Install { opts, no_start } => daemon_ctl::install(opts.into(), !no_start, cli.json),
            DaemonCommand::Uninstall => daemon_ctl::uninstall(cli.json),
        },
        Command::Daemon { action: None, opts: DaemonOpts { interval, active_within } } => {
            let remote = asm_core::hub::client::load()?;
            eprintln!(
                "Pushing to {} as {} every {interval}s{} (ctrl-c to stop).",
                remote.url,
                remote.machine.name,
                active_within.map_or(String::new(), |m| format!(", only sessions active in the last {m}m"))
            );
            Ok(asm_core::hub::daemon::run(&remote, &filter, std::time::Duration::from_secs(interval), active_within.map(|m| std::time::Duration::from_secs(m * 60)), |line| {
                eprintln!("{line}")
            })?)
        }
        Command::Pull { r#ref, all, project_dir, from: Some(peer) } => {
            if all {
                fail!(Usage, "--from takes one session from a peer; --all is for the hub");
            }
            let Some(query) = r#ref else { fail!(Usage, "name the session to pull from {peer}") };
            pull_from(&peer, &query, project_dir.as_deref(), cli.json)
        }
        Command::Pull { r#ref, all, project_dir, from: None } => {
            pull(r#ref.as_deref(), all, project_dir.as_deref(), &filter, cli.json)
        }
        Command::Remote(command) => remote(command, &filter, cli.json),
        Command::Peer(command) => peer(command, &filter, cli.json),
        Command::Control(command) => control_cmd::run(command, cli.json),
        Command::List(opts) => list(&filter, &opts, cli.json),
        Command::Children { r#ref, select } => children(&r#ref, &filter, &select, cli.json),
        Command::Messages { r#ref, role, tail, range, text, format } => {
            messages(&r#ref, &filter, role, tail, range.as_deref(), text, &format)
        }
        Command::Events { status, interval } => follow(&filter, status, interval),
        Command::Capabilities => capabilities(cli.json),
        Command::Projects { worktrees } => projects(cli.json, worktrees),
        Command::Show { r#ref } => show(&r#ref, &filter, cli.json),
        Command::Resume { r#ref } => resume(&r#ref, &filter),
        Command::Rename { r#ref, title } => rename(&r#ref, &title, &filter, cli.json),
        Command::Move { r#ref, new_dir } => move_session(&r#ref, &new_dir, &filter, cli.json),
        Command::Archive { r#ref } => archive(&r#ref, &filter, cli.json),
        Command::Unarchive { r#ref } => unarchive(&r#ref, &filter, cli.json),
        Command::Delete { r#ref, yes } => delete(&r#ref, yes, &filter, cli.json),
        Command::Send { r#ref, message } => send(&r#ref, &message, &filter, cli.json),
        Command::Import { r#ref, to, project_dir, mode, dry_run, show_toolmap } => import(
            &r#ref,
            &to,
            project_dir.as_deref(),
            &mode,
            dry_run,
            show_toolmap,
            &filter,
            cli.json,
        ),
        Command::Export { r#ref, output } => export(&r#ref, output.as_deref(), &filter, cli.json),
        Command::Worktrees { repo } => worktrees(repo.as_deref(), cli.json),
        Command::Search { query, limit, no_refresh, select } => {
            search(&query.join(" "), limit, !no_refresh, &filter, &select, cli.json)
        }
        Command::Index { stats } => index(stats, &filter, cli.json),
        Command::Doctor => doctor(cli.json),
        Command::Update { check, force, version } => update::run(check, force, version, cli.json),
        Command::Sync(command) => sync(command, cli.json),
    }?;
    Ok(None)
}

/// Redrawing progress on one line only makes sense on a terminal; piped
/// output would otherwise collect escape sequences.
fn progress_reporter() -> impl FnMut(asm_core::index::RefreshProgress) {
    let tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
    move |p: asm_core::index::RefreshProgress| {
        if !tty {
            return;
        }
        match p.note {
            // The count is what a long wait needs; the name says it is
            // making progress through real sessions.
            Some(note) => eprint!("\r\x1b[2Kindexing {}/{}  {note}", p.done, p.total),
            None if p.total > 0 => eprint!("\r\x1b[2Kindexing {} sessions…", p.total),
            None => {}
        }
    }
}

fn clear_progress() {
    if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        eprint!("\r\x1b[2K");
    }
}

fn search(
    query: &str,
    limit: usize,
    refresh: bool,
    filter: &SessionFilter,
    select: &Select,
    json: bool,
) -> anyhow::Result<()> {
    use asm_core::index::{Index, SearchQuery};

    if query.trim().is_empty() {
        fail!(Usage, "nothing to search for");
    }
    let mut index = Index::open()?;
    if refresh {
        // Cheap unless something actually changed: only sessions whose
        // fingerprint moved are re-extracted.
        let report = index.refresh(progress_reporter())?;
        clear_progress();
        for failure in &report.failed {
            eprintln!("warning: {failure}");
        }
    }

    let hits = index.search(&SearchQuery {
        text: query.to_string(),
        agent: filter.agent,
        project: filter.project.clone(),
        limit,
        state: select.status.clone(),
    })?;

    if select.ids_only || select.null {
        // A session with several hits is named once.
        let mut seen = std::collections::HashSet::new();
        return print_ids(hits.iter().filter(|h| seen.insert((&h.agent, &h.native_id))).map(|h| h.native_id.as_str()), select.null);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&hits)?);
        return Ok(());
    }
    if hits.is_empty() {
        println!("No matches for {query:?}.");
        return Ok(());
    }
    let short = |agent: &str, id: &str| -> String {
        id.chars().take(if agent == "opencode" { 12 } else { 8 }).collect()
    };
    for hit in &hits {
        let id = short(&hit.agent, &hit.native_id);
        let archived = if hit.status == "archived" { "  (archived)" } else { "" };
        // A hit inside a subagent says whose subagent it is.
        let parent = hit.parent.as_deref().map_or(String::new(), |p| format!("  ↳ in {}", short(&hit.agent, p)));
        println!(
            "{}  {}  {}{parent}{archived}",
            hit.agent,
            id,
            hit.title.as_deref().unwrap_or("(untitled)")
        );
        // Bold the matched terms on a terminal, brackets when piped.
        let (start, end) = if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            ("\x1b[1;33m", "\x1b[0m")
        } else {
            ("[", "]")
        };
        let snippet = hit
            .snippet
            .replace('\n', " ")
            .replace(asm_core::index::MATCH_START, start)
            .replace(asm_core::index::MATCH_END, end);
        println!("    {} #{}  {}", hit.role, hit.seq, snippet);
    }
    println!("\n{} match(es).", hits.len());
    Ok(())
}

fn index(stats_only: bool, filter: &SessionFilter, json: bool) -> anyhow::Result<()> {
    use asm_core::index::Index;

    let mut index = Index::open()?;
    let report =
        if stats_only { None } else { Some(index.refresh_where(filter, progress_reporter())?) };
    clear_progress();
    // Re-extracting sessions leaves free pages behind; this is the
    // maintenance path where reclaiming them is worth the extra moment.
    let reclaimed = match &report {
        Some(report) if report.reindexed > 0 => index.compact().unwrap_or(0),
        _ => 0,
    };
    let stats = index.stats()?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "refresh": report,
                "stats": stats,
            }))?
        );
        return Ok(());
    }
    if let Some(report) = &report {
        println!(
            "Scanned {} sessions: {} indexed, {} unchanged, {} removed ({} messages).",
            report.scanned,
            report.reindexed,
            report.unchanged,
            report.removed,
            report.messages_indexed
        );
        for failure in &report.failed {
            println!("  failed: {failure}");
        }
        if reclaimed > 0 {
            println!("Reclaimed {:.1} MB.", reclaimed as f64 / 1_048_576.0);
        }
    }
    println!(
        "Index at {} — {} sessions, {} messages, {:.1} MB.",
        stats.path.display(),
        stats.sessions,
        stats.messages,
        stats.bytes as f64 / 1_048_576.0
    );
    Ok(())
}

fn sync(command: SyncCommand, json: bool) -> anyhow::Result<()> {
    match command {
        SyncCommand::Init { remote } => {
            let outcome = asm_core::sync::init(remote.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&outcome)?);
                return Ok(());
            }
            if outcome.created_repo {
                println!("Initialized archive repository at {}.", outcome.archive_root.display());
            } else {
                println!("Archive repository already present at {}.", outcome.archive_root.display());
            }
            if let Some(remote) = &outcome.remote_set {
                println!("Remote origin set to {remote}.");
            }
        }
        SyncCommand::Status => {
            let status = asm_core::sync::status()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
                return Ok(());
            }
            println!("archive     {}", status.archive_root.display());
            println!(
                "sessions    {} ({:.1} MB)",
                status.sessions.len(),
                status.total_bytes as f64 / 1_048_576.0
            );
            if status.git_initialized {
                println!("branch      {}", status.branch.as_deref().unwrap_or("-"));
                println!("remote      {}", status.remote.as_deref().unwrap_or("(none)"));
                println!("last commit {}", status.last_commit.as_deref().unwrap_or("(none)"));
                println!("uncommitted {} path(s)", status.uncommitted);
            }
            for session in status.sessions.iter().take(20) {
                println!(
                    "  {} {} {}",
                    session.agent,
                    &session.id[..session.id.len().min(12)],
                    session.title.as_deref().unwrap_or("(untitled)")
                );
            }
            for note in &status.notes {
                println!("note: {note}");
            }
        }
        SyncCommand::Commit { message } => {
            let message = message
                .unwrap_or_else(|| format!("asm archive snapshot {}", jiff::Timestamp::now()));
            let outcome = asm_core::sync::commit(&message)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&outcome)?);
                return Ok(());
            }
            if outcome.committed {
                println!("Committed {} changed path(s).", outcome.files_changed);
            } else {
                println!("Nothing to commit.");
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn import(
    query: &str,
    to: &str,
    project_dir: Option<&std::path::Path>,
    mode: &str,
    dry_run: bool,
    show_toolmap: bool,
    filter: &SessionFilter,
    json: bool,
) -> anyhow::Result<()> {
    use asm_core::import::{ImportMode, ImportOpts, toolmap};

    let Some(target) = AgentKind::parse(to) else {
        fail!(Usage, "unknown target agent '{to}' (try: claude-code, opencode)");
    };

    if show_toolmap {
        let table = toolmap::table(target);
        if json {
            let pairs: Vec<_> = table.iter().map(|(from, to)| serde_json::json!({ "from": from, "to": to })).collect();
            return print_json(&serde_json::json!({ "target": target, "tools": pairs }));
        }
        for (from, to) in table {
            println!("{from} -> {to}");
        }
        return Ok(());
    }

    let mode = match mode {
        "full" => ImportMode::Full,
        "seed" => ImportMode::Seed,
        other => fail!(Usage, "unknown mode '{other}' (full | seed)"),
    };
    let session = resolve(query, filter)?;
    let opts = ImportOpts { mode, project: project_dir.map(|p| p.to_path_buf()), dry_run };
    let outcome = ops::import(&session, target, &opts)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&outcome)?);
        return Ok(());
    }
    for warning in &outcome.warnings {
        eprintln!("warning: {warning}");
    }
    if let Some(artifact) = &outcome.dry_run_artifact {
        println!("{artifact}");
        eprintln!("\n--- dry run: nothing was written ---");
    } else if outcome.in_sync {
        println!(
            "In sync: this session was already imported as {} (delete it to re-import).",
            outcome.target.as_ref().map(|t| t.native_id.as_str()).unwrap_or("?")
        );
        return Ok(());
    } else if let Some(target_ref) = &outcome.target {
        println!("Imported as {}:{}.", target_ref.agent, target_ref.native_id);
    }
    let summary = outcome.loss.human_summary();
    if !summary.is_empty() {
        eprintln!("\nLoss report:\n{summary}");
    }
    if let Some(hint) = &outcome.resume_hint {
        eprintln!("\nResume with: {hint}");
    }
    Ok(())
}

fn export(
    query: &str,
    output: Option<&std::path::Path>,
    filter: &SessionFilter,
    as_json: bool,
) -> anyhow::Result<()> {
    let session = resolve(query, filter)?;
    let ir = ops::export_ir(&session)?;
    let json = serde_json::to_string_pretty(&ir)?;
    match output {
        Some(path) => {
            std::fs::write(path, &json).map_err(|e| asm_core::CoreError::io(path, e))?;
            if as_json {
                // The IR went to the file; stdout says where.
                return print_json(&mutated(&session, serde_json::json!({ "path": path, "messages": ir.messages.len() })));
            }
            eprintln!(
                "Exported {} ({} messages) to {}.",
                session.short_id(),
                ir.messages.len(),
                path.display()
            );
        }
        None => println!("{json}"),
    }
    Ok(())
}

fn worktrees(repo: Option<&std::path::Path>, json: bool) -> anyhow::Result<()> {
    let repo = match repo {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir()?,
    };
    let worktrees = asm_core::git::worktrees(&repo)?;
    let sessions = ops::list_sessions(&SessionFilter::default())?;
    if json {
        // Each worktree as git reports it, plus the sessions living in it.
        let full: Vec<serde_json::Value> = worktrees
            .iter()
            .map(|wt| {
                let here: Vec<&Session> = sessions.iter().filter(|s| s.project_root == wt.path).collect();
                let mut value = serde_json::to_value(wt)?;
                value["session_count"] = here.len().into();
                value["sessions"] = serde_json::to_value(&here)?;
                Ok::<_, serde_json::Error>(value)
            })
            .collect::<Result<_, _>>()?;
        return print_json(&full);
    }
    for wt in &worktrees {
        let here: Vec<&asm_core::model::Session> =
            sessions.iter().filter(|s| s.project_root == wt.path).collect();
        let mut flags = Vec::new();
        if wt.is_main {
            flags.push("main");
        }
        if wt.detached {
            flags.push("detached");
        }
        if wt.locked {
            flags.push("locked");
        }
        if wt.prunable {
            flags.push("prunable");
        }
        let branch = wt.branch.as_deref().unwrap_or("-");
        let suffix = if flags.is_empty() { String::new() } else { format!(" [{}]", flags.join(",")) };
        println!("{}  ({branch}){suffix}  — {} session(s)", wt.path.display(), here.len());
        for s in here {
            println!(
                "    {} {} {}",
                s.handle.agent,
                s.short_id(),
                s.title.as_deref().unwrap_or("(untitled)")
            );
        }
    }
    Ok(())
}

/// The one session `query` names. The error stays typed (not found, or
/// ambiguous with its candidates) so the top level can report which.
fn resolve(query: &str, filter: &SessionFilter) -> anyhow::Result<Session> {
    Ok(ops::resolve_ref(query, filter)?)
}

/// `{"ok": true, "agent", "id", ...extra}`: what a command that changed one
/// session says about it. `extra` is the operation's own outcome, flattened.
fn mutated(session: &Session, extra: impl serde::Serialize) -> serde_json::Value {
    let mut value = serde_json::json!({ "ok": true, "agent": session.handle.agent, "id": session.handle.native_id });
    if let (Some(base), Ok(serde_json::Value::Object(more))) = (value.as_object_mut(), serde_json::to_value(extra)) {
        base.extend(more);
    }
    value
}

/// One id per line (or NUL-ended), nothing else.
fn print_ids<'a>(ids: impl Iterator<Item = &'a str>, null: bool) -> anyhow::Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    for id in ids {
        write!(out, "{id}{}", if null { '\0' } else { '\n' })?;
    }
    Ok(())
}

fn build_filter(cli: &Cli) -> anyhow::Result<SessionFilter> {
    let agent = match &cli.agent {
        Some(name) => match AgentKind::parse(name) {
            Some(agent) => Some(agent),
            None => fail!(
                Usage,
                "unknown agent '{name}' (try: claude-code, opencode, jcode, codex, antigravity)"
            ),
        },
        None => None,
    };
    let project = match &cli.project {
        Some(p) => Some(
            p.canonicalize()
                .with_context(|| format!("project directory not found: {}", p.display()))?,
        ),
        None => None,
    };
    Ok(SessionFilter { agent, project, include_children: cli.include_children, include_debug: false })
}

fn list(filter: &SessionFilter, opts: &ListOpts, json: bool) -> anyhow::Result<()> {
    let select = &opts.select;
    if opts.follow {
        if select.ids_only || select.null {
            fail!(Usage, "--follow prints JSON events, not ids: drop --ids-only / -0");
        }
        return follow(filter, select.status.clone(), opts.interval);
    }
    // Children are read either way: the `+N` after a session counts them,
    // and `--include-children` nests them under it.
    let (sessions, problems) =
        ops::list_sessions_partial(&SessionFilter { include_children: true, ..filter.clone() });
    for problem in problems {
        eprintln!("asm: could not read {problem}");
    }
    for note in ops::store_notes() {
        eprintln!("asm: {note}");
    }
    // A match whose parent did not match stands on its own in the tree.
    let query = ListQuery { state: select.status.clone(), text: None };
    let sessions: Vec<Session> = sessions.into_iter().filter(|s| query.keep(s)).collect();
    let tree = ops::group_children(&sessions);
    if json || select.ids_only || select.null {
        // A flat array in listing order, nested or not. Without
        // `--include-children` it is the roots, as it always was.
        let listed: Vec<&Session> = if filter.include_children {
            sessions.iter().collect()
        } else {
            tree.iter().map(|n| &sessions[n.index]).collect()
        };
        if select.ids_only || select.null {
            return print_ids(listed.iter().map(|s| s.handle.native_id.as_str()), select.null);
        }
        println!("{}", serde_json::to_string_pretty(&listed)?);
        return Ok(());
    }
    if sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }
    if filter.include_children {
        let rows: Vec<format::Row> = ops::flatten(&tree, &|_| true)
            .into_iter()
            .map(|(i, depth)| format::Row { session: &sessions[i], depth, below: 0, parent: parent_label(&sessions, &sessions[i]) })
            .collect();
        format::session_rows(&rows, true);
    } else {
        let rows: Vec<format::Row> = tree
            .iter()
            .map(|n| format::Row { session: &sessions[n.index], depth: 0, below: n.descendants(), parent: None })
            .collect();
        format::session_rows(&rows, false);
    }
    Ok(())
}

/// The id a session's parent goes by in `among`: a parent is shown by the id
/// its own row shows (a jcode session by its name); one that is not listed
/// by what its id begins with.
fn parent_label(among: &[Session], s: &Session) -> Option<String> {
    let parent = s.parent.as_deref()?;
    Some(
        among
            .iter()
            .find(|p| p.handle.agent == s.handle.agent && p.handle.native_id == parent)
            .map_or_else(|| asm_core::model::short_id_of(s.handle.agent, parent, None), |p| p.short_id())
            .to_string(),
    )
}

fn children(query: &str, filter: &SessionFilter, select: &Select, json: bool) -> anyhow::Result<()> {
    let session = resolve(query, filter)?;
    let keep = ListQuery { state: select.status.clone(), text: None };
    let below: Vec<(usize, Session)> =
        ops::descendants(&session)?.into_iter().filter(|(_, s)| keep.keep(s)).collect();
    if select.ids_only || select.null {
        return print_ids(below.iter().map(|(_, s)| s.handle.native_id.as_str()), select.null);
    }
    if json {
        let rows: Vec<serde_json::Value> = below
            .iter()
            .map(|(depth, s)| {
                let mut value = serde_json::to_value(s)?;
                value["depth"] = (*depth).into();
                Ok::<_, serde_json::Error>(value)
            })
            .collect::<Result<_, _>>()?;
        return print_json(&rows);
    }
    if below.is_empty() {
        println!("No sessions under {}.", session.short_id());
        return Ok(());
    }
    let mut known: Vec<Session> = below.iter().map(|(_, s)| s.clone()).collect();
    known.push(session);
    let rows: Vec<format::Row> =
        below.iter().map(|(depth, s)| format::Row { session: s, depth: *depth, below: 0, parent: parent_label(&known, s) }).collect();
    format::session_rows(&rows, true);
    Ok(())
}

fn messages(
    query: &str,
    filter: &SessionFilter,
    role: Option<String>,
    tail: Option<usize>,
    range: Option<&str>,
    text_only: bool,
    format: &str,
) -> anyhow::Result<()> {
    use std::io::Write;
    let range = match range {
        Some(r) => Some(ops::MessageQuery::parse_range(r).map_err(|e| coded(ErrorCode::Usage, e))?),
        None => None,
    };
    let session = resolve(query, filter)?;
    let rows = ops::messages(&session, &ops::MessageQuery { role, range, text_only, tail })?;
    if format == "json" {
        return print_json(&rows);
    }
    let mut out = std::io::stdout().lock();
    for row in &rows {
        writeln!(out, "{}", serde_json::to_string(row)?)?;
    }
    Ok(())
}

/// `asm events`: print each change to the listing as a line of JSON, until
/// stopped. A closed stdout ends it quietly.
fn follow(filter: &SessionFilter, status: Option<String>, interval: f64) -> anyhow::Result<()> {
    use std::io::Write;
    let query = ListQuery { state: status, text: None };
    let mut watch = ops::SessionWatch::new();
    let mut shown: Vec<String> = Vec::new();
    let mut out = std::io::stdout().lock();
    let mut emit = |value: serde_json::Value| -> bool {
        writeln!(out, "{value}").and_then(|_| out.flush()).is_ok()
    };
    let report = |problems: Vec<String>, shown: &mut Vec<String>| {
        // Said when it starts or changes, not at every look.
        if problems != *shown {
            for problem in &problems {
                eprintln!("asm: could not read {problem}");
            }
            *shown = problems;
        }
    };
    let (_, problems) = watch.poll(filter, &query);
    report(problems, &mut shown);
    if !emit(serde_json::json!({ "event": "ready" })) {
        return Ok(());
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs_f64(interval));
        // A reader that left is otherwise noticed only by the next write,
        // which may be hours away.
        if stdout_closed() {
            return Ok(());
        }
        let (changes, problems) = watch.poll(filter, &query);
        report(problems, &mut shown);
        for change in changes {
            if !emit(serde_json::json!({ "event": change.kind, "session": change.session })) {
                return Ok(());
            }
        }
    }
}

/// Whether whoever reads stdout has gone (a closed pipe), without writing.
#[cfg(unix)]
fn stdout_closed() -> bool {
    let mut fd = libc::pollfd { fd: 1, events: 0, revents: 0 };
    // Safety: one valid pollfd and a zero timeout; poll only reports.
    unsafe { libc::poll(&mut fd, 1, 0) };
    fd.revents & (libc::POLLERR | libc::POLLHUP) != 0
}

#[cfg(not(unix))]
fn stdout_closed() -> bool {
    false
}

fn capabilities(json: bool) -> anyhow::Result<()> {
    let report = ops::capabilities_report();
    if json {
        return print_json(&report);
    }
    println!(
        "asm {}  json contract {}  ir {}  index schema {}",
        report.asm_version, report.json_contract_version, report.ir_version, report.index_schema_version
    );
    for a in &report.agents {
        let c = a.capabilities;
        let can: Vec<&str> = [
            ("list", c.list),
            ("read", c.read_transcript),
            ("live", c.liveness),
            ("rename", c.rename),
            ("archive", c.archive),
            ("move", c.relocate),
            ("delete", c.delete),
            ("import", c.import_ir),
            ("export", c.export_ir),
            ("resume", c.resume_native),
            ("send", c.send_message),
            ("hub", a.hub_sync),
        ]
        .into_iter()
        .filter_map(|(name, on)| on.then_some(name))
        .collect();
        let store = match (&a.store_root, a.installed) {
            (Some(root), true) => root.display().to_string(),
            _ => "not installed".to_string(),
        };
        let schema = a.schema.map_or(String::new(), |s| {
            format!("  schema {s}{}", if a.unmigrated == Some(true) { " (migrating)" } else { "" })
        });
        println!("{:<12} {store}{schema}\n             {}", a.agent, can.join(" "));
    }
    Ok(())
}

fn projects(json: bool, show_worktrees: bool) -> anyhow::Result<()> {
    let projects = ops::list_projects()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&projects)?);
        return Ok(());
    }
    if projects.is_empty() {
        println!("No projects found.");
        return Ok(());
    }
    if !show_worktrees {
        format::project_table(&projects);
        return Ok(());
    }
    for project in &projects {
        format::project_table(std::slice::from_ref(project));
        format::project_worktrees(project);
    }
    Ok(())
}

fn show(query: &str, filter: &SessionFilter, json: bool) -> anyhow::Result<()> {
    let session = resolve(query, filter)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&session)?);
        return Ok(());
    }
    format::session_card(&session);
    Ok(())
}

fn rename(query: &str, title: &str, filter: &SessionFilter, json: bool) -> anyhow::Result<()> {
    let session = resolve(query, filter)?;
    ops::rename(&session, title)?;
    if json {
        return print_json(&mutated(&session, serde_json::json!({ "title": title })));
    }
    println!("Renamed {} to \"{title}\".", session.short_id());
    Ok(())
}

fn daemon_status(json: bool) -> anyhow::Result<()> {
    use asm_core::hub::daemon::{DaemonState, ago, status};
    let st = status()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&st)?);
        return Ok(());
    }
    let now = jiff::Timestamp::now().as_second().max(0) as u64;
    let since = |t: u64| format!("{} ago", ago(now.saturating_sub(t)));
    let Some(f) = &st.file else {
        println!("daemon      not running (never started here; `asm daemon` starts one)");
        return Ok(());
    };
    match st.state {
        DaemonState::Running => println!("daemon      running (pid {}, up {})", f.pid, ago(now.saturating_sub(f.started))),
        DaemonState::Hung => println!(
            "daemon      not responding (pid {}): no pass finished for {}",
            f.pid,
            ago(st.quiet_for.unwrap_or(0))
        ),
        DaemonState::NotRunning => println!("daemon      not running (last ran {})", since(f.last_pass.unwrap_or(f.started))),
    }
    println!("hub         {} as {}{}", f.hub, f.machine, if f.hub_reachable { "" } else { "  (unreachable)" });
    println!("interval    {}s", f.interval);
    println!("last pass   {}", f.last_pass.map_or("none yet".into(), since));
    println!("last push   {}", f.last_push.map_or("none yet".into(), since));
    println!(
        "pushes      {}",
        f.active_window.map_or("everything that differs from the hub".to_string(), |w| format!(
            "sessions live or changed in the last {}",
            ago(w)
        ))
    );
    println!("waiting     {} session{}", f.pending, if f.pending == 1 { "" } else { "s" });
    if f.idle > 0 {
        println!("idle        {} not pushed: out of step but not being worked in", f.idle);
    }
    println!("totals      {} synced over {} passes", f.synced, f.passes);
    if let Some(e) = &f.last_error {
        println!("problem     {e}");
    }
    if !f.recent.is_empty() {
        println!("\nrecent");
        for e in f.recent.iter().rev().take(8) {
            println!("  {:>5} ago  {}{}", ago(now.saturating_sub(e.at)), if e.ok { "" } else { "! " }, e.line);
        }
    }
    Ok(())
}

fn move_session(query: &str, new_dir: &std::path::Path, filter: &SessionFilter, json: bool) -> anyhow::Result<()> {
    let session = resolve(query, filter)?;
    let outcome = ops::relocate(&session, new_dir)?;
    if json {
        return print_json(&mutated(
            &session,
            serde_json::json!({ "dir": new_dir, "new_transcript": outcome.new_transcript, "warnings": outcome.warnings }),
        ));
    }
    for warning in &outcome.warnings {
        eprintln!("warning: {warning}");
    }
    match &outcome.new_transcript {
        Some(path) => println!(
            "Moved {} to {} (transcript: {}).",
            session.short_id(),
            new_dir.display(),
            path.display()
        ),
        None => println!("Moved {} to {}.", session.short_id(), new_dir.display()),
    }
    Ok(())
}

fn archive(query: &str, filter: &SessionFilter, json: bool) -> anyhow::Result<()> {
    let session = resolve(query, filter)?;
    let outcome = ops::archive(&session)?;
    if json {
        return print_json(&mutated(&session, &outcome));
    }
    match &outcome.archived_to {
        Some(dir) => println!("Archived {} to {}.", session.short_id(), dir.display()),
        None => println!("Archived {} (native flag).", session.short_id()),
    }
    Ok(())
}

fn unarchive(query: &str, filter: &SessionFilter, json: bool) -> anyhow::Result<()> {
    let restored = ops::unarchive(query, filter)?;
    if json {
        // The session may be one only the archive store knows, so it is
        // named as it was asked for; `restored_to` is null for an agent that
        // clears a flag in place.
        return print_json(&serde_json::json!({ "ok": true, "query": query, "restored_to": restored }));
    }
    match restored {
        Some(path) => println!("Restored to {}.", path.display()),
        None => println!("Unarchived {query}."),
    }
    Ok(())
}

/// How many of a session's children the confirmation names; the rest are
/// counted.
const DELETE_NAMES: usize = 5;

/// What `asm delete` asks. A session with sessions under it says so: for the
/// agents that delete the whole tree (`cascades`) it names what goes with
/// it, and for the others what stays. `below` is [`ops::descendants`].
fn delete_prompt(
    session: &asm_core::model::Session,
    below: &[(usize, asm_core::model::Session)],
    cascades: bool,
) -> String {
    let mut prompt = format!(
        "Delete {} \"{}\" ({}) and all its sidecars?",
        session.short_id(),
        session.title.as_deref().unwrap_or("(untitled)"),
        session.handle.agent,
    );
    let n = below.len();
    let s = if n == 1 { "" } else { "s" };
    if n > 0 && cascades {
        prompt.push_str(&format!(" It also deletes the {n} session{s} under it:\n"));
        for (depth, child) in below.iter().take(DELETE_NAMES) {
            prompt.push_str(&format!(
                "  {}{}  {}\n",
                "  ".repeat(depth - 1),
                child.short_id(),
                format::truncate(child.title.as_deref().unwrap_or("(untitled)"), 60),
            ));
        }
        if n > DELETE_NAMES {
            prompt.push_str(&format!("  … and {} more\n", n - DELETE_NAMES));
        }
    } else if n > 0 {
        let (the, are) = if n == 1 { ("The session".to_string(), "is") } else { (format!("The {n} sessions"), "are") };
        prompt.push_str(&format!(" {the} under it {are} kept, and top-level from now on.\n"));
    }
    prompt.push_str(if prompt.ends_with('\n') { "[y/N] " } else { " [y/N] " });
    prompt
}

fn delete(query: &str, yes: bool, filter: &SessionFilter, json: bool) -> anyhow::Result<()> {
    // A program cannot answer a prompt: say what is missing instead.
    if json && !yes {
        fail!(Usage, "confirmation required: pass --yes");
    }
    let session = resolve(query, filter)?;
    if !yes {
        // Asked before anything is deleted, and only when it is to be shown.
        let below = ops::descendants(&session).unwrap_or_default();
        eprint!("{}", delete_prompt(&session, &below, ops::deletes_descendants(session.handle.agent)));
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }
    let report = ops::delete(&session)?;
    if json {
        return print_json(&mutated(&session, &report));
    }
    if let Some(backup) = &report.backup_dir {
        println!("Backed up to {}.", backup.display());
    }
    let touched = report.removed.len();
    match report.descendants {
        0 => println!("Deleted {} ({touched} paths/stores touched).", session.short_id()),
        n => println!(
            "Deleted {} and the {n} session{} under it ({touched} paths/stores touched).",
            session.short_id(),
            if n == 1 { "" } else { "s" }
        ),
    }
    if let Some(note) = &report.note {
        println!("{note}.");
    }
    Ok(())
}

fn resume(query: &str, filter: &SessionFilter) -> anyhow::Result<()> {
    let session = resolve(query, filter)?;
    let mut cmd = ops::resume_command(&session)?;
    let program = cmd.get_program().to_string_lossy().into_owned();
    let status = cmd
        .status()
        .with_context(|| format!("failed to launch '{program}' — is it on PATH?"))?;
    match status.code() {
        Some(0) | None => Ok(()),
        Some(code) => std::process::exit(code),
    }
}

fn doctor(json: bool) -> anyhow::Result<()> {
    let report = ops::doctor()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    if report.agents.is_empty() {
        println!("No agent stores detected.");
        return Ok(());
    }
    for agent in &report.agents {
        let state = if agent.store_found { "ok" } else { "missing" };
        println!(
            "{}: store {} ({}), {} sessions",
            agent.agent,
            state,
            agent.store_root.display(),
            agent.session_count
        );
        for warning in &agent.warnings {
            println!("  warning: {warning}");
        }
    }
    Ok(())
}

/// Send a message into a session and stream the reply to the terminal.
///
/// The reply is rendered as it arrives rather than collected and printed at
/// the end: these turns run for minutes, and a silent terminal is
/// indistinguishable from a hung one.
fn send(
    r#ref: &str,
    message: &str,
    filter: &SessionFilter,
    json: bool,
) -> anyhow::Result<()> {
    use std::io::Write;

    let session = resolve(r#ref, filter)?;
    let message = if message == "-" {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
        buf
    } else {
        message.to_string()
    };
    if message.trim().is_empty() {
        fail!(Usage, "refusing to send an empty message");
    }

    if !json {
        eprintln!(
            "sending to {} {} in {}",
            session.handle.agent,
            session.short_id(),
            session.project_root.display()
        );
    }

    let mut failure = None;
    let mut out = std::io::stdout();
    asm_core::live::send(&session, &message, &mut |event| {
        if json {
            if let Ok(line) = serde_json::to_string(&event) {
                let _ = writeln!(out, "{line}");
            }
            let _ = out.flush();
            if let LiveEvent::Done { ok: false, error } = &event {
                failure = error.clone();
            }
            return;
        }
        match event {
            LiveEvent::Started { .. } => {}
            LiveEvent::Text { text, .. } => {
                let _ = write!(out, "{text}");
                let _ = out.flush();
            }
            LiveEvent::Reasoning { text } => eprintln!("\x1b[2m{}\x1b[0m", text.trim()),
            LiveEvent::ToolCall { name, detail } => {
                eprintln!("\x1b[2m· {name}{}\x1b[0m", detail.map(|d| format!(" {d}")).unwrap_or_default())
            }
            LiveEvent::ToolResult { is_error: true, output, .. } => {
                eprintln!("\x1b[33m· tool failed: {}\x1b[0m", output.trim())
            }
            LiveEvent::ToolResult { .. } => {}
            LiveEvent::Usage(usage) => {
                if let Some(cost) = usage.cost_usd {
                    eprintln!("\x1b[2m${cost:.4}\x1b[0m");
                }
            }
            LiveEvent::Raw { line } => eprintln!("\x1b[2m{line}\x1b[0m"),
            LiveEvent::Done { ok: false, error } => failure = error,
            LiveEvent::Done { .. } => {
                let _ = writeln!(out);
            }
        }
    })?;

    match failure {
        Some(error) => Err(coded(ErrorCode::Internal, error)),
        None => Ok(()),
    }
}

fn print_json(value: &impl serde::Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn hub_store() -> anyhow::Result<asm_core::hub::store::Hub> {
    let root = asm_core::hub::store::Hub::default_root().context("cannot determine asm's data directory")?;
    // `attach`, not `open`: this may run beside a live `asm hub serve`.
    Ok(asm_core::hub::store::Hub::attach(&root)?)
}

/// Hub management runs on the hub machine and reads its store directly, so
/// there is no admin API for a stolen credential to reach.
fn hub(command: HubCommand, json: bool) -> anyhow::Result<()> {
    let store = hub_store()?;
    match command {
        HubCommand::Serve { .. } | HubCommand::Receive { .. } | HubCommand::Send { .. } => {
            unreachable!("dispatched before the store is opened")
        }
        HubCommand::Token { rotate } => {
            let token = if rotate { store.rotate_join_token()? } else { store.join_token()? };
            if json {
                return print_json(&serde_json::json!({ "token": token }));
            }
            println!("ASM_JOIN_TOKEN={token} asm join <hub-url>");
        }
        HubCommand::AdminToken => {
            let token = store.rotate_admin_token()?;
            if json {
                return print_json(&serde_json::json!({ "token": token }));
            }
            println!("Admin token (shown once; running this again replaces it):\n\n  {token}\n");
            println!("Open /admin on this hub and paste it. It controls the hub itself: machines,");
            println!("the join token, deleting sessions. It does not touch anyone's agents.");
        }
        HubCommand::CommandsToken => {
            let token = store.rotate_commands_token()?;
            if json {
                return print_json(&serde_json::json!({ "token": token }));
            }
            println!("Commands token (shown once; running this again replaces it):\n\n  {token}\n");
            println!("ASM_HUB_COMMANDS_TOKEN={token} asm control push <machine> <session>");
            println!("It lets its holder ask machines that turned remote control on to push or pull");
            println!("sessions. It cannot read sessions, delete anything, or open the admin page.");
        }
        HubCommand::Machines => {
            let machines = store.machines()?;
            if json {
                return print_json(&machines);
            }
            if machines.is_empty() {
                println!("No machines have joined. `asm hub token` prints the join command.");
            } else {
                format::machine_table(&machines, None);
            }
        }
        HubCommand::Revoke { machine } => {
            let removed = store.revoke(&machine).with_context(|| format!("revoking {machine}"))?;
            if json {
                return print_json(&removed);
            }
            println!("Revoked {} ({}). Its credential no longer works.", removed.name, removed.id);
        }
    }
    Ok(())
}

/// The join token: `--token -` reads it from stdin, else $ASM_JOIN_TOKEN.
fn join_token(token: Option<&str>) -> anyhow::Result<String> {
    match token {
        Some("-") => {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).context("reading the join token from stdin")?;
            Ok(line.trim().to_string())
        }
        Some(token) => Ok(token.to_string()),
        None => std::env::var("ASM_JOIN_TOKEN").map_err(|_| {
            coded(ErrorCode::Usage, "set ASM_JOIN_TOKEN to the token `asm hub token` prints, or pass --token -")
        }),
    }
}

fn machine_name(name: Option<&str>) -> anyhow::Result<String> {
    name.map(String::from)
        .or_else(asm_core::process::hostname)
        .ok_or_else(|| coded(ErrorCode::Usage, "could not tell this machine's name; pass --name"))
}

fn join(url: &str, token: Option<&str>, name: Option<&str>, insecure_http: bool, json: bool) -> anyhow::Result<()> {
    let token = join_token(token)?;
    let name = machine_name(name)?;
    let remote = asm_core::hub::client::join(url, &token, &name, insecure_http)?;
    if json {
        return print_json(&serde_json::json!({ "url": remote.url, "machine": remote.machine }));
    }
    println!("Joined {} as {} ({}).", remote.url, remote.machine.name, remote.machine.id);
    println!("Push this machine's sessions with `asm push --all`.");
    Ok(())
}

fn push(
    refs: &[String],
    all: bool,
    force: bool,
    move_away: bool,
    filter: &SessionFilter,
    json: bool,
) -> anyhow::Result<()> {
    use asm_core::bulk::ItemOutcome;
    if move_away && all {
        fail!(Usage, "--move takes the sessions to move by name, not --all");
    }
    let sessions = match (all, refs.is_empty()) {
        (true, true) => ops::list_sessions(filter)?,
        (false, false) => refs
            .iter()
            .map(|r| ops::resolve_ref(r, filter).with_context(|| format!("resolving {r}")))
            .collect::<anyhow::Result<Vec<_>>>()?,
        (true, false) => fail!(Usage, "name sessions or pass --all, not both"),
        (false, true) => fail!(Usage, "name the sessions to push, or pass --all"),
    };
    // Checked before anything is pushed: a move that uploads and then
    // cannot archive here is a copy the user did not ask for.
    if move_away
        && let Some(s) = sessions.iter().find(|s| {
            !asm_core::hub::bundle::restorable(s.handle.agent)
                || !ops::adapter_for(s.handle.agent)
                    .is_some_and(|a| asm_core::adapter::AgentRead::capabilities(&a).archive)
        })
    {
        fail!(
            Unsupported,
            "{} sessions cannot both be restored elsewhere and archived here, so {} is not \
             moved; push it without --move",
            s.handle.agent,
            s.short_id()
        );
    }
    if move_away
        && let Some(s) = sessions.iter().find(|s| {
            s.handle.agent == asm_core::model::AgentKind::JCode
                && matches!(&s.handle.location, asm_core::model::SessionLocation::JsonlFile { path }
                    if std::fs::metadata(path.with_extension("journal.jsonl")).is_ok_and(|m| m.len() > 0))
        })
    {
        fail!(
            Conflict,
            "jcode has turns of {} it has not written into the session yet; resume it once so it \
             does, then move it",
            s.short_id()
        );
    }
    let remote = asm_core::hub::client::load()?;
    let mut report = asm_core::hub::actions::push(&remote, &sessions, force, move_away)?;
    // Archived only once the hub holds exactly this copy, which an Ok means.
    if move_away {
        for item in &mut report.items {
            let ItemOutcome::Ok { note } = &item.outcome else { continue };
            let Some(session) = sessions
                .iter()
                .find(|s| s.handle.agent == item.agent && s.handle.native_id == item.native_id)
            else {
                continue;
            };
            item.outcome = match ops::archive(session) {
                Ok(_) => ItemOutcome::Ok { note: format!("{note}; archived here, `asm pull` it on the other machine") },
                Err(e) => ItemOutcome::Failed { error: format!("on the hub, but not archived here: {e}") },
            };
        }
    }
    if json {
        print_json(&report)?;
    } else {
        for item in &report.items {
            if let ItemOutcome::Ok { note } = &item.outcome
                && note != "in sync"
            {
                println!("{}: {note}", item.label);
            }
        }
        println!("{}", report.summary("Push"));
        for line in report.problems() {
            eprintln!("  {line}");
        }
    }
    if report.failed() > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn pull(
    query: Option<&str>,
    all: bool,
    project_dir: Option<&std::path::Path>,
    filter: &SessionFilter,
    json: bool,
) -> anyhow::Result<()> {
    let remote = asm_core::hub::client::load()?;
    let query = match (query, all) {
        (Some(query), false) => query,
        (None, true) => {
            let report =
                asm_core::hub::actions::pull_all(&remote, filter.agent, filter.project.as_deref())?;
            if json {
                print_json(&report)?;
            } else {
                for item in &report.items {
                    if let asm_core::bulk::ItemOutcome::Ok { note } = &item.outcome {
                        println!("{}: {note}", item.label);
                    }
                }
                println!("{}", report.summary("Pull"));
                for line in report.problems() {
                    eprintln!("  {line}");
                }
            }
            if report.failed() > 0 {
                std::process::exit(1);
            }
            return Ok(());
        }
        _ => fail!(Usage, "name the session to pull, or pass --all"),
    };
    let pulled = asm_core::hub::actions::pull(&remote, query, project_dir)?;
    report_pulled(
        &pulled,
        "the hub's copy",
        "`asm push` it",
        "`asm push --force` makes this machine's copy the head; the other stays on the hub as a revision",
        json,
    )
}

fn session_label(pulled: &asm_core::hub::actions::Pulled) -> String {
    format!("{} {}", pulled.agent, asm_core::model::short_id_of(pulled.agent, &pulled.id, pulled.slug.as_deref()))
}

/// Say what a pull did to this machine — from the hub or from a peer — and
/// how to resume. The two differ only in what to do when this copy is ahead
/// (`ahead`) or the two diverged (`diverged`).
fn report_pulled(
    pulled: &asm_core::hub::actions::Pulled,
    theirs: &str,
    ahead: &str,
    diverged: &str,
    json: bool,
) -> anyhow::Result<()> {
    use asm_core::hub::bundle::InstallOutcome;
    if json {
        print_json(&pulled)?;
        if pulled.installed.outcome == InstallOutcome::Diverged {
            std::process::exit(1);
        }
        return Ok(());
    }
    let from = pulled.from.as_deref().unwrap_or("the hub");
    let label = session_label(pulled);
    let project = pulled.installed.project_root.display();
    match pulled.installed.outcome {
        InstallOutcome::New => println!("Installed {label} from {from} in {project}."),
        InstallOutcome::FastForward { appended } => println!(
            "Updated {label} from {from}: {} of new conversation appended.",
            asm_core::fmt::human_bytes(appended)
        ),
        InstallOutcome::Replaced => println!(
            "Updated {label} from {from}: the older copy here was backed up and replaced."
        ),
        InstallOutcome::Renamed => println!(
            "Updated {label} from {from}: renamed to match (the old titles were saved first); nothing else differed."
        ),
        InstallOutcome::InSync => println!("{label} is already in sync with {from}."),
        InstallOutcome::Ahead => {
            println!("{label} here is ahead of {theirs}; nothing changed. {ahead}.")
        }
        InstallOutcome::Diverged => fail!(
            Conflict,
            "{label} has been continued both here and on {from} since they last synced, so \
             neither copy contains the other. Nothing was changed. {diverged}"
        ),
    }
    if pulled.installed.outcome != InstallOutcome::Ahead
        && let Ok(session) = ops::resolve_ref(&format!("{}:{}", pulled.agent, pulled.id), &SessionFilter::default())
        && let Ok(cmd) = ops::resume_command(&session)
    {
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy()).collect();
        println!("Resume with: cd {project} && {} {}", cmd.get_program().to_string_lossy(), args.join(" "));
    }
    Ok(())
}

/// What a diverged pair of copies can do without a hub: nothing automatic.
const PEER_DIVERGED: &str = "A peer transfer never replaces a copy: `asm archive` the copy you do not \
                             want on one side and transfer again, or sync both machines through a hub";

fn push_to(
    peer_name: &str,
    refs: &[String],
    project_dir: Option<&std::path::Path>,
    filter: &SessionFilter,
    json: bool,
) -> anyhow::Result<()> {
    use asm_core::hub::bundle::InstallOutcome;
    let [query] = refs else { fail!(Usage, "--to sends one session; name it") };
    let peer = asm_core::hub::peer::get(peer_name)?;
    let session = resolve(query, filter)?;
    let sent = asm_core::hub::peer::push_to(&peer, &session, project_dir)?;
    if json {
        print_json(&sent)?;
        if sent.installed.outcome == InstallOutcome::Diverged {
            std::process::exit(1);
        }
        return Ok(());
    }
    let label = session_label(&sent);
    let project = sent.installed.project_root.display();
    match sent.installed.outcome {
        InstallOutcome::New => println!("Sent {label} to {peer_name}: installed in {project} there."),
        InstallOutcome::FastForward { appended } => println!(
            "Updated {label} on {peer_name}: {} of new conversation appended.",
            asm_core::fmt::human_bytes(appended)
        ),
        InstallOutcome::Replaced => {
            println!("Updated {label} on {peer_name}: the older copy there was backed up and replaced.")
        }
        InstallOutcome::Renamed => println!(
            "Updated {label} on {peer_name}: renamed to match (the old titles were saved first); nothing else differed."
        ),
        InstallOutcome::InSync => println!("{peer_name} already has exactly this copy of {label}."),
        InstallOutcome::Ahead => println!(
            "{peer_name}'s copy of {label} is ahead of this one; nothing changed. `asm pull --from {peer_name}` it."
        ),
        InstallOutcome::Diverged => fail!(
            Conflict,
            "{label} has been continued both here and on {peer_name} since the two last agreed, so \
             neither copy contains the other. Nothing was changed. {PEER_DIVERGED}"
        ),
    }
    Ok(())
}

fn pull_from(peer_name: &str, query: &str, project_dir: Option<&std::path::Path>, json: bool) -> anyhow::Result<()> {
    let peer = asm_core::hub::peer::get(peer_name)?;
    let pulled = asm_core::hub::peer::pull_from(&peer, query, project_dir)?;
    report_pulled(
        &pulled,
        &format!("{peer_name}'s copy"),
        &format!("`asm push --to {peer_name}` it"),
        PEER_DIVERGED,
        json,
    )
}

fn peer(command: PeerCommand, filter: &SessionFilter, json: bool) -> anyhow::Result<()> {
    use asm_core::hub::peer;
    let shown = |p: &peer::Peer| serde_json::json!({ "name": p.name, "address": p.address() });
    match command {
        PeerCommand::Add { name, address, token, machine, insecure_http } => {
            // The token is read only when the address needs one: an ssh peer
            // must not fail for want of a token it would never use.
            let token = if address.starts_with("http") { Some(join_token(token.as_deref())?) } else { None };
            let added = peer::add(&name, &address, token.as_deref(), &machine_name(machine.as_deref())?, insecure_http)?;
            if json {
                return print_json(&shown(&added));
            }
            println!("Added peer {} at {}.", added.name, added.address());
            println!("Send a session with `asm push <session> --to {}`; `asm peer sessions {}` lists what is there.", added.name, added.name);
        }
        PeerCommand::Remove { name } => {
            let removed = peer::remove(&name)?;
            if json {
                return print_json(&shown(&removed));
            }
            println!("Removed peer {} ({}).", removed.name, removed.address());
        }
        PeerCommand::List => {
            let peers = peer::list()?;
            if json {
                return print_json(&peers.iter().map(shown).collect::<Vec<_>>());
            }
            if peers.is_empty() {
                println!("No peers. `asm peer add <name> ssh://[user@]host` adds one.");
            }
            for p in &peers {
                println!("{}  {}", p.name, p.address());
            }
        }
        PeerCommand::Sessions { peer: name } => {
            let mut sessions = peer::sessions(&peer::get(&name)?)?;
            if let Some(agent) = filter.agent {
                sessions.retain(|s| s.handle.agent == agent);
            }
            if json {
                return print_json(&sessions);
            }
            if sessions.is_empty() {
                println!("No sessions on {name}.");
                return Ok(());
            }
            format::session_table(&sessions);
        }
    }
    Ok(())
}

/// `asm hub receive`: the ssh end of `asm push --to`. Only the outcome goes
/// to stdout, as JSON, since the caller parses it.
fn hub_receive(project_dir: Option<&std::path::Path>) -> anyhow::Result<()> {
    let installed = asm_core::hub::peer::receive(&mut std::io::stdin().lock(), u64::MAX, project_dir)?;
    println!("{}", serde_json::to_string(&installed)?);
    Ok(())
}

/// `asm hub send`: the ssh end of `asm pull --from`. Only the tar goes to
/// stdout.
fn hub_send(query: &str) -> anyhow::Result<()> {
    let session = resolve(query, &SessionFilter::default())?;
    let packed = asm_core::hub::peer::pack(&session)?;
    let mut tar = std::fs::File::open(&packed.tar)?;
    std::io::copy(&mut tar, &mut std::io::stdout().lock())?;
    Ok(())
}

fn remote(command: RemoteCommand, filter: &SessionFilter, json: bool) -> anyhow::Result<()> {
    let remote = asm_core::hub::client::load()?;
    match command {
        RemoteCommand::List => {
            let local = ops::list_sessions(filter)?;
            let rows = asm_core::hub::actions::remote_list(&remote, &local)?;
            if json {
                return print_json(&rows);
            }
            if rows.is_empty() {
                println!("Nothing here or on the hub yet.");
            } else {
                format::remote_table(&rows);
            }
        }
        RemoteCommand::Machines => {
            let machines = remote.machines()?;
            if json {
                return print_json(&machines);
            }
            format::machine_table(&machines, Some(&remote.machine.id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("asm").chain(args.iter().copied()))
    }

    /// `--all` means "everything the filters match" on push and pull; the
    /// subagent switch is `--include-children`. The two had one name, and
    /// `asm push --all` set both.
    #[test]
    fn all_means_everything_matching_and_children_have_their_own_flag() {
        assert!(parse(&["list", "--include-children"]).unwrap().include_children);
        assert!(!parse(&["push", "--all"]).unwrap().include_children, "--all no longer adds children");
        assert!(
            parse(&["list", "--all"]).is_err(),
            "the old spelling fails loudly rather than meaning something else"
        );

        // Pull takes a session or --all, never both, and --all lands every
        // session at its own place.
        assert!(parse(&["pull", "7f3a1c88"]).is_ok());
        assert!(parse(&["pull", "--all", "--agent", "codex", "--project", "/x"]).is_ok());
        assert!(parse(&["pull", "7f3a1c88", "--all"]).is_err());
        assert!(parse(&["pull", "--all", "--project-dir", "/x"]).is_err());
    }

    /// A peer transfer names one peer and, for a push, may say where the
    /// session goes there; the ssh ends of it are commands but not help.
    #[test]
    fn peer_transfers_parse_and_the_ssh_ends_are_hidden() {
        assert!(matches!(parse(&["push", "7f3a", "--to", "desk"]).unwrap().command, Some(Command::Push { to: Some(_), .. })));
        assert!(parse(&["push", "7f3a", "--to", "desk", "--project-dir", "/x"]).is_ok());
        assert!(parse(&["push", "7f3a", "--project-dir", "/x"]).is_err(), "--project-dir on a push is for --to");
        assert!(matches!(parse(&["pull", "7f3a", "--from", "desk"]).unwrap().command, Some(Command::Pull { from: Some(_), .. })));
        assert!(parse(&["peer", "add", "desk", "ssh://me@desk"]).is_ok());
        assert!(parse(&["peer", "sessions", "desk", "--agent", "codex"]).is_ok());
        assert!(parse(&["hub", "serve", "--peer"]).is_ok());
        assert!(parse(&["hub", "receive", "--project-dir", "/x"]).is_ok());
        assert!(parse(&["hub", "send", "claude-code:7f3a"]).is_ok());
        let help = match Cli::try_parse_from(["asm", "hub", "--help"]) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("--help is an error that prints"),
        };
        assert!(help.contains("serve") && !help.contains("receive"), "{help}");
    }

    fn session(agent: AgentKind, id: &str, title: &str, parent: Option<&str>) -> asm_core::model::Session {
        use asm_core::model::{SessionLocation, SessionRef, SessionStatus};
        asm_core::model::Session {
            handle: SessionRef {
                agent,
                native_id: id.into(),
                location: SessionLocation::JsonlFile { path: format!("/t/{id}").into() },
            },
            title: Some(title.into()),
            slug: None,
            project_root: "/w/app".into(),
            git_branch: None,
            created: None,
            updated: None,
            model: None,
            usage: Default::default(),
            status: SessionStatus::Idle,
            parent: parent.map(str::to_string),
            agent_version: None,
            size_bytes: None,
        }
    }

    /// A session with nothing under it is asked about exactly as it always was.
    #[test]
    fn deleting_a_session_with_no_children_asks_what_it_always_did() {
        let root = session(AgentKind::OpenCode, "ses_71bc0e4faa21", "Plan", None);
        for cascades in [true, false] {
            assert_eq!(
                delete_prompt(&root, &[], cascades),
                "Delete ses_71bc \"Plan\" (opencode) and all its sidecars? [y/N] "
            );
        }
    }

    /// OpenCode (1.x and 2.x alike: the prompt knows the agent, not the
    /// store) deletes a session's whole tree, so the prompt says what that
    /// is: how many, and which, indented by depth.
    #[test]
    fn deleting_an_opencode_parent_names_what_goes_with_it() {
        let root = session(AgentKind::OpenCode, "ses_root00000000", "Plan", None);
        let below = vec![
            (1, session(AgentKind::OpenCode, "ses_kid100000000", "Review docs (@general subagent)", Some("ses_root00000000"))),
            (2, session(AgentKind::OpenCode, "ses_gran10000000", "Nested", Some("ses_kid100000000"))),
        ];
        assert_eq!(
            delete_prompt(&root, &below, true),
            "Delete ses_root \"Plan\" (opencode) and all its sidecars? It also deletes the 2 sessions under it:\n\
             \x20 ses_kid1  Review docs (@general subagent)\n\
             \x20   ses_gran  Nested\n\
             [y/N] "
        );
        let one = &below[..1];
        assert!(delete_prompt(&root, one, true).contains("the 1 session under it:"));
    }

    /// A long tree is counted, not listed.
    #[test]
    fn a_long_list_of_children_is_cut_and_counted() {
        let root = session(AgentKind::OpenCode, "ses_root00000000", "Plan", None);
        let below: Vec<_> = (0..8)
            .map(|i| (1, session(AgentKind::OpenCode, &format!("ses_kid{i}0000000"), &"x".repeat(100), Some("ses_root00000000"))))
            .collect();
        let prompt = delete_prompt(&root, &below, true);
        assert!(prompt.contains("the 8 sessions under it:"), "{prompt}");
        assert_eq!(prompt.lines().filter(|l| l.starts_with("  ses_kid")).count(), DELETE_NAMES);
        assert!(prompt.contains("… and 3 more"), "{prompt}");
        assert!(prompt.lines().all(|l| l.chars().count() < 100), "titles are clipped");
        assert!(prompt.ends_with("\n[y/N] "));
    }

    /// Where deleting a session leaves its children (jcode), the prompt says
    /// they stay, rather than that they go.
    #[test]
    fn a_parent_whose_children_stay_says_so() {
        let main = session(AgentKind::JCode, "session_main_1790000000000_aaaa", "Main", None);
        let kid = (1, session(AgentKind::JCode, "session_kid_1790000000001_bbbb", "Kid", Some("session_main_1790000000000_aaaa")));
        let prompt = delete_prompt(&main, &[kid.clone(), kid], false);
        assert!(prompt.contains("The 2 sessions under it are kept, and top-level from now on.\n"), "{prompt}");
        assert!(!prompt.contains("also deletes"));
    }
}
