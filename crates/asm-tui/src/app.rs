//! App state and event loop. Panel-local state stays here; anything slow
//! goes through the worker.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

use asm_core::bulk::{BulkAction, BulkReport};
use asm_core::model::{AgentKind, Session, SessionStatus};
use asm_core::ops;

use crate::worker::{PreviewKind, PreviewLine, Request, Response, Worker};

pub enum LoopOutcome {
    Quit,
    /// Leave the TUI, run this interactively, come back.
    RunCommand(std::process::Command),
}

#[derive(Debug, PartialEq)]
pub enum Mode {
    Normal,
    Filter,
    Rename,
    ConfirmDelete,
    /// Input: file path to write the IR export to.
    Export,
    /// Input: destination project directory.
    Move,
    /// Confirm import into the other agent.
    ConfirmImport,
    /// Input: a full-text query across every transcript.
    Search,
    /// Confirm a verb over the whole selection.
    ConfirmBulk,
    /// Input for a bulk verb that needs one: a destination directory.
    BulkInput,
    /// Composing a message to send into the selected session.
    Send,
    /// Choosing agents or a project to narrow the list to.
    Picker,
}

#[derive(PartialEq, Clone, Copy)]
pub enum PickerKind {
    Agent,
    Project,
}

/// A list to pick from, over the session list. Agents toggle (several at
/// once); a project is one choice, typed at to narrow.
pub struct Picker {
    pub kind: PickerKind,
    pub cursor: usize,
    pub query: String,
}

/// One row of a picker: what it stands for, how many sessions it has, and
/// whether it is part of the filter now.
pub struct PickerRow {
    pub label: String,
    pub count: usize,
    pub on: bool,
    pub agent: Option<AgentKind>,
}

pub struct App {
    worker: Worker,
    pub sessions: Vec<Session>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub mode: Mode,
    pub input: String,
    pub filter: String,
    pub status: String,
    pub preview_for: Option<String>,
    pub preview: Vec<PreviewLine>,
    pub preview_scroll: u16,
    pub preview_focus: bool,
    pub scanning: bool,
    /// Full-text results; while present they replace the session list.
    pub hits: Option<Vec<asm_core::index::SearchHit>>,
    pub hit_selected: usize,
    pub searched_for: String,
    /// The session a prompt or confirmation is about, captured when that
    /// mode was entered. A background rescan can reorder the list between
    /// the keystroke that opens a confirmation and the one that answers it,
    /// so re-reading the selection would act on the wrong session.
    pending: Option<Session>,
    /// Sessions ticked for a bulk action, keyed by identity rather than by
    /// row: a background rescan reorders and can drop rows, and an index
    /// would then point at a different session than the one ticked.
    pub selection: HashSet<(AgentKind, String)>,
    /// The selection resolved to real sessions, taken when the action was
    /// confirmed. Anything that vanished in between is dropped here, once,
    /// rather than failing later with a confusing error.
    pending_batch: Vec<Session>,
    pending_action: Option<BulkAction>,
    /// The last batch's per-item outcome, shown as an overlay when
    /// anything did not simply work.
    pub report: Option<(String, BulkReport)>,
    /// True while a reply is streaming. The worker is occupied for the
    /// whole turn, so nothing else can be asked of it until this clears.
    pub sending: bool,
    /// The session the reply in flight belongs to, so a turn that finishes
    /// after the user has moved on does not reload someone else's preview.
    sending_for: Option<Session>,
    /// Which agents will accept a message at all, resolved once at startup
    /// rather than per keystroke.
    can_send: HashMap<AgentKind, bool>,
    /// Store-health report, shown as an overlay on `D`.
    pub doctor: Vec<String>,
    pub doctor_warnings: usize,
    pub show_doctor: bool,
    /// The transcript pane. Closed until asked for: the list is what the
    /// browser is for, and a full-width list shows more of every session.
    pub preview_open: bool,
    pub help_open: bool,
    pub help_scroll: u16,
    /// Agents the list is narrowed to; empty means all of them.
    pub agents: HashSet<AgentKind>,
    /// The project the list is narrowed to, with its descendants.
    pub project: Option<PathBuf>,
    pub picker: Option<Picker>,
    /// A rescan's sessions, held until it finishes: the first scan streams
    /// straight into the list, but replacing rows under the cursor while
    /// the user is reading them is worse than waiting a moment.
    incoming: Vec<Session>,
    /// Stores that could not be read on the last scan.
    pub problems: Vec<String>,
    /// Whether this scan's batches go straight on screen.
    streaming: bool,
    /// A scan asked for while one was running.
    rescan_pending: bool,
    /// Set by anything that has something to say, so the scan that follows
    /// does not overwrite it with a session count.
    status_sticky: bool,
    /// How far the background index has got, while it is running.
    pub indexing: Option<asm_core::index::RefreshProgress>,
    /// The filter as it was when `/` was pressed, so esc can restore it.
    filter_before: String,
    /// Projects as the CLI and the web UI mean them: a repository and its
    /// worktrees, not one directory per session.
    projects: Vec<asm_core::model::Project>,
}

impl App {
    pub fn new(worker: Worker) -> Self {
        App {
            worker,
            sessions: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            mode: Mode::Normal,
            input: String::new(),
            filter: String::new(),
            status: "loading sessions…".to_string(),
            preview_for: None,
            preview: Vec::new(),
            preview_scroll: 0,
            preview_focus: false,
            scanning: false,
            hits: None,
            hit_selected: 0,
            searched_for: String::new(),
            pending: None,
            selection: HashSet::new(),
            pending_batch: Vec::new(),
            pending_action: None,
            report: None,
            sending: false,
            sending_for: None,
            can_send: asm_core::adapter::Adapter::available()
                .iter()
                .map(|a| {
                    use asm_core::adapter::AgentRead;
                    (a.kind(), a.capabilities().send_message)
                })
                .collect(),
            doctor: Vec::new(),
            doctor_warnings: 0,
            show_doctor: false,
            preview_open: false,
            help_open: false,
            help_scroll: 0,
            agents: HashSet::new(),
            project: None,
            picker: None,
            incoming: Vec::new(),
            problems: Vec::new(),
            streaming: true,
            rescan_pending: false,
            status_sticky: false,
            indexing: None,
            filter_before: String::new(),
            projects: Vec::new(),
        }
    }

    pub fn set_status(&mut self, status: String) {
        self.status = status;
    }

    /// A batch of a scan, as the store gave it up.
    fn take_batch(&mut self, batch: Vec<Session>) {
        if self.streaming {
            // Rows arriving must not move the cursor off the session it is
            // on: `a` and `p` act on it with no confirmation.
            let under_cursor = self.selected_session().map(Self::key_of);
            self.sessions.extend(batch);
            self.sessions.sort_by_key(|s| std::cmp::Reverse(s.updated));
            self.apply_filter();
            self.keep_cursor_on(under_cursor);
        } else {
            self.incoming.extend(batch);
        }
    }

