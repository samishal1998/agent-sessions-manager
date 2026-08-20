//! Background worker: everything slower than a frame runs here.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender, channel};

use asm_core::adapter::SessionFilter;
use asm_core::ir::{IrPart, IrRole};
use asm_core::model::Session;
use asm_core::ops;

pub enum Request {
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
    Doctor,
    /// One verb over a selected set. Sessions are resolved by the caller at
    /// the moment the action is confirmed, never by index.
    Bulk(Vec<Session>, asm_core::bulk::BulkAction),
    /// Send a message into a session. Unlike every other request this one
    /// answers with a stream of `Response::Live` rather than a single
    /// response, and it occupies the worker until the agent is finished.
    Send(Box<Session>, String),
}

pub enum Response {
    Sessions(Vec<Session>),
    /// (session native id, rendered transcript lines)
    Preview(String, Vec<PreviewLine>),
    /// (query, hits)
    Hits(String, Vec<asm_core::index::SearchHit>),
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
            let response = handle(request);
            if resp_tx.send(response).is_err() {
                break;
            }
        }
    });
    Worker { tx: req_tx, rx: resp_rx, cancel }
}

fn handle(request: Request) -> Response {
    match request {
        Request::Scan => match ops::list_sessions(&SessionFilter::default()) {
            Ok(sessions) => Response::Sessions(sessions),
            Err(e) => Response::Error(e.to_string()),
        },
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
                "deleted {} (backup: {})",
                session.short_id(),
                report
                    .backup_dir
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "none".into())
            )),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::Send(..) => unreachable!("streamed in the worker loop"),
        Request::Bulk(sessions, action) => {
            let verb = action.verb().to_string();
            Response::Bulk(verb, asm_core::bulk::run(&sessions, &action))
        }
        Request::Doctor => match ops::doctor() {
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
        },
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

/// Search runs on the worker thread, so the incremental index refresh it
/// does first never blocks a frame.
fn run_search(query: &str) -> anyhow::Result<Vec<asm_core::index::SearchHit>> {
    use asm_core::index::{Index, SearchQuery};
    let mut index = Index::open()?;
    index.refresh(|_| {})?;
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
