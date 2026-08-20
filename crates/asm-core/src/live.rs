//! Sending a new message into an existing session and streaming the reply.
//!
//! Every agent asm supports already ships a headless "resume this session,
//! send this prompt, print JSON events" entry point. Using those, rather
//! than a daemon socket or a private HTTP API, is the same trade the import
//! path makes with `opencode import`: the agent's own sanctioned surface,
//! versioned with the agent, with no discovery problem to solve.
//!
//! Four properties this module depends on, each verified against a real
//! install rather than assumed:
//!
//! - **The send appends; it does not fork.** For Claude Code, OpenCode,
//!   Codex and Antigravity, resuming by id and sending a prompt continues
//!   the same session — same native id, same transcript, no new row. A
//!   frontend that forked instead would silently strand every reply in a
//!   session the user is not looking at.
//! - **The driver lives here, not in the frontends.** Spawning, streaming,
//!   cancelling and reaping a child process is one job with one correct
//!   answer, and the TUI and the web UI would otherwise each get their own
//!   half-right version.
//! - **Unrecognized lines survive as [`LiveEvent::Raw`].** These formats
//!   will change. A dropped line reads to the user as "the agent said
//!   nothing", which is the worst possible way to learn about a format
//!   change.
//! - **Exactly one [`LiveEvent::Done`] reaches the caller, and it is the
//!   last event.** Adapters do report in-band failures as `Done` — a turn
//!   can fail while the process exits 0, which is what OpenCode's `error`
//!   line and Claude's `is_error` result are — so the driver intercepts
//!   those, keeps the reason, and folds it into its own single terminal
//!   event. Otherwise every frontend has to decide for itself whether the
//!   first or the last `Done` wins, and they will not all decide the same
//!   way.
//! - **Live sessions are refused.** Sending into a session a human is
//!   actively driving in another terminal is the one case none of these
//!   CLIs promise to handle.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::CoreError;
use crate::adapter::{AgentRead, Adapter};
use crate::ir::IrRole;
use crate::model::{Session, SessionStatus, Usage};

/// One normalized step of a reply, whatever agent produced it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LiveEvent {
    /// The agent accepted the turn. `session_id` is what it says it is
    /// working on — worth surfacing, because it disagreeing with the
    /// session we asked for is how a silent fork would look.
    Started { session_id: Option<String> },
    Text { role: IrRole, text: String },
    Reasoning { text: String },
    ToolCall { name: String, detail: Option<String> },
    ToolResult { name: Option<String>, output: String, is_error: bool },
    Usage(Usage),
    Done { ok: bool, error: Option<String> },
    /// A line this asm does not recognize, kept verbatim.
    Raw { line: String },
}

/// Send-side adapter surface.
///
/// Deliberately separate from [`crate::adapter::AgentWrite`], whose every
/// method refuses a session that is not idle and backs up before touching
/// anything. Sending is the opposite shape of operation — it asks the agent
/// to change its own store, through its own code — and folding it in would
/// mean fighting that invariant on every call.
pub trait AgentLive: AgentRead {
    /// Headless command that resumes `session` and sends `message`, writing
    /// newline-delimited JSON events to stdout. `None` when this agent has
    /// no id-addressable resume.
    fn send_command(&self, session: &Session, message: &str) -> Option<Command>;

    /// Normalize one stdout line. Returning an empty vec drops the line;
    /// return [`LiveEvent::Raw`] instead whenever a line might have meant
    /// something.
    fn parse_event(&self, line: &str) -> Vec<LiveEvent>;
}

impl AgentLive for Adapter {
    fn send_command(&self, session: &Session, message: &str) -> Option<Command> {
        match self {
            Adapter::ClaudeCode(a) => a.send_command(session, message),
            Adapter::OpenCode(a) => a.send_command(session, message),
            Adapter::JCode(a) => a.send_command(session, message),
            Adapter::Codex(a) => a.send_command(session, message),
            Adapter::Antigravity(a) => a.send_command(session, message),
        }
    }

    fn parse_event(&self, line: &str) -> Vec<LiveEvent> {
        match self {
            Adapter::ClaudeCode(a) => a.parse_event(line),
            Adapter::OpenCode(a) => a.parse_event(line),
            Adapter::JCode(a) => a.parse_event(line),
            Adapter::Codex(a) => a.parse_event(line),
            Adapter::Antigravity(a) => a.parse_event(line),
        }
    }
}