    /// Put the cursor back on a session after the list moved under it.
    fn keep_cursor_on(&mut self, key: Option<(AgentKind, String)>) {
        let Some(key) = key else { return };
        if let Some(position) =
            self.filtered.iter().position(|&i| Self::key_of(&self.sessions[i]) == key)
        {
            self.selected = position;
        }
    }

    /// How many sessions there are, and whether more are still coming.
    pub fn found(&self) -> usize {
        self.sessions.len().max(self.incoming.len())
    }

    pub fn request_scan(&mut self) {
        // One scan at a time: a second one would land its batches in the
        // middle of the first's and publish whichever finished last as the
        // whole list. Asking again while one runs queues it instead.
        if self.scanning {
            self.rescan_pending = true;
            return;
        }
        self.scanning = true;
        // With nothing on screen, rows go straight in as they arrive.
        // With a list already up, they are collected and swapped in at the
        // end: replacing rows under the cursor mid-read is worse than a
        // moment's wait.
        self.streaming = self.sessions.is_empty();
        self.incoming.clear();
        let _ = self.worker.tx.send(Request::Scan);
        // Store health is cheap and worth knowing without being asked for:
        // duplicate ids and stale locks are exactly what a browser should
        // surface.
        let _ = self.worker.tx.send(Request::Doctor);
        let _ = self.worker.tx.send(Request::Projects);
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.filtered.get(self.selected).map(|&i| &self.sessions[i])
    }

    /// Advance the cursor, so ticking with space walks down the list the
    /// way holding space in a file manager does.
    fn move_down(&mut self) {
        if self.selected + 1 < self.filtered.len() {
            self.selected += 1;
            self.sync_preview();
        }
    }

    fn key_of(session: &Session) -> (AgentKind, String) {
        (session.handle.agent, session.handle.native_id.clone())
    }

    pub fn is_ticked(&self, session: &Session) -> bool {
        self.selection.contains(&Self::key_of(session))
    }

    fn toggle_tick(&mut self) {
        if let Some(session) = self.selected_session() {
            let key = Self::key_of(session);
            if !self.selection.remove(&key) {
                self.selection.insert(key);
            }
        }
        self.status = self.selection_status();
    }

    /// Tick everything the filter is currently showing — not the whole
    /// store, which would silently include rows the user cannot see.
    fn tick_all_visible(&mut self) {
        let keys: Vec<_> =
            self.filtered.iter().map(|&i| Self::key_of(&self.sessions[i])).collect();
        // A second press on an already-complete selection clears it, so the
        // key is a toggle rather than a one-way door.
        if keys.iter().all(|k| self.selection.contains(k)) {
            self.selection.clear();
        } else {
            self.selection.extend(keys);
        }
        self.status = self.selection_status();
    }

    fn selection_status(&self) -> String {
        match self.selection.len() {
            0 => format!("{} sessions", self.sessions.len()),
            n => format!("{n} selected — a archive · d delete · m move · e export · i import · p push"),
        }
    }

    /// The sessions a bulk action should run over, resolved now. Ticks that
    /// no longer match a session are dropped and counted.
    fn resolve_batch(&self) -> (Vec<Session>, usize) {
        let found: Vec<Session> = self
            .sessions
            .iter()
            .filter(|s| self.selection.contains(&Self::key_of(s)))
            .cloned()
            .collect();
        let vanished = self.selection.len().saturating_sub(found.len());
        (found, vanished)
    }

    /// Begin a bulk action: resolve the ticks, then either confirm it or
    /// ask for the directory it needs.
    fn begin_bulk(&mut self, action: BulkAction, input_default: Option<String>) {
        let (batch, vanished) = self.resolve_batch();
        if batch.is_empty() {
            self.status = "nothing selected is still there".to_string();
            return;
        }
        self.pending_batch = batch;
        self.pending_action = Some(action);
        if let Some(default) = input_default {
            self.input = default;
            self.mode = Mode::BulkInput;
        } else {
            self.mode = Mode::ConfirmBulk;
        }
        if vanished > 0 {
            self.status = format!("{vanished} selected session(s) are gone; acting on the rest");
        }
    }

    fn send_bulk(&mut self) {
        let sessions = std::mem::take(&mut self.pending_batch);
        if let Some(action) = self.pending_action.take() {
            self.status = format!("{} {} session(s)…", action.verb().to_lowercase(), sessions.len());
            let _ = self.worker.tx.send(Request::Bulk(sessions, action));
        }
        self.mode = Mode::Normal;
    }

    /// What a pending bulk confirmation is about, for the prompt.
    pub fn pending_bulk(&self) -> Option<(&BulkAction, usize)> {
        self.pending_action.as_ref().map(|a| (a, self.pending_batch.len()))
    }

    /// Everything but the agents: what a count in the agent picker means.
    fn matches_but_agents(&self, session: &Session) -> bool {
        self.matches_with(session, false, true)
    }

    /// Everything but the project, for the project picker's counts.
    fn matches_but_project(&self, session: &Session) -> bool {
        self.matches_with(session, true, false)
    }

    /// Every filter at once: the agents, the project and the typed text.
    fn matches(&self, session: &Session) -> bool {
        self.matches_with(session, true, true)
    }

    fn matches_with(&self, session: &Session, agents: bool, project: bool) -> bool {
        if agents && !self.agents.is_empty() && !self.agents.contains(&session.handle.agent) {
            return false;
        }
        if project
            && let Some(project) = &self.project
            && !self.in_project(session, project)
        {
            return false;
        }
        let needle = self.filter.to_lowercase();
        needle.is_empty()
            || session.title.as_deref().unwrap_or("").to_lowercase().contains(&needle)
            || session.handle.native_id.to_lowercase().contains(&needle)
            || session.project_root.display().to_string().to_lowercase().contains(&needle)
            || session.handle.agent.to_string().contains(&needle)
    }

    /// Whether a session belongs to the project rooted here: its own
    /// directory, a subdirectory of it, or any other worktree of the same
    /// repository — the same rule `asm projects` groups by.
    fn in_project(&self, session: &Session, project: &PathBuf) -> bool {
        let under = |root: &PathBuf| {
            !root.as_os_str().is_empty()
                && (session.project_root == *root || session.project_root.starts_with(root))
        };
        under(project)
            || self
                .projects
                .iter()
                .find(|p| p.root == *project)
                .is_some_and(|p| p.worktrees.iter().any(|w| under(&w.path)))
    }

