//! App state and event loop. Panel-local state stays here; anything slow
//! goes through the worker.

use std::collections::HashSet;
use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

use asm_core::bulk::{BulkAction, BulkReport};
use asm_core::model::{AgentKind, Session, SessionStatus};
use asm_core::ops;

use crate::worker::{PreviewLine, Request, Response, Worker};

pub enum LoopOutcome {
    Quit,
    /// Leave the TUI, run this interactively, come back.
    RunCommand(std::process::Command),
}

#[derive(PartialEq)]
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
    /// Store-health report, shown as an overlay on `D`.
    pub doctor: Vec<String>,
    pub doctor_warnings: usize,
    pub show_doctor: bool,
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
            doctor: Vec::new(),
            doctor_warnings: 0,
            show_doctor: false,
        }
    }

    pub fn set_status(&mut self, status: String) {
        self.status = status;
    }

    pub fn request_scan(&mut self) {
        self.scanning = true;
        let _ = self.worker.tx.send(Request::Scan);
        // Store health is cheap and worth knowing without being asked for:
        // duplicate ids and stale locks are exactly what a browser should
        // surface.
        let _ = self.worker.tx.send(Request::Doctor);
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
            n => format!("{n} selected — a archive · d delete · m move · e export · i import"),
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

    fn apply_filter(&mut self) {
        let needle = self.filter.to_lowercase();
        self.filtered = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                if needle.is_empty() {
                    return true;
                }
                s.title.as_deref().unwrap_or("").to_lowercase().contains(&needle)
                    || s.handle.native_id.to_lowercase().contains(&needle)
                    || s.project_root.display().to_string().to_lowercase().contains(&needle)
                    || s.handle.agent.to_string().contains(&needle)
            })
            .map(|(i, _)| i)
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
        self.sync_preview();
    }

    fn sync_preview(&mut self) {
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

    fn drain_worker(&mut self) {
        while let Ok(response) = self.worker.rx.try_recv() {
            match response {
                Response::Sessions(sessions) => {
                    self.scanning = false;
                    self.sessions = sessions;
                    self.status = format!("{} sessions", self.sessions.len());
                    self.preview_for = None;
                    self.apply_filter();
                }
                Response::Preview(id, lines) => {
                    if self.preview_for.as_deref() == Some(id.as_str()) {
                        self.preview = lines;
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
                }
                Response::Done(message) => {
                    self.status = message;
                    self.request_scan();
                }
                Response::Bulk(verb, report) => {
                    self.status = report.summary(&verb);
                    // Only interrupt with the detail when there is detail;
                    // a clean batch just updates the status line.
                    self.report =
                        (report.failed() + report.skipped() > 0).then_some((verb, report));
                    self.selection.clear();
                    self.request_scan();
                }
                Response::Error(message) => {
                    self.status = format!("error: {message}");
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
            }
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
                    return None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let len = self.hits.as_ref().map_or(0, Vec::len);
                    if self.hit_selected + 1 < len {
                        self.hit_selected += 1;
                    }
                    return None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.hit_selected = self.hit_selected.saturating_sub(1);
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
            KeyCode::Esc => {
                if self.report.is_some() {
                    self.report = None;
                } else if !self.selection.is_empty() {
                    self.selection.clear();
                    self.status = self.selection_status();
                } else {
                    return Some(LoopOutcome::Quit);
                }
            }
            KeyCode::Char(' ') => {
                self.toggle_tick();
                self.move_down();
            }
            KeyCode::Char('*') => self.tick_all_visible(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Some(LoopOutcome::Quit);
            }
            KeyCode::Tab => self.preview_focus = !self.preview_focus,
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
            KeyCode::PageDown => self.preview_scroll = self.preview_scroll.saturating_add(20),
            KeyCode::PageUp => self.preview_scroll = self.preview_scroll.saturating_sub(20),
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
            KeyCode::Esc => {
                self.mode = Mode::Normal;
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