/// Folds the adapter's events into what the caller sees.
///
/// Its whole job is the single-terminal-`Done` invariant: an adapter may
/// report an in-band failure as `Done`, and the driver owns the one
/// terminal event, so those are absorbed here and surfaced once at the end.
#[derive(Default)]
struct Funnel {
    in_band_error: Option<String>,
}

impl Funnel {
    /// `Some(event)` to pass on, `None` to absorb.
    fn absorb(&mut self, event: LiveEvent) -> Option<LiveEvent> {
        match event {
            LiveEvent::Done { ok: false, error } => {
                // First reason wins: it is the one nearest the cause, and
                // later lines are usually fallout from it.
                if self.in_band_error.is_none() {
                    self.in_band_error =
                        Some(error.unwrap_or_else(|| "the turn failed".to_string()));
                }
                None
            }
            // No adapter reports success in-band today, and if one starts,
            // the driver still owns the terminal event.
            LiveEvent::Done { ok: true, .. } => None,
            other => Some(other),
        }
    }

    /// The single terminal event, given how the process actually ended.
    fn finish(self, cancelled: bool, exited_cleanly: bool, stderr: &str) -> LiveEvent {
        if cancelled {
            return LiveEvent::Done { ok: false, error: Some("cancelled".to_string()) };
        }
        // The process may well have exited 0: `opencode run` and
        // `claude -p` both do when the *invocation* worked and the *turn*
        // did not. The turn is what the caller asked about.
        if let Some(error) = self.in_band_error {
            return LiveEvent::Done { ok: false, error: Some(error) };
        }
        if exited_cleanly {
            return LiveEvent::Done { ok: true, error: None };
        }
        // The agent's own diagnostics beat an exit code, when it wrote any.
        let detail = stderr.trim();
        LiveEvent::Done {
            ok: false,
            error: Some(if detail.is_empty() {
                "the agent exited without completing the turn".to_string()
            } else {
                detail.to_string()
            }),
        }
    }
}

/// Why a send was refused before any process started.
fn precheck(session: &Session, adapter: &Adapter) -> Result<(), CoreError> {
    if !adapter.capabilities().send_message {
        return Err(CoreError::NotSupported {
            agent: session.handle.agent.as_str(),
            op: "send a message into a session",
        });
    }
    if let SessionStatus::Live { pid } = session.status {
        return Err(CoreError::SessionLive { id: session.handle.native_id.clone(), pid });
    }
    if session.status == SessionStatus::Archived {
        return Err(CoreError::Invalid {
            msg: format!(
                "session {} is archived; unarchive it before sending",
                session.handle.native_id
            ),
        });
    }
    Ok(())
}

/// Never cancelled. Used by [`send`], which has no cancellation channel.
static NEVER: AtomicBool = AtomicBool::new(false);

/// Send `message` into `session`, calling `on_event` for each step of the
/// reply as it arrives.
pub fn send(
    session: &Session,
    message: &str,
    on_event: &mut dyn FnMut(LiveEvent),
) -> Result<(), CoreError> {
    send_cancellable(session, message, &NEVER, on_event)
}

/// As [`send`], but stops and kills the child when `cancel` flips true.
///
/// The flag is checked once per output line rather than on a timer: a agent
/// that has gone quiet is usually thinking, and killing it mid-thought
/// loses the turn.
pub fn send_cancellable(
    session: &Session,
    message: &str,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(LiveEvent),
) -> Result<(), CoreError> {
    let adapter = crate::ops::adapter_for(session.handle.agent).ok_or(CoreError::NotSupported {
        agent: session.handle.agent.as_str(),
        op: "send (agent store not found)",
    })?;
    precheck(session, &adapter)?;

    let mut command = adapter.send_command(session, message).ok_or(CoreError::NotSupported {
        agent: session.handle.agent.as_str(),
        op: "send a message into a session",
    })?;
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|e| CoreError::Invalid {
        msg: format!("could not start {}: {e}", session.handle.agent),
    })?;

    // stderr is drained on its own thread. Left unread it fills the pipe
    // buffer and the child blocks writing to it, which looks exactly like
    // an agent that hung.
    let stderr = child.stderr.take();
    let stderr_thread = stderr.map(|stderr| {
        std::thread::spawn(move || {
            let mut captured = String::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if captured.len() < 8 * 1024 {
                    captured.push_str(&line);
                    captured.push('\n');
                }
            }
            captured
        })
    });

    let mut cancelled = false;
    let mut funnel = Funnel::default();
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines() {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                let _ = child.kill();
                break;
            }
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            for event in adapter.parse_event(&line) {
                if let Some(event) = funnel.absorb(event) {
                    on_event(event);
                }
            }
        }
    }

    let status = reap(&mut child);
    let stderr = stderr_thread.and_then(|t| t.join().ok()).unwrap_or_default();

    let event = funnel.finish(cancelled, status.is_some_and(|ok| ok), &stderr);
    on_event(event);
    Ok(())
}