    /// What the filters are, in words, for the bar above the status line.
    pub fn active_filters(&self) -> Vec<String> {
        let mut active = Vec::new();
        if !self.agents.is_empty() {
            let mut names: Vec<String> = self.agents.iter().map(|a| a.to_string()).collect();
            names.sort();
            active.push(names.join(", "));
        }
        if let Some(project) = &self.project {
            active.push(crate::ui::shorten(&project.display().to_string(), 40));
        }
        if !self.filter.is_empty() {
            active.push(format!("\"{}\"", self.filter));
        }
        active
    }

    pub fn clear_filters(&mut self) -> bool {
        let had = !self.agents.is_empty() || self.project.is_some() || !self.filter.is_empty();
        self.agents.clear();
        self.project = None;
        self.filter.clear();
        if had {
            self.apply_filter();
            self.status = format!("{} sessions", self.sessions.len());
        }
        had
    }

    /// The rows of the open picker: agents, or projects narrowed by what
    /// has been typed. Counts come from the sessions the OTHER filters
    /// leave, so a count is what picking that row would show.
    pub fn picker_rows(&self) -> Vec<PickerRow> {
        let Some(picker) = &self.picker else { return Vec::new() };
        match picker.kind {
            PickerKind::Agent => {
                // What picking this agent would leave, with the project and
                // the text still applied.
                let left = |agent: AgentKind| {
                    self.sessions
                        .iter()
                        .filter(|s| s.handle.agent == agent && self.matches_but_agents(s))
                        .count()
                };
                let mut agents: Vec<AgentKind> =
                    self.sessions.iter().map(|s| s.handle.agent).collect::<HashSet<_>>().into_iter().collect();
                agents.sort_by_key(|a| a.to_string());
                agents
                    .into_iter()
                    .map(|agent| PickerRow {
                        label: agent.to_string(),
                        count: left(agent),
                        on: self.agents.contains(&agent),
                        agent: Some(agent),
                    })
                    .collect()
            }
            PickerKind::Project => {
                let needle = picker.query.to_lowercase();
                let left = |root: &PathBuf| {
                    self.sessions
                        .iter()
                        .filter(|s| self.in_project(s, root) && self.matches_but_project(s))
                        .count()
                };
                let mut rows: Vec<PickerRow> = self
                    .projects
                    .iter()
                    .map(|p| p.root.clone())
                    .filter(|root| !root.as_os_str().is_empty())
                    .map(|root| {
                        let label = crate::ui::home_relative(&root);
                        (label, root)
                    })
                    // Both what is on screen and the full path: the row
                    // reads `~/code/x`, and `/home` should still find it.
                    .filter(|(label, root)| {
                        label.to_lowercase().contains(&needle)
                            || root.display().to_string().to_lowercase().contains(&needle)
                    })
                    .map(|(label, root)| PickerRow {
                        count: left(&root),
                        on: self.project.as_ref() == Some(&root),
                        label,
                        agent: None,
                    })
                    .collect();
                rows.sort_by(|a, b| a.label.cmp(&b.label));
                rows.insert(
                    0,
                    PickerRow {
                        label: "all projects".into(),
                        count: self.sessions.iter().filter(|s| self.matches_but_project(s)).count(),
                        on: self.project.is_none(),
                        agent: None,
                    },
                );
                rows
            }
        }
    }

    #[cfg(test)]
    pub fn set_projects_for_test(&mut self, projects: Vec<asm_core::model::Project>) {
        self.projects = projects;
    }

    #[cfg(test)]
    pub fn open_picker_for_test(&mut self, kind: PickerKind) {
        self.open_picker(kind);
    }

    fn open_picker(&mut self, kind: PickerKind) {
        self.picker = Some(Picker { kind, cursor: 0, query: String::new() });
        self.mode = Mode::Picker;
    }

    /// Apply the highlighted row. Agents toggle and the picker stays open
    /// for the next one; a project is one choice, so picking closes it.
    fn pick(&mut self) {
        let rows = self.picker_rows();
        let Some(picker) = &self.picker else { return };
        let Some(row) = rows.get(picker.cursor) else { return };
        match picker.kind {
            PickerKind::Agent => {
                if let Some(agent) = row.agent
                    && !self.agents.remove(&agent)
                {
                    self.agents.insert(agent);
                }
            }
            PickerKind::Project => {
                self.project = self
                    .projects
                    .iter()
                    .map(|p| p.root.clone())
                    .find(|root| crate::ui::home_relative(root) == row.label);
                self.picker = None;
                self.mode = Mode::Normal;
            }
        }
        self.apply_filter();
        self.status = match self.active_filters().as_slice() {
            [] => format!("{} sessions", self.sessions.len()),
            filters => format!("{} of {} — {}", self.filtered.len(), self.sessions.len(), filters.join(" · ")),
        };
    }

    fn on_picker_key(&mut self, key: KeyEvent) {
        let len = self.picker_rows().len();
        let Some(picker) = &mut self.picker else { return };
        // A rescan can shorten the rows under an open picker.
        picker.cursor = picker.cursor.min(len.saturating_sub(1));
        let agents = picker.kind == PickerKind::Agent;
        let close = |app: &mut Self| {
            app.picker = None;
            app.mode = Mode::Normal;
        };
        match key.code {
            // Every printable key belongs to the project picker's query, so
            // only the agent picker answers to letters.
            KeyCode::Esc if picker.query.is_empty() => close(self),
            KeyCode::Esc => picker.query.clear(),
            KeyCode::Char('q') if agents => close(self),
            // Each choice applies as it is made; Enter is just "done".
            KeyCode::Enter if agents => close(self),
            KeyCode::Enter => self.pick(),
            KeyCode::Char(' ') if agents => self.pick(),
            KeyCode::Down | KeyCode::Tab => picker.cursor = (picker.cursor + 1).min(len.saturating_sub(1)),
            KeyCode::Up | KeyCode::BackTab => picker.cursor = picker.cursor.saturating_sub(1),
            KeyCode::Char('j') if agents => picker.cursor = (picker.cursor + 1).min(len.saturating_sub(1)),
            KeyCode::Char('k') if agents => picker.cursor = picker.cursor.saturating_sub(1),
            KeyCode::Backspace => {
                picker.query.pop();
                picker.cursor = 0;
            }
            KeyCode::Char(c) if !agents => {
                picker.query.push(c);
                picker.cursor = 0;
            }
            _ => {}
        }
    }

