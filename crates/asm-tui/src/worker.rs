//! Background worker: everything slower than a frame runs here.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender, channel};

use asm_core::adapter::SessionFilter;
use asm_core::ir::{IrPart, IrRole};
use asm_core::model::Session;
use asm_core::ops;

pub enum Request {
    /// Read every store. The answer also carries the projects and the
    /// stores' health, worked out from what was just read rather than from
    /// a listing apiece.
    Scan,
    LoadPreview(Box<Session>),
    Rename(Box<Session>, String),
    Archive(Box<Session>),
    Unarchive(Box<Session>),
    Delete(Box<Session>),
    Export(Box<Session>, std::path::PathBuf),
    Move(Box<Session>, std::path::PathBuf),
    Import(Box<Session>, asm_core::model::AgentKind),
    Search(String),
    /// How this machine stands with the hub, given the sessions it has.
    Hub(Vec<Session>),
    /// Pull one session from the hub, into a directory if one was given.
    Pull(asm_core::model::AgentKind, String, Option<std::path::PathBuf>),
    /// One verb over a selected set. Sessions are resolved by the caller at
    /// the moment the action is confirmed, never by index.
    Bulk(Vec<Session>, asm_core::bulk::BulkAction),
    /// Send a message into a session. Unlike every other request this one
    /// answers with a stream of `Response::Live` rather than a single
    /// response, and it occupies the worker until the agent is finished.
    Send(Box<Session>, String),
}

pub enum Response {
    /// Part of a scan, as the store gave it up.
    SessionBatch(Vec<Session>),
    /// The scan is finished; anything that could not be read is here.
    ScanDone(Vec<String>),
    /// How far the background index has got.
    Indexing(asm_core::index::RefreshProgress),
    /// (sessions re-read, sessions that could not be)
    Indexed(usize, usize),
    IndexFailed(String),
    /// (session native id, rendered transcript lines)
    Preview(String, Vec<PreviewLine>),
    /// (query, hits)
    Hits(String, Vec<asm_core::index::SearchHit>),
    /// The projects the list can be narrowed to — repositories with their
    /// worktrees, the same grouping the CLI and the web UI show.
    Projects(Vec<asm_core::model::Project>),
    Hub(Box<asm_core::hub::actions::HubStatus>, Option<asm_core::hub::daemon::DaemonStatus>),
    /// A pull that did not land. `needs_dir` when the session's directory
    /// does not exist here, so the TUI can ask for one rather than give up.
    PullFailed { agent: asm_core::model::AgentKind, id: String, message: String, needs_dir: bool },
    /// (report lines, number of warnings among them)
    Doctor(Vec<String>, usize),
    Done(String),
    Error(String),
    /// (the verb, the per-item report)
    Bulk(String, asm_core::bulk::BulkReport),
    /// One step of a reply in progress.
    Live(asm_core::live::LiveEvent),
}

pub struct PreviewLine {
    pub kind: PreviewKind,
    pub text: String,
}

pub enum PreviewKind {
    Role,
    Text,
    Tool,
    Meta,
}

pub struct Worker {
    pub tx: Sender<Request>,
    /// The index runs on its own thread: it is the slowest thing asm does
    /// on a busy machine, and nothing else should wait behind it.
    pub index_tx: Sender<()>,
    pub rx: Receiver<Response>,
    /// Set to stop a send in progress. A flag rather than a message
    /// because the worker is busy streaming and will not read its inbox
    /// again until the turn ends — which is exactly what needs
    /// interrupting.
    pub cancel: Arc<AtomicBool>,
}

pub fn spawn() -> Worker {
    let (req_tx, req_rx) = channel::<Request>();
    let (resp_tx, resp_rx) = channel::<Response>();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let index_tx = spawn_indexer(resp_tx.clone());
    std::thread::spawn(move || {
        while let Ok(request) = req_rx.recv() {
            // Sending streams many responses, so it cannot go through
            // `handle`, which returns exactly one.
            if let Request::Send(session, message) = request {
                let mut failed = None;
                let result = asm_core::live::send_cancellable(
                    &session,
                    &message,
                    &worker_cancel,
                    &mut |event| {
                        let _ = resp_tx.send(Response::Live(event));
                    },
                );
                if let Err(e) = result {
                    failed = Some(e.to_string());
                }
                if let Some(error) = failed {
                    let _ = resp_tx
                        .send(Response::Live(asm_core::live::LiveEvent::Done {
                            ok: false,
                            error: Some(error),
                        }));
                }
                continue;
            }
            // Scanning streams: a machine with thousands of sessions
            // shows the first of them while the rest are still being read.
            if let Request::Scan = request {
                let tx = resp_tx.clone();
                let mut found = Vec::new();
                let problems = ops::stream_sessions(&SessionFilter::default(), |_, batch| {
                    found.extend(batch.iter().cloned());
                    // A closed channel means the TUI has gone; stop reading.
                    tx.send(Response::SessionBatch(batch)).is_ok()
                });
                if resp_tx.send(Response::ScanDone(problems)).is_err() {
                    break;
                }
                // Store health is cheap and worth knowing without being
                // asked for: duplicate ids and stale locks are exactly what
                // a browser should surface. Both it and the projects are
                // read off the sessions just listed — listing them again
                // for each cost as much as the scan did, and everything
                // asked of the worker in the meantime waited behind them.
                if resp_tx.send(Response::Projects(projects_of(&found))).is_err()
                    || resp_tx.send(health_of(&found)).is_err()
                {
                    break;
                }
                continue;
            }
            let response = handle(request);
            if resp_tx.send(response).is_err() {
                break;
            }
        }
    });
    Worker { tx: req_tx, index_tx, rx: resp_rx, cancel }
}