/// `Some(true)` on a clean exit, `Some(false)` on a failure, `None` when the
/// child could not be waited for at all.
fn reap(child: &mut Child) -> Option<bool> {
    child.wait().ok().map(|status| status.success())
}

/// Helpers shared by the per-agent parsers.
pub(crate) mod parse {
    use super::LiveEvent;
    use serde_json::Value;

    /// Parse a line as JSON, or hand it back as [`LiveEvent::Raw`].
    ///
    /// Every one of these CLIs also prints the occasional non-JSON line —
    /// an update notice, a warning — onto the same stream.
    pub(crate) fn json_or_raw(line: &str) -> Result<Value, Vec<LiveEvent>> {
        serde_json::from_str::<Value>(line)
            .map_err(|_| vec![LiveEvent::Raw { line: line.to_string() }])
    }

    pub(crate) fn str_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
        let mut cursor = value;
        for key in path {
            cursor = cursor.get(key)?;
        }
        cursor.as_str().filter(|s| !s.is_empty())
    }

    pub(crate) fn u64_at(value: &Value, path: &[&str]) -> Option<u64> {
        let mut cursor = value;
        for key in path {
            cursor = cursor.get(key)?;
        }
        cursor.as_u64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant every frontend is built on: one terminal `Done`, last.
    ///
    /// Without it the CLI, the web UI and the TUI each have to decide
    /// whether the first or the last `Done` wins — and the TUI, which
    /// assigns its status line per event, would show "reply complete" for
    /// a turn that failed.
    #[test]
    fn an_in_band_failure_becomes_the_one_terminal_event() {
        let mut funnel = Funnel::default();
        // OpenCode's `{"type":"error"}` and Claude's `is_error` result both
        // arrive as this, and both processes then exit 0.
        assert_eq!(
            funnel.absorb(LiveEvent::Done {
                ok: false,
                error: Some("model not approved for this key".into())
            }),
            None,
            "an adapter's Done must not reach the caller"
        );
        let terminal = funnel.finish(false, true, "");
        assert_eq!(
            terminal,
            LiveEvent::Done {
                ok: false,
                error: Some("model not approved for this key".into())
            },
            "a clean exit must not turn a failed turn into a success"
        );
    }

    #[test]
    fn the_first_reason_wins_and_later_ones_are_fallout() {
        let mut funnel = Funnel::default();
        funnel.absorb(LiveEvent::Done { ok: false, error: Some("quota exhausted".into()) });
        funnel.absorb(LiveEvent::Done { ok: false, error: Some("stream closed".into()) });
        assert_eq!(
            funnel.finish(false, false, "some stderr noise"),
            LiveEvent::Done { ok: false, error: Some("quota exhausted".into()) }
        );
    }

    #[test]
    fn cancelling_outranks_everything_else() {
        let mut funnel = Funnel::default();
        funnel.absorb(LiveEvent::Done { ok: false, error: Some("stream closed".into()) });
        assert_eq!(
            funnel.finish(true, false, "broken pipe"),
            LiveEvent::Done { ok: false, error: Some("cancelled".into()) }
        );
    }

    #[test]
    fn a_clean_run_reports_success_and_passes_content_through() {
        let mut funnel = Funnel::default();
        let text = LiveEvent::Text { role: crate::ir::IrRole::Assistant, text: "hi".into() };
        assert_eq!(funnel.absorb(text.clone()), Some(text));
        assert_eq!(funnel.finish(false, true, ""), LiveEvent::Done { ok: true, error: None });
    }

    /// A non-zero exit with nothing on stdout is the common shape of a
    /// crash; the agent's own stderr is far more useful than the code.
    #[test]
    fn stderr_explains_a_failed_exit() {
        let funnel = Funnel::default();
        assert_eq!(
            funnel.finish(false, false, "  No credentials found. Let's log in!\n"),
            LiveEvent::Done {
                ok: false,
                error: Some("No credentials found. Let's log in!".into())
            }
        );
    }

    #[test]
    fn a_non_json_line_survives_as_raw() {
        let events = super::parse::json_or_raw("Update available: 1.2.3").unwrap_err();
        assert_eq!(events, vec![LiveEvent::Raw { line: "Update available: 1.2.3".into() }]);
    }
}