    fn apply_filter(&mut self) {
        self.filtered = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| self.matches(s))
            .map(|(i, _)| i)
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
        self.sync_preview();
    }

    fn sync_preview(&mut self) {
        // Nothing is parsed for a pane nobody opened; opening it calls
        // this again.
        if !self.preview_open {
            self.preview_for = None;
            self.preview.clear();
            return;
        }
        let Some(session) = self.selected_session().cloned() else {
            self.preview_for = None;
            self.preview.clear();
            return;
        };
        if self.preview_for.as_deref() == Some(session.handle.native_id.as_str()) {
            return;
        }
        self.preview_for = Some(session.handle.native_id.clone());
        self.preview.clear();
        self.preview_scroll = 0;
        let _ = self.worker.tx.send(Request::LoadPreview(Box::new(session)));
    }

    /// Load a session's transcript into the preview, even if it is already
    /// the one shown. `sync_preview` short-circuits on an unchanged id,
    /// which is right when moving the cursor and wrong after a reply has
    /// changed the transcript under it.
    fn load_preview(&mut self, session: Session) {
        self.preview_for = Some(session.handle.native_id.clone());
        self.preview.clear();
        self.preview_scroll = 0;
        let _ = self.worker.tx.send(Request::LoadPreview(Box::new(session)));
    }

    /// Whether asm can send into this session's agent at all.
    pub fn can_send(&self, session: &Session) -> bool {
        self.can_send.get(&session.handle.agent).copied().unwrap_or(false)
    }

    /// Start composing a reply to the highlighted session.
    fn begin_send(&mut self) {
        // The worker streams a turn on the one thread it has, so a second
        // send would sit in the queue behind the first and then fire at a
        // session the user may have moved away from.
        if self.sending {
            self.status = "a reply is already in flight (esc stops it)".to_string();
            return;
        }
        let Some(session) = self.selected_session().cloned() else { return };
        if !self.can_send(&session) {
            self.status = format!("asm cannot send into {} sessions", session.handle.agent);
            return;
        }
        // The core refuses a session another process is driving; say so
        // before the message is typed rather than after it is lost.
        if let asm_core::model::SessionStatus::Live { .. } = session.status {
            self.status = format!(
                "{} is live in another {} process; close it to reply from here",
                session.short_id(),
                session.handle.agent
            );
            return;
        }
        // The preview is the conversation being replied to, so open it if
        // the user has not already; typing into a blank pane is disorienting.
        self.preview_open = true;
        if self.preview_for.as_deref() != Some(session.handle.native_id.as_str()) {
            self.load_preview(session.clone());
        }
        self.pending = Some(session);
        self.input.clear();
        self.mode = Mode::Send;
    }

    fn send_pending(&mut self) {
        let Some(session) = self.pending.take() else { return };
        let message = std::mem::take(&mut self.input);
        if message.trim().is_empty() {
            self.status = "nothing to send".to_string();
            return;
        }
        self.worker.cancel.store(false, std::sync::atomic::Ordering::Relaxed);
        self.sending = true;
        self.sending_for = Some(session.clone());
        self.preview_focus = true;
        // `push_live` below needs `sending_for` already set: it refuses to
        // write into a pane showing a different session.
        // Echo the message straight away. The agent takes seconds to
        // minutes to answer and a pane that does not visibly change reads
        // as a keystroke that did not register.
        self.push_live(PreviewKind::Role, "you".to_string());
        self.push_live(PreviewKind::Text, message.clone());
        self.status = format!("sending to {}… (esc stops)", session.handle.agent);
        let _ = self.worker.tx.send(Request::Send(Box::new(session), message));
    }

    /// The native id of the session whose reply is streaming, if the
    /// preview pane is still showing it.
    ///
    /// Moving the cursor during a send repoints the pane at another
    /// session; the worker keeps streaming regardless, and appending to
    /// whatever is on screen would splice one session's reply into
    /// another's transcript.
    fn streaming_into_view(&self) -> bool {
        match (&self.sending_for, &self.preview_for) {
            (Some(session), Some(showing)) => &session.handle.native_id == showing,
            _ => false,
        }
    }

    fn push_live(&mut self, kind: PreviewKind, text: String) {
        if !self.streaming_into_view() {
            return;
        }
        self.preview.push(PreviewLine { kind, text });
        // Follow the tail, the way a terminal does; the user is watching
        // the newest line, not the oldest.
        self.preview_scroll = self.preview.len().saturating_sub(1) as u16;
    }

    fn on_live(&mut self, event: asm_core::live::LiveEvent) {
        use asm_core::ir::IrRole;
        use asm_core::live::LiveEvent;
        match event {
            LiveEvent::Started { .. } | LiveEvent::Usage(_) => {}
            LiveEvent::Text { role, text } => {
                self.push_live(
                    PreviewKind::Role,
                    match role {
                        IrRole::Assistant => "assistant".to_string(),
                        IrRole::User => "user".to_string(),
                        IrRole::System => "system".to_string(),
                    },
                );
                self.push_live(PreviewKind::Text, text);
            }
            LiveEvent::Reasoning { text } => {
                self.push_live(PreviewKind::Meta, first_line(&text))
            }
            LiveEvent::ToolCall { name, detail } => self.push_live(
                PreviewKind::Tool,
                match detail {
                    Some(detail) => format!("{name} {}", first_line(&detail)),
                    None => name,
                },
            ),
            LiveEvent::ToolResult { output, is_error, .. } => self.push_live(
                PreviewKind::Meta,
                format!(
                    "{}: {}",
                    if is_error { "failed" } else { "result" },
                    first_line(&output)
                ),
            ),
            // Kept rather than dropped: these stream formats change, and a
            // swallowed line reads as the agent answering with silence.
            LiveEvent::Raw { line } => self.push_live(PreviewKind::Meta, first_line(&line)),
            LiveEvent::Done { ok, error } => {
                self.sending = false;
                self.status = match (ok, error) {
                    (true, _) => "reply complete".to_string(),
                    (false, Some(error)) => format!("send failed: {error}"),
                    (false, None) => "send failed".to_string(),
                };
                // The turn is only in the store once the agent has written
                // it, so re-read rather than trusting the echo above — but
                // only if that session is still the one on screen.
                let showing = self.streaming_into_view();
                if let Some(session) = self.sending_for.take()
                    && showing
                {
                    self.load_preview(session);
                }
                self.request_scan();
            }
        }
    }

    fn drain_worker(&mut self) {
        while let Ok(response) = self.worker.rx.try_recv() {
            match response {
                Response::SessionBatch(batch) => self.take_batch(batch),
                Response::ScanDone(problems) => {
                    self.scanning = false;
                    let under_cursor = self.selected_session().map(Self::key_of);
                    if !self.streaming {
                        // A rescan's rows, held back so the list did not
                        // shift under the cursor while it was being read.
                        self.sessions = std::mem::take(&mut self.incoming);
                        self.sessions.sort_by_key(|s| std::cmp::Reverse(s.updated));
                        self.preview_for = None;
                        self.apply_filter();
                        self.keep_cursor_on(under_cursor);
                        self.sync_preview();
                    }
                    self.streaming = false;
                    self.problems = problems;
                    // Whatever the last action said stands: a count is not
                    // worth erasing "archived 4c0d9a71" with.
                    if !self.status_sticky {
                        self.status = format!("{} sessions", self.sessions.len());
                    }
                    // Now that the list is up, keep the index current.
                    let _ = self.worker.index_tx.send(());
                    if std::mem::take(&mut self.rescan_pending) {
                        self.request_scan();
                    }
                }
                // Cleared by `Indexed`, not by the last count: the final
                // batch is still on its way into the database.
                Response::Indexing(progress) => self.indexing = Some(progress),
                Response::Indexed(reindexed, failed) => {
                    self.indexing = None;
                    if reindexed > 0 && !self.status_sticky {
                        self.status = format!(
                            "{} sessions — indexed {reindexed}{}",
                            self.sessions.len(),
                            if failed > 0 { format!(", {failed} unreadable") } else { String::new() }
                        );
                    }
                }
                // The index stopped; saying so beats a spinner that never
                // finishes.
                Response::IndexFailed(message) => {
                    self.indexing = None;
                    self.status = format!("search index: {message}");
                    self.status_sticky = true;
                }
                Response::Preview(id, lines) => {
                    if self.preview_for.as_deref() == Some(id.as_str()) {
                        self.preview = lines;
                    }
                }
                Response::Projects(projects) => {
                    self.projects = projects;
                    if let Some(picker) = &mut self.picker {
                        // The rows under the cursor just changed.
                        picker.cursor = 0;
                    }
                }
                Response::Doctor(lines, warnings) => {
                    self.doctor = lines;
                    self.doctor_warnings = warnings;
                }
                Response::Hits(query, hits) => {
                    self.status = format!("{} match(es) for {query:?}", hits.len());
                    self.searched_for = query;
                    self.hit_selected = 0;
                    self.hits = Some(hits);
                    // Searching inside transcripts is the one time the
                    // transcript is what you are looking at, so it opens
                    // itself and follows the highlighted match. Nothing to
                    // show means nothing to open, and a pane the user had
                    // open stays open.
                    self.preview_open |= self.hits.as_ref().is_some_and(|h| !h.is_empty());
                    self.preview_hit();
                }
                Response::Done(message) => {
                    self.status = message;
                    self.status_sticky = true;
                    self.request_scan();
                }
                Response::Bulk(verb, report) => {
                    self.status = report.summary(&verb);
                    self.status_sticky = true;
                    // Only interrupt with the detail when there is detail;
                    // a clean batch just updates the status line.
                    self.report =
                        (report.failed() + report.skipped() > 0).then_some((verb, report));
                    self.selection.clear();
                    self.request_scan();
                }
                Response::Live(event) => self.on_live(event),
                Response::Error(message) => {
                    self.status = format!("error: {message}");
                    self.status_sticky = true;
                    self.scanning = false;
                }
            }
        }
    }

    pub fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> anyhow::Result<LoopOutcome> {
        loop {
            self.drain_worker();
            terminal.draw(|frame| crate::ui::draw(frame, self))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else { continue };
            if !key.is_press() {
                continue;
            }
            match self.mode {
                Mode::Normal => {
                    if let Some(outcome) = self.on_normal_key(key) {
                        return Ok(outcome);
                    }
                }
                Mode::Filter => self.on_filter_key(key),
                Mode::Rename => self.on_rename_key(key),
                Mode::ConfirmDelete => self.on_confirm_key(key),
                Mode::Export => self.on_export_key(key),
                Mode::Move => self.on_move_key(key),
                Mode::ConfirmImport => self.on_confirm_import_key(key),
                Mode::Search => self.on_search_key(key),
                Mode::ConfirmBulk => self.on_confirm_bulk_key(key),
                Mode::BulkInput => self.on_bulk_input_key(key),
                Mode::Send => self.on_send_key(key),
                Mode::Picker => self.on_picker_key(key),
            }
        }
    }

    /// Load the transcript of the highlighted match, so moving through
    /// results reads as moving through conversations.
    fn preview_hit(&mut self) {
        let Some(hits) = &self.hits else { return };
        let Some(hit) = hits.get(self.hit_selected) else { return };
        let (agent, id) = (hit.agent.clone(), hit.native_id.clone());
        let found = self
            .sessions
            .iter()
            .find(|s| s.handle.agent.to_string() == agent && s.handle.native_id == id)
            .cloned();
        if let Some(session) = found
            && self.preview_for.as_deref() != Some(session.handle.native_id.as_str())
        {
            self.load_preview(session);
        }
    }

    /// Move the session cursor onto the session a search hit belongs to.
    fn jump_to_hit(&mut self) {
        let Some(hits) = &self.hits else { return };
        let Some(hit) = hits.get(self.hit_selected) else { return };
        let (target, seq) = ((hit.agent.clone(), hit.native_id.clone()), hit.seq);
        let found = self.filtered.iter().position(|&i| {
            let s = &self.sessions[i];
            s.handle.agent.to_string() == target.0 && s.handle.native_id == target.1
        });
        match found {
            Some(position) => {
                self.selected = position;
                self.hits = None;
                self.sync_preview();
                self.status = format!("jumped to match #{seq}");
            }
            None => {
                // Indexed but not in the current list: a filter is hiding
                // it, or it was archived since the index was written.
                self.status =
                    "that session is not in the current list (clear the filter, or rescan)"
                        .to_string();
            }
        }
    }

    fn on_normal_key(&mut self, key: KeyEvent) -> Option<LoopOutcome> {
        // The last action's message has been read by now.
        self.status_sticky = false;
        // A reply in flight owns Esc: the agent is spending tokens and can
        // be editing files, so stopping it outranks whatever Esc would
        // otherwise back out of.
        if self.sending && key.code == KeyCode::Esc {
            self.worker.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            self.status = "stopping…".to_string();
            return None;
        }
        // Help stays up while it is being read: only the scroll keys do
        // anything else, and anything else at all closes it.
        if self.help_open {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => self.help_scroll = self.help_scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => self.help_scroll = self.help_scroll.saturating_sub(1),
                _ => self.help_open = false,
            }
            return None;
        }
        if key.code == KeyCode::Char('?') {
            self.help_open = true;
            self.help_scroll = 0;
            return None;
        }
        // The health overlay swallows the next key, whatever it is.
        if self.show_doctor {
            self.show_doctor = false;
            return None;
        }
        if key.code == KeyCode::Char('D') {
            self.show_doctor = true;
            return None;
        }
        // While results are showing, the list keys drive the results.
        if self.hits.is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.hits = None;
                    self.status = "back to sessions".to_string();
                    // The pane was following the matches; put it back on
                    // the row the cursor is on.
                    self.preview_for = None;
                    self.sync_preview();
                    return None;
                }
                // The transcript keys keep working while results are up:
                // the pane it opened is the point of the search.
                KeyCode::Right | KeyCode::Char('l') => {
                    self.preview_open = true;
                    self.preview_for = None;
                    self.preview_hit();
                    return None;
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    self.preview_open = false;
                    self.preview_focus = false;
                    return None;
                }
                KeyCode::Tab => {
                    self.preview_focus = !self.preview_focus && self.preview_open;
                    return None;
                }
                KeyCode::PageDown => {
                    self.preview_scroll = self.preview_scroll.saturating_add(20);
                    return None;
                }
                KeyCode::PageUp => {
                    self.preview_scroll = self.preview_scroll.saturating_sub(20);
                    return None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let len = self.hits.as_ref().map_or(0, Vec::len);
                    if self.hit_selected + 1 < len {
                        self.hit_selected += 1;
                        self.preview_hit();
                    }
                    return None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.hit_selected > 0 {
                        self.hit_selected -= 1;
                        self.preview_hit();
                    }
                    return None;
                }
                KeyCode::Enter => {
                    self.jump_to_hit();
                    return None;
                }
                KeyCode::Char('/') => {
                    self.mode = Mode::Search;
                    self.input = self.searched_for.clone();
                    return None;
                }
                _ => return None,
            }
        }
        match key.code {
            KeyCode::Char('q') => return Some(LoopOutcome::Quit),
            // Esc backs out of things in the order they were put up, and
            // only quits when there is nothing left to back out of.
            // Esc backs out one layer at a time — the overlay, then the
            // selection, then the filters — and only then leaves.
            KeyCode::Esc => {
                if self.report.is_some() {
                    self.report = None;
                } else if !self.selection.is_empty() {
                    self.selection.clear();
                    self.status = self.selection_status();
                } else if self.preview_open {
                    self.preview_open = false;
                    self.preview_focus = false;
                } else if !self.clear_filters() {
                    return Some(LoopOutcome::Quit);
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.preview_open = true;
                self.sync_preview();
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.preview_open = false;
                self.preview_focus = false;
            }
            KeyCode::Char('A') => self.open_picker(PickerKind::Agent),
            KeyCode::Char('P') => self.open_picker(PickerKind::Project),
            KeyCode::Char(' ') => {
                self.toggle_tick();
                self.move_down();
            }
            KeyCode::Char('*') => self.tick_all_visible(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Some(LoopOutcome::Quit);
            }
            // Tab reaches for the transcript, opening it if it is closed.
            KeyCode::Tab => {
                if self.preview_open {
                    self.preview_focus = !self.preview_focus;
                } else {
                    self.preview_open = true;
                    self.preview_focus = true;
                    self.sync_preview();
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.preview_focus {
                    self.preview_scroll = self.preview_scroll.saturating_add(1);
                } else if self.selected + 1 < self.filtered.len() {
                    self.selected += 1;
                    self.sync_preview();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.preview_focus {
                    self.preview_scroll = self.preview_scroll.saturating_sub(1);
                } else if self.selected > 0 {
                    self.selected -= 1;
                    self.sync_preview();
                }
            }
            KeyCode::PageDown if self.preview_open => {
                self.preview_scroll = self.preview_scroll.saturating_add(20)
            }
            KeyCode::PageUp if self.preview_open => {
                self.preview_scroll = self.preview_scroll.saturating_sub(20)
            }
            KeyCode::Char('G') | KeyCode::End => {
                if !self.preview_focus && !self.filtered.is_empty() {
                    self.selected = self.filtered.len() - 1;
                    self.sync_preview();
                }
            }
            KeyCode::Char('g') | KeyCode::Home => {
                if !self.preview_focus {
                    self.selected = 0;
                    self.sync_preview();
                }
            }
            KeyCode::Char('/') => {
                self.mode = Mode::Filter;
                self.input = self.filter.clone();
                self.filter_before = self.filter.clone();
            }
            KeyCode::Char('R') => self.request_scan(),
            KeyCode::Enter => {
                if let Some(session) = self.selected_session() {
                    match ops::resume_command(session) {
                        Ok(command) => return Some(LoopOutcome::RunCommand(command)),
                        Err(e) => self.status = format!("error: {e}"),
                    }
                }
            }
            // `c` composes a reply — `s` is already search, and Enter in a
            // list means "open". This one spends money, so it gets a key of
            // its own rather than overloading an existing one.
            KeyCode::Char('c') => self.begin_send(),
            // Push to the hub. Nothing here is overwritten, so one session
            // goes straight away; a batch is confirmed like the others.
            KeyCode::Char('p') => {
                if !self.selection.is_empty() {
                    self.begin_bulk(BulkAction::Push, None);
                } else if let Some(session) = self.selected_session().cloned() {
                    self.status = "pushing…".to_string();
                    let _ = self.worker.tx.send(Request::Bulk(vec![session], BulkAction::Push));
                }
            }
            KeyCode::Char('r') => {
                if let Some(session) = self.selected_session().cloned() {
                    self.input = session.title.clone().unwrap_or_default();
                    self.pending = Some(session);
                    self.mode = Mode::Rename;
                }
            }
            KeyCode::Char('a') => {
                if !self.selection.is_empty() {
                    // Archive and unarchive are one key on one session, so
                    // a mixed batch has to pick: the majority state wins,
                    // and the confirmation says which.
                    let (batch, _) = self.resolve_batch();
                    let archived = batch
                        .iter()
                        .filter(|s| s.status == SessionStatus::Archived)
                        .count();
                    let action = if archived * 2 > batch.len() {
                        BulkAction::Unarchive
                    } else {
                        BulkAction::Archive
                    };
                    self.begin_bulk(action, None);
                } else if let Some(session) = self.selected_session() {
                    let request = if session.status == SessionStatus::Archived {
                        Request::Unarchive(Box::new(session.clone()))
                    } else {
                        Request::Archive(Box::new(session.clone()))
                    };
                    self.status = "working…".to_string();
                    let _ = self.worker.tx.send(request);
                }
            }
            KeyCode::Char('d') => {
                if !self.selection.is_empty() {
                    self.begin_bulk(BulkAction::Delete, None);
                } else if let Some(session) = self.selected_session().cloned() {
                    self.pending = Some(session);
                    self.mode = Mode::ConfirmDelete;
                }
            }
            KeyCode::Char('e') => {
                if !self.selection.is_empty() {
                    // A batch writes one file each, so it needs a directory
                    // rather than the single-session file path.
                    let dir = std::env::current_dir()
                        .map(|d| d.display().to_string())
                        .unwrap_or_default();
                    self.begin_bulk(BulkAction::Export { dir: Default::default() }, Some(dir));
                } else if let Some(session) = self.selected_session().cloned() {
                    self.input = format!("{}.ir.json", session.short_id());
                    self.pending = Some(session);
                    self.mode = Mode::Export;
                }
            }
            KeyCode::Char('m') => {
                if !self.selection.is_empty() {
                    let default = self
                        .selected_session()
                        .map(|s| s.project_root.display().to_string())
                        .unwrap_or_default();
                    self.begin_bulk(BulkAction::Move { dir: Default::default() }, Some(default));
                } else if let Some(session) = self.selected_session().cloned() {
                    self.input = session.project_root.display().to_string();
                    self.pending = Some(session);
                    self.mode = Mode::Move;
                }
            }
            KeyCode::Char('i') => {
                if !self.selection.is_empty() {
                    // One destination for the batch: whichever agent the
                    // majority of the selection is not already in.
                    let (batch, _) = self.resolve_batch();
                    let claude = batch
                        .iter()
                        .filter(|s| s.handle.agent == AgentKind::ClaudeCode)
                        .count();
                    let to = if claude * 2 > batch.len() {
                        AgentKind::OpenCode
                    } else {
                        AgentKind::ClaudeCode
                    };
                    self.begin_bulk(BulkAction::Import { to }, None);
                } else if let Some(session) = self.selected_session().cloned() {
                    self.pending = Some(session);
                    self.mode = Mode::ConfirmImport;
                }
            }
            KeyCode::Char('s') => {
                self.mode = Mode::Search;
                self.input = self.searched_for.clone();
            }
            _ => {}
        }
        None
    }

    fn on_confirm_bulk_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => self.send_bulk(),
            _ => {
                self.pending_batch.clear();
                self.pending_action = None;
                self.mode = Mode::Normal;
            }
        }
    }

    fn on_bulk_input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.pending_batch.clear();
                self.pending_action = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                if self.input.trim().is_empty() {
                    return;
                }
                let dir = std::path::PathBuf::from(self.input.trim());
                self.pending_action = match self.pending_action.take() {
                    Some(BulkAction::Move { .. }) => Some(BulkAction::Move { dir }),
                    Some(BulkAction::Export { .. }) => Some(BulkAction::Export { dir }),
                    other => other,
                };
                self.send_bulk();
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    fn on_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.pending = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                let query = self.input.trim().to_string();
                self.mode = Mode::Normal;
                if query.is_empty() {
                    self.hits = None;
                    return;
                }
                self.status = "searching transcripts…".to_string();
                let _ = self.worker.tx.send(Request::Search(query));
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    /// The agent an import would target. Reads the captured session while a
    /// confirmation is open, so the prompt and the action cannot disagree.
    pub fn import_target(&self) -> Option<asm_core::model::AgentKind> {
        use asm_core::model::AgentKind;
        self.pending
            .as_ref()
            .or_else(|| self.selected_session())
            .map(|s| match s.handle.agent {
                AgentKind::ClaudeCode => AgentKind::OpenCode,
                _ => AgentKind::ClaudeCode,
            })
    }

    /// The session a prompt is about, for display.
    pub fn pending_session(&self) -> Option<&Session> {
        self.pending.as_ref().or_else(|| self.selected_session())
    }

    fn on_export_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.pending = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                if let Some(session) = self.pending.take()
                    && !self.input.is_empty()
                {
                    let path = std::path::PathBuf::from(self.input.clone());
                    let _ = self.worker.tx.send(Request::Export(Box::new(session), path));
                    self.status = "exporting…".to_string();
                }
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    fn on_move_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.pending = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                if let Some(session) = self.pending.take()
                    && !self.input.is_empty()
                {
                    let dir = std::path::PathBuf::from(self.input.clone());
                    let _ = self.worker.tx.send(Request::Move(Box::new(session), dir));
                    self.status = "moving…".to_string();
                }
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    fn on_confirm_import_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                // Target is derived from the captured session, not the
                // selection, which may have moved under a rescan.
                if let (Some(target), Some(session)) = (self.import_target(), self.pending.take()) {
                    let _ = self.worker.tx.send(Request::Import(Box::new(session), target));
                    self.status = "importing…".to_string();
                }
                self.mode = Mode::Normal;
            }
            _ => {
                self.pending = None;
                self.mode = Mode::Normal;
            }
        }
    }

    fn on_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            // It filters as it is typed, so cancelling has to put back what
            // was there when it opened.
            KeyCode::Esc => {
                self.filter = std::mem::take(&mut self.filter_before);
                self.mode = Mode::Normal;
                self.apply_filter();
            }
            KeyCode::Enter => {
                self.filter = self.input.clone();
                self.mode = Mode::Normal;
                self.apply_filter();
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.filter = self.input.clone();
                self.apply_filter();
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.filter = self.input.clone();
                self.apply_filter();
            }
            _ => {}
        }
    }

    fn on_send_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.pending = None;
                self.input.clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                self.send_pending();
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    fn on_rename_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.pending = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                if let Some(session) = self.pending.take() {
                    let _ = self
                        .worker
                        .tx
                        .send(Request::Rename(Box::new(session), self.input.clone()));
                    self.status = "renaming…".to_string();
                }
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    fn on_confirm_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(session) = self.pending.take() {
                    let _ = self.worker.tx.send(Request::Delete(Box::new(session)));
                    self.status = "deleting…".to_string();
                }
                self.mode = Mode::Normal;
            }
            _ => {
                self.pending = None;
                self.mode = Mode::Normal;
            }
        }
    }
}