fn handle(request: Request) -> Response {
    match request {
        Request::Scan => unreachable!("streamed in the worker loop"),
        Request::LoadPreview(session) => match ops::export_ir(&session) {
            Ok(ir) => Response::Preview(session.handle.native_id.clone(), render_preview(&ir)),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::Rename(session, title) => match ops::rename(&session, &title) {
            Ok(()) => Response::Done(format!("renamed {}", session.short_id())),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::Archive(session) => match ops::archive(&session) {
            Ok(_) => Response::Done(format!("archived {}", session.short_id())),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::Unarchive(session) => {
            let query = session.handle.native_id.clone();
            match ops::unarchive(&query, &SessionFilter::default()) {
                Ok(_) => Response::Done(format!("unarchived {}", session.short_id())),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::Delete(session) => match ops::delete(&session) {
            Ok(report) => Response::Done(format!(
                "deleted {} (backup: {}){}",
                session.short_id(),
                report
                    .backup_dir
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "none".into()),
                report.note.map(|n| format!(". {n}")).unwrap_or_default()
            )),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::Send(..) => unreachable!("streamed in the worker loop"),
        Request::Bulk(sessions, action) => {
            let verb = action.verb().to_string();
            Response::Bulk(verb, asm_core::bulk::run(&sessions, &action))
        }
        Request::Hub(sessions) => {
            Response::Hub(Box::new(asm_core::hub::actions::hub_status(&sessions)), asm_core::hub::daemon::status().ok())
        }
        Request::Pull(agent, id, dir) => pull_one(agent, id, dir.as_deref()),
        Request::Search(query) => match run_search(&query) {
            Ok(hits) => Response::Hits(query, hits),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::Export(session, path) => match export_to(&session, &path) {
            Ok(count) => {
                Response::Done(format!("exported {count} messages to {}", path.display()))
            }
            Err(e) => Response::Error(e.to_string()),
        },
        Request::Move(session, dir) => match ops::relocate(&session, &dir) {
            Ok(outcome) => {
                let warnings = if outcome.warnings.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", outcome.warnings.join("; "))
                };
                Response::Done(format!(
                    "moved {} to {}{warnings}",
                    session.short_id(),
                    dir.display()
                ))
            }
            Err(e) => Response::Error(e.to_string()),
        },
        Request::Import(session, target) => {
            let opts = asm_core::import::ImportOpts {
                mode: asm_core::import::ImportMode::Full,
                project: None,
                dry_run: false,
            };
            match ops::import(&session, target, &opts) {
                Ok(outcome) if outcome.in_sync => Response::Done(format!(
                    "already in sync as {}",
                    outcome.target.map(|t| t.native_id).unwrap_or_default()
                )),
                Ok(outcome) => Response::Done(format!(
                    "imported as {} — {}",
                    outcome.target.map(|t| t.native_id).unwrap_or_default(),
                    outcome.resume_hint.unwrap_or_default()
                )),
                Err(e) => Response::Error(e.to_string()),
            }
        }
    }
}

/// A thread that does nothing but keep the search index current, reporting
/// how far it has got. Searching does not wait for it: what is indexed is
/// searchable, and the rest arrives as it is read.
fn spawn_indexer(tx: Sender<Response>) -> Sender<()> {
    let (index_tx, index_rx) = channel::<()>();
    std::thread::spawn(move || {
        while index_rx.recv().is_ok() {
            // Whatever piled up while the last refresh ran is one refresh.
            while index_rx.try_recv().is_ok() {}
            let mut index = match asm_core::index::Index::open() {
                Ok(index) => index,
                Err(e) => {
                    let _ = tx.send(Response::IndexFailed(e.to_string()));
                    continue;
                }
            };
            let report = index.refresh(|progress| {
                let _ = tx.send(Response::Indexing(progress));
            });
            let _ = match report {
                Ok(report) => tx.send(Response::Indexed(report.reindexed, report.failed.len())),
                Err(e) => tx.send(Response::IndexFailed(e.to_string())),
            };
        }
    });
    index_tx
}

/// How long a repository (or a list of its worktrees) is remembered. A
/// rescan follows every action, and each one used to fork `git` again for
/// every project directory; a worktree made in another terminal is seen
/// within this long, or at once on `R`.
const GIT_MEMORY: std::time::Duration = std::time::Duration::from_secs(30);

/// What git said about a directory, and when.
struct Remembered<V> {
    ttl: std::time::Duration,
    entries: std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, (std::time::Instant, V)>>,
}

impl<V: Clone> Remembered<V> {
    fn new(ttl: std::time::Duration) -> Self {
        Remembered { ttl, entries: Default::default() }
    }

    /// The answer for `dir`, asking only if none is remembered. The lock is
    /// not held while asking: that is a process spawn.
    fn get(&self, dir: &std::path::Path, ask: impl FnOnce() -> V) -> V {
        if let Some((at, value)) = self.entries.lock().unwrap().get(dir)
            && at.elapsed() < self.ttl
        {
            return value.clone();
        }
        let value = ask();
        self.entries
            .lock()
            .unwrap()
            .insert(dir.to_path_buf(), (std::time::Instant::now(), value.clone()));
        value
    }

    fn forget_all(&self) {
        self.entries.lock().unwrap().clear();
    }
}

static REPOS: std::sync::LazyLock<Remembered<Option<asm_core::git::Repo>>> =
    std::sync::LazyLock::new(|| Remembered::new(GIT_MEMORY));
static WORKTREES: std::sync::LazyLock<Remembered<Vec<asm_core::git::Worktree>>> =
    std::sync::LazyLock::new(|| Remembered::new(GIT_MEMORY));

/// Ask git again about everything: for a rescan the user asked for.
pub fn forget_git() {
    REPOS.forget_all();
    WORKTREES.forget_all();
}

fn projects_of(sessions: &[Session]) -> Vec<asm_core::model::Project> {
    ops::group_projects(
        sessions,
        |dir| REPOS.get(dir, || asm_core::git::repo_of(dir)),
        |root| WORKTREES.get(root, || asm_core::git::worktrees(root).unwrap_or_default()),
    )
}

/// The health overlay's lines for the stores as they stand, with the
/// number of warnings among them. Session counts are the scan's own.
fn health_of(sessions: &[Session]) -> Response {
    let counted = |adapter: &asm_core::adapter::Adapter| {
        use asm_core::adapter::AgentRead;
        Ok(sessions.iter().filter(|s| s.handle.agent == adapter.kind()).count())
    };
    match ops::doctor_with(counted) {
        Ok(report) => {
            let mut lines = Vec::new();
            let mut warnings = 0;
            for agent in &report.agents {
                lines.push(format!(
                    "{}: {} — {} sessions",
                    agent.agent,
                    if agent.store_found { "store ok" } else { "store missing" },
                    agent.session_count
                ));
                lines.push(format!("  {}", agent.store_root.display()));
                for warning in &agent.warnings {
                    warnings += 1;
                    lines.push(format!("  ⚠ {warning}"));
                }
            }
            if lines.is_empty() {
                lines.push("No agent stores detected.".to_string());
            }
            Response::Doctor(lines, warnings)
        }
        Err(e) => Response::Error(e.to_string()),
    }
}

/// Search reads what the indexer has written so far; it never refreshes,
/// because waiting for a whole store to be read is what the index thread
/// exists to avoid.
fn run_search(query: &str) -> anyhow::Result<Vec<asm_core::index::SearchHit>> {
    use asm_core::index::{Index, SearchQuery};
    let index = Index::open()?;
    Ok(index.search(&SearchQuery { text: query.to_string(), limit: 200, ..Default::default() })?)
}

fn export_to(session: &Session, path: &std::path::Path) -> anyhow::Result<usize> {
    let ir = ops::export_ir(session)?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&ir)?)?;
    Ok(ir.messages.len())
}

/// Cap the preview to the transcript tail — the TUI renders a window, not
/// a 28 MB file.
const MAX_PREVIEW_MESSAGES: usize = 200;
const MAX_TEXT_CHARS: usize = 4_000;

fn render_preview(ir: &asm_core::ir::IrSession) -> Vec<PreviewLine> {
    let mut lines = Vec::new();
    let total = ir.messages.len();
    let skip = total.saturating_sub(MAX_PREVIEW_MESSAGES);
    if skip > 0 {
        lines.push(PreviewLine {
            kind: PreviewKind::Meta,
            text: format!("[... {skip} earlier messages not shown ...]"),
        });
    }
    for message in ir.messages.iter().skip(skip) {
        let role = match message.role {
            IrRole::User => "user",
            IrRole::Assistant => "assistant",
            IrRole::System => "system",
        };
        let when = message
            .timestamp
            .map(|t| t.to_string())
            .unwrap_or_default();
        lines.push(PreviewLine { kind: PreviewKind::Role, text: format!("● {role}  {when}") });
        for part in &message.parts {
            match part {
                IrPart::Text { text } => {
                    for l in clip(text, MAX_TEXT_CHARS).lines() {
                        lines.push(PreviewLine { kind: PreviewKind::Text, text: l.to_string() });
                    }
                }
                IrPart::Reasoning { summary, .. } => {
                    // Signed thinking blocks can carry no readable text; the
                    // part still means the model reasoned here.
                    let first = summary.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                    let text = if first.is_empty() {
                        "(thinking, not recorded)".to_string()
                    } else {
                        format!("(thinking) {}", clip(first, 200))
                    };
                    lines.push(PreviewLine { kind: PreviewKind::Meta, text });
                }
                IrPart::ToolCall { name, input, .. } => {
                    lines.push(PreviewLine {
                        kind: PreviewKind::Tool,
                        text: format!("⚙ {name} {}", clip(&input.to_string(), 200)),
                    });
                }
                IrPart::ToolResult { output, is_error, .. } => {
                    let status = if *is_error { "error" } else { "ok" };
                    lines.push(PreviewLine {
                        kind: PreviewKind::Tool,
                        text: format!("  ↳ [{status}] {}", clip(output.lines().next().unwrap_or(""), 200)),
                    });
                }
                IrPart::File { path, .. } => {
                    lines.push(PreviewLine {
                        kind: PreviewKind::Meta,
                        text: format!(
                            "📎 {}",
                            path.as_ref().map(|p| p.0.as_str()).unwrap_or("(file)")
                        ),
                    });
                }
                IrPart::Agent { name, transcript, .. } => {
                    lines.push(PreviewLine {
                        kind: PreviewKind::Meta,
                        text: format!("↳ subagent {name} ({} turns)", transcript.len()),
                    });
                }
                IrPart::Unknown => {}
            }
        }
        lines.push(PreviewLine { kind: PreviewKind::Text, text: String::new() });
    }
    lines
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

fn pull_one(agent: asm_core::model::AgentKind, id: String, dir: Option<&std::path::Path>) -> Response {
    use asm_core::hub::{actions, bundle::InstallOutcome, client};
    let pulled = client::load().and_then(|remote| actions::pull(&remote, &format!("{agent}:{id}"), dir));
    match pulled {
        Ok(p) => Response::Done(match p.installed.outcome {
            InstallOutcome::Diverged => format!(
                "{} was continued both here and on {}; nothing changed",
                p.id,
                p.from.as_deref().unwrap_or("the hub")
            ),
            InstallOutcome::InSync => format!("{} is already in sync with the hub", p.id),
            outcome => format!(
                "pulled {} into {} ({})",
                p.id,
                p.installed.project_root.display(),
                match outcome {
                    InstallOutcome::New => "new",
                    InstallOutcome::FastForward { .. } => "updated",
                    InstallOutcome::Replaced => "replaced, backed up",
                    InstallOutcome::Renamed => "renamed",
                    _ => "ahead",
                }
            ),
        }),
        Err(e) => {
            let message = e.to_string();
            let needs_dir = message.contains("--project-dir");
            Response::PullFailed { agent, id, message, needs_dir }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::path::Path;
    use std::time::Duration;

    /// Asked twice, git is asked once: the answer is what a scan used to
    /// fork a process for, for every project directory.
    #[test]
    fn a_remembered_answer_is_not_asked_for_again() {
        let memory = Remembered::<u32>::new(Duration::from_secs(60));
        let asked = Cell::new(0);
        let ask = |answer| {
            asked.set(asked.get() + 1);
            answer
        };
        assert_eq!(memory.get(Path::new("/a"), || ask(1)), 1);
        assert_eq!(memory.get(Path::new("/a"), || ask(2)), 1, "the first answer stands");
        assert_eq!(memory.get(Path::new("/b"), || ask(3)), 3, "per directory");
        assert_eq!(asked.get(), 2);

        // Forgotten on request (a rescan the user asked for).
        memory.forget_all();
        assert_eq!(memory.get(Path::new("/a"), || ask(4)), 4);
        assert_eq!(asked.get(), 3);
    }

    #[test]
    fn an_old_answer_is_asked_for_again() {
        let memory = Remembered::<u32>::new(Duration::ZERO);
        assert_eq!(memory.get(Path::new("/a"), || 1), 1);
        assert_eq!(memory.get(Path::new("/a"), || 2), 2, "worktrees made since are seen");
    }
}
