//! Command-line frontend: clap definitions and handlers over `asm_core::ops`.

mod format;
mod update;

use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};

use asm_core::adapter::SessionFilter;
use asm_core::model::AgentKind;
use asm_core::ops;

#[derive(Parser)]
#[command(name = "asm", version, about = "Cross-agent session manager")]
pub struct Cli {
    /// Emit machine-readable JSON instead of tables.
    #[arg(long, global = true)]
    json: bool,

    /// Restrict to one agent (claude-code | opencode).
    #[arg(long, global = true)]
    agent: Option<String>,

    /// Restrict to sessions of this project directory.
    #[arg(long, global = true)]
    project: Option<PathBuf>,

    /// Include subagent/child sessions (hidden by default).
    #[arg(long, global = true)]
    all: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List sessions across all agents (default command).
    List,
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
    Show { r#ref: String },
    /// Resume a session interactively in its native agent.
    Resume { r#ref: String },
    /// Set a session's title (uses the agent's native mechanism).
    Rename { r#ref: String, title: String },
    /// Move a session to another project directory / worktree.
    Move { r#ref: String, new_dir: PathBuf },
    /// Archive a session (OpenCode: native flag; Claude: moved into asm's
    /// archive store).
    Archive { r#ref: String },
    /// Restore an archived session.
    Unarchive { r#ref: String },
    /// Delete a session and all its sidecars (backed up first).
    Delete {
        r#ref: String,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Import a session into another agent (the flagship).
    Import {
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
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Search the existing index without bringing it up to date.
        #[arg(long)]
        no_refresh: bool,
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
    /// Open the interactive TUI browser.
    Tui,
    /// Serve the web UI (loopback by default).
    Serve {
        #[arg(long, default_value_t = 7433)]
        port: u16,
        /// Address to bind. An IP or hostname; `0.0.0.0` / `::` bind every
        /// interface. Anything outside loopback exposes an unauthenticated
        /// API to your network.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
}

#[derive(Subcommand)]
enum SyncCommand {
    /// Make the archive a git repository (optionally with a remote).
    Init {
        #[arg(long)]
        remote: Option<String>,
    },
    /// Show what is archived and the repository's state.
    Status,
    /// Commit the current archive contents locally.
    Commit {
        #[arg(short, long)]
        message: Option<String>,
    },
}

/// A frontend the binary should launch after argument parsing (the CLI
/// crate stays free of TUI/web dependencies).
pub enum Frontend {
    Tui,
    Serve { host: String, port: u16 },
}

pub fn run() -> anyhow::Result<Option<Frontend>> {
    let cli = Cli::parse();
    let filter = build_filter(&cli)?;

    // Bare `asm` on a terminal opens the interactive browser; piped or
    // scripted invocations get the plain listing.
    let default_command = if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        Command::Tui
    } else {
        Command::List
    };

    match cli.command.unwrap_or(default_command) {
        Command::Tui => return Ok(Some(Frontend::Tui)),
        Command::Serve { port, host } => return Ok(Some(Frontend::Serve { host, port })),
        Command::List => list(&filter, cli.json),
        Command::Projects { worktrees } => projects(cli.json, worktrees),
        Command::Show { r#ref } => show(&r#ref, &filter, cli.json),
        Command::Resume { r#ref } => resume(&r#ref, &filter),
        Command::Rename { r#ref, title } => rename(&r#ref, &title, &filter),
        Command::Move { r#ref, new_dir } => move_session(&r#ref, &new_dir, &filter),
        Command::Archive { r#ref } => archive(&r#ref, &filter),
        Command::Unarchive { r#ref } => unarchive(&r#ref, &filter),
        Command::Delete { r#ref, yes } => delete(&r#ref, yes, &filter),
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
        Command::Export { r#ref, output } => export(&r#ref, output.as_deref(), &filter),
        Command::Worktrees { repo } => worktrees(repo.as_deref(), cli.json),
        Command::Search { query, limit, no_refresh } => {
            search(&query.join(" "), limit, !no_refresh, &filter, cli.json)
        }
        Command::Index { stats } => index(stats, cli.json),
        Command::Doctor => doctor(cli.json),
        Command::Update { check, force, version } => update::run(check, force, version, cli.json),
        Command::Sync(command) => sync(command, cli.json),
    }?;
    Ok(None)
}

/// Redrawing progress on one line only makes sense on a terminal; piped
/// output would otherwise collect escape sequences.
fn progress_reporter() -> impl FnMut(&str) {
    let tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
    move |message: &str| {
        if tty {
            eprint!("\r\x1b[2K{message}");
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
    json: bool,
) -> anyhow::Result<()> {
    use asm_core::index::{Index, SearchQuery};

    if query.trim().is_empty() {
        bail!("nothing to search for");
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
    })?;

    if json {
        println!("{}", serde_json::to_string_pretty(&hits)?);
        return Ok(());
    }
    if hits.is_empty() {
        println!("No matches for {query:?}.");
        return Ok(());
    }
    for hit in &hits {
        let id: String = hit.native_id.chars().take(if hit.agent == "opencode" { 12 } else { 8 }).collect();
        let archived = if hit.status == "archived" { "  (archived)" } else { "" };
        println!(
            "{}  {}  {}{archived}",
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

fn index(stats_only: bool, json: bool) -> anyhow::Result<()> {
    use asm_core::index::Index;

    let mut index = Index::open()?;
    let report =
        if stats_only { None } else { Some(index.refresh(progress_reporter())?) };
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

    let target = AgentKind::parse(to)
        .with_context(|| format!("unknown target agent '{to}' (try: claude-code, opencode)"))?;

    if show_toolmap {
        for (from, to) in toolmap::table(target) {
            println!("{from} -> {to}");
        }
        return Ok(());
    }

    let mode = match mode {
        "full" => ImportMode::Full,
        "seed" => ImportMode::Seed,
        other => anyhow::bail!("unknown mode '{other}' (full | seed)"),
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
) -> anyhow::Result<()> {
    let session = resolve(query, filter)?;
    let ir = ops::export_ir(&session)?;
    let json = serde_json::to_string_pretty(&ir)?;
    match output {
        Some(path) => {
            std::fs::write(path, &json)?;
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
    if json {
        println!("{}", serde_json::to_string_pretty(&worktrees)?);
        return Ok(());
    }
    let sessions = ops::list_sessions(&SessionFilter::default())?;
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

fn resolve(query: &str, filter: &SessionFilter) -> anyhow::Result<asm_core::model::Session> {
    ops::resolve_ref(query, filter).map_err(|e| anyhow::anyhow!("{e}"))
}

fn build_filter(cli: &Cli) -> anyhow::Result<SessionFilter> {
    let agent = match &cli.agent {
        Some(name) => Some(
            AgentKind::parse(name)
                .with_context(|| format!("unknown agent '{name}' (try: claude-code, opencode)"))?,
        ),
        None => None,
    };
    let project = match &cli.project {
        Some(p) => Some(
            p.canonicalize()
                .with_context(|| format!("project directory not found: {}", p.display()))?,
        ),
        None => None,
    };
    Ok(SessionFilter { agent, project, include_children: cli.all })
}

fn list(filter: &SessionFilter, json: bool) -> anyhow::Result<()> {
    let sessions = ops::list_sessions(filter)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&sessions)?);
        return Ok(());
    }
    if sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }
    format::session_table(&sessions);
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
    let session = match ops::resolve_ref(query, filter) {
        Ok(s) => s,
        Err(e) => bail!("{e}"),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&session)?);
        return Ok(());
    }
    format::session_card(&session);
    Ok(())
}

fn rename(query: &str, title: &str, filter: &SessionFilter) -> anyhow::Result<()> {
    let session = resolve(query, filter)?;
    ops::rename(&session, title)?;
    println!("Renamed {} to \"{title}\".", session.short_id());
    Ok(())
}

fn move_session(query: &str, new_dir: &std::path::Path, filter: &SessionFilter) -> anyhow::Result<()> {
    let session = resolve(query, filter)?;
    let outcome = ops::relocate(&session, new_dir)?;
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

fn archive(query: &str, filter: &SessionFilter) -> anyhow::Result<()> {
    let session = resolve(query, filter)?;
    let outcome = ops::archive(&session)?;
    match &outcome.archived_to {
        Some(dir) => println!("Archived {} to {}.", session.short_id(), dir.display()),
        None => println!("Archived {} (native flag).", session.short_id()),
    }
    Ok(())
}

fn unarchive(query: &str, filter: &SessionFilter) -> anyhow::Result<()> {
    match ops::unarchive(query, filter)? {
        Some(path) => println!("Restored to {}.", path.display()),
        None => println!("Unarchived {query}."),
    }
    Ok(())
}

fn delete(query: &str, yes: bool, filter: &SessionFilter) -> anyhow::Result<()> {
    let session = resolve(query, filter)?;
    if !yes {
        eprint!(
            "Delete {} \"{}\" ({}) and all its sidecars? [y/N] ",
            session.short_id(),
            session.title.as_deref().unwrap_or("(untitled)"),
            session.handle.agent,
        );
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }
    let report = ops::delete(&session)?;
    if let Some(backup) = &report.backup_dir {
        println!("Backed up to {}.", backup.display());
    }
    println!("Deleted {} ({} paths/stores touched).", session.short_id(), report.removed.len());
    Ok(())
}

fn resume(query: &str, filter: &SessionFilter) -> anyhow::Result<()> {
    let session = ops::resolve_ref(query, filter).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut cmd = ops::resume_command(&session).map_err(|e| anyhow::anyhow!("{e}"))?;
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