/// The first line with something on it, clipped. Streamed tool output and
/// reasoning run to hundreds of lines and the preview is one pane.
fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() > 200 {
        format!("{}…", line.chars().take(200).collect::<String>())
    } else {
        line.to_string()
    }
}

#[cfg(test)]
pub(crate) fn test_app(sessions: Vec<Session>) -> App {
    let (tx, _rx) = std::sync::mpsc::channel();
    let (_tx, rx) = std::sync::mpsc::channel();
    let worker = crate::worker::Worker {
        tx,
        index_tx: std::sync::mpsc::channel().0,
        rx,
        cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let mut app = App::new(worker);
    app.sessions = sessions;
    app.filtered = (0..app.sessions.len()).collect();
    app
}

#[cfg(test)]
mod tests {
    use super::*;
    use asm_core::model::{SessionLocation, SessionRef, Usage};

    fn session(agent: AgentKind, id: &str, project: &str) -> Session {
        Session {
            handle: SessionRef {
                agent,
                native_id: id.into(),
                location: SessionLocation::JsonlFile { path: format!("/tmp/{id}.jsonl").into() },
            },
            title: Some(format!("about {id}")),
            slug: None,
            project_root: project.into(),
            git_branch: None,
            created: None,
            updated: None,
            model: None,
            usage: Usage::default(),
            status: SessionStatus::Idle,
            parent: None,
            agent_version: None,
            size_bytes: Some(10),
        }
    }

    fn project(root: &str) -> asm_core::model::Project {
        asm_core::model::Project {
            root: root.into(),
            repo: None,
            agents: Vec::new(),
            worktrees: vec![asm_core::model::ProjectWorktree {
                path: root.into(),
                branch: None,
                is_main: true,
                session_count: 1,
            }],
            session_count: 1,
            size_bytes: 0,
            last_updated: None,
        }
    }

    fn app() -> App {
        let mut app = test_app(vec![
            session(AgentKind::ClaudeCode, "aaa", "/w/mercury"),
            session(AgentKind::ClaudeCode, "bbb", "/w/atlas"),
            session(AgentKind::OpenCode, "ccc", "/w/mercury"),
            // A session whose agent never recorded a directory.
            session(AgentKind::Antigravity, "ddd", ""),
        ]);
        app.projects = vec![project("/w/mercury"), project("/w/atlas")];
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        match app.mode {
            Mode::Picker => app.on_picker_key(KeyEvent::from(code)),
            Mode::Filter => app.on_filter_key(KeyEvent::from(code)),
            _ => {
                app.on_normal_key(KeyEvent::from(code));
            }
        }
    }

    /// Each toggle applies as it is made, so the key that closes the picker
    /// must not also toggle the row under the cursor.
    #[test]
    fn closing_the_agent_picker_keeps_what_was_chosen() {
        let mut app = app();
        press(&mut app, KeyCode::Char('A'));
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.agents.len(), 1, "space chose one");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.agents.len(), 1, "enter closed it, and changed nothing");

        // And a bare enter chooses nothing at all.
        let mut app = super::tests::app();
        press(&mut app, KeyCode::Char('A'));
        press(&mut app, KeyCode::Enter);
        assert!(app.agents.is_empty(), "no filter the user did not ask for");
    }

    /// The project picker is typed into, so letters belong to the query.
    #[test]
    fn every_letter_reaches_the_project_query() {
        let mut app = app();
        press(&mut app, KeyCode::Char('P'));
        for c in ['q', ' ', 'k'] {
            press(&mut app, KeyCode::Char(c));
        }
        assert_eq!(app.mode, Mode::Picker, "none of those closed it");
        assert_eq!(app.picker.as_ref().unwrap().query, "q k");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.picker.as_ref().unwrap().query, "", "first esc clears the query");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Normal, "the second closes");
    }

    /// A count says what picking that row would leave, so it respects the
    /// filters already on.
    #[test]
    fn picker_counts_respect_the_other_filters() {
        let mut app = app();
        app.project = Some("/w/mercury".into());
        app.apply_filter();
        app.open_picker(PickerKind::Agent);
        let rows = app.picker_rows();
        let claude = rows.iter().find(|r| r.label == "claude-code").unwrap();
        assert_eq!(claude.count, 1, "one claude session in mercury, not two");

        // And a project row counts what the agent filter would leave.
        let mut app = super::tests::app();
        app.agents.insert(AgentKind::OpenCode);
        app.apply_filter();
        app.open_picker(PickerKind::Project);
        let rows = app.picker_rows();
        let mercury = rows.iter().find(|r| r.label.ends_with("mercury")).unwrap();
        assert_eq!(mercury.count, 1);
        assert_eq!(rows[0].label, "all projects");
        assert_eq!(rows[0].count, 1, "one opencode session in all");
    }

    /// A session with no directory is no project; offering it as one gave a
    /// nameless row whose filter hid nothing.
    #[test]
    fn a_session_without_a_directory_is_not_a_project() {
        let mut app = app();
        app.open_picker(PickerKind::Project);
        let rows = app.picker_rows();
        assert!(rows.iter().all(|r| !r.label.is_empty()), "no nameless rows");
        assert_eq!(rows.len(), 3, "all projects, mercury, atlas");
    }

    #[test]
    fn a_search_with_no_matches_does_not_open_an_empty_transcript() {
        let mut app = app();
        app.hits = Some(Vec::new());
        app.preview_open |= app.hits.as_ref().is_some_and(|h| !h.is_empty());
        assert!(!app.preview_open);
    }

    /// It filters as it is typed, so cancelling has to put back what was
    /// there before.
    #[test]
    fn esc_in_the_filter_restores_what_it_replaced() {
        let mut app = app();
        app.filter = "mercury".into();
        app.apply_filter();
        let before = app.filtered.len();
        press(&mut app, KeyCode::Char('/'));
        for c in ['a', 't'] {
            press(&mut app, KeyCode::Char(c));
        }
        assert_ne!(app.filtered.len(), before, "typing filters as it goes");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.filter, "mercury");
        assert_eq!(app.filtered.len(), before);
    }

    /// Rows arriving during a scan must not move the cursor off what the
    /// user is pointing at, and a scan asked for mid-scan must not publish
    /// half a list.
    #[test]
    fn a_scan_keeps_the_cursor_and_queues_a_rescan() {
        let mut app = test_app(Vec::new());
        app.scanning = true;
        app.take_batch(vec![session(AgentKind::ClaudeCode, "old", "/w/mercury")]);
        assert_eq!(app.selected_session().map(|s| s.handle.native_id.clone()), Some("old".into()));

        // A newer session arrives and sorts above it; the cursor follows
        // the session, not the row.
        let mut newer = session(AgentKind::OpenCode, "new", "/w/atlas");
        newer.updated = Some("2030-01-01T00:00:00Z".parse().unwrap());
        app.take_batch(vec![newer]);
        assert_eq!(app.sessions.len(), 2);
        assert_eq!(app.selected_session().map(|s| s.handle.native_id.clone()), Some("old".into()));

        // Asking for a scan while one runs waits for it.
        app.request_scan();
        assert!(app.rescan_pending, "queued, not started under the running one");
        assert!(app.scanning);
    }

    /// Esc peels back one layer at a time, and only leaves when there is
    /// nothing left to back out of.
    #[test]
    fn esc_backs_out_before_it_quits() {
        let mut app = app();
        app.agents.insert(AgentKind::ClaudeCode);
        app.preview_open = true;
        app.selection.insert((AgentKind::ClaudeCode, "aaa".into()));
        assert!(app.on_normal_key(KeyEvent::from(KeyCode::Esc)).is_none());
        assert!(app.selection.is_empty());
        assert!(app.on_normal_key(KeyEvent::from(KeyCode::Esc)).is_none());
        assert!(!app.preview_open, "then the transcript");
        assert!(app.on_normal_key(KeyEvent::from(KeyCode::Esc)).is_none());
        assert!(app.agents.is_empty(), "then the filters");
        assert!(matches!(app.on_normal_key(KeyEvent::from(KeyCode::Esc)), Some(LoopOutcome::Quit)));
    }
}
