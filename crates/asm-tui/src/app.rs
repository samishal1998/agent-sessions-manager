//! App state and event loop. Panel-local state stays here; anything slow
//! goes through the worker.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

use asm_core::bulk::{BulkAction, BulkReport};
use asm_core::hub::actions::{HubStatus, Row, RowState, SyncAction};
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
    /// Where to put a pulled session whose own directory is not here.
    PullDir,
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

/// What the list shows of one row besides its session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Place {
    /// How far under its parent the row sits (0 for a root).
    pub depth: u16,
    /// Sessions under this one, at any depth, shown or not.
    pub below: usize,
    /// Whether they are shown.
    pub open: bool,
}

pub struct App {
    worker: Worker,
    /// Every session, children too: the list is grouped from these.
    pub sessions: Vec<Session>,
    /// The rows of the list, as positions in `sessions`, in display order.
    pub filtered: Vec<usize>,
    /// A `Place` for each of `filtered`.
    pub places: Vec<Place>,
    /// Subagent sessions shown as a tree under their parent (`T`) rather
    /// than hidden behind it.
    pub tree: bool,
    /// Sessions whose children are shown the other way from `tree`: opened
    /// when subagents are hidden, folded when they are shown.
    toggled: HashSet<(AgentKind, String)>,
    /// Where each of `sessions` has its parent, if that is in `sessions`.
    parent_ix: Vec<Option<usize>>,
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
    /// Sessions a confirmed delete takes along that are not themselves in
    /// it, counted when it was asked for.
    pending_below: usize,
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
    /// `.` (or `asm --here`): the checkout asm runs in, and with
    /// `all_worktrees` every checkout of its repository. Exclusive with
    /// `project`: setting one clears the other.
    pub here: Option<asm_core::ops::Here>,
    pub all_worktrees: bool,
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
    /// What the user asked the worker for and has not heard back about, in
    /// the order asked. The worker is serial, so the front is the one
    /// running and the rest wait behind it (and behind any scan).
    pub in_flight: VecDeque<String>,
    /// How far the background index has got, while it is running.
    pub indexing: Option<asm_core::index::RefreshProgress>,
    /// How this machine stood with the hub at the last check. `None` until
    /// the first one answers; `joined: false` when there is no hub at all.
    pub hub: Option<HubStatus>,
    pub hub_checking: bool,
    /// The hub stopped answering, so `hub` is the last picture it gave.
    pub hub_stale: bool,
    /// The daemon on this machine, as it last reported (a local file, so it
    /// is never stale the way the hub's answer can be).
    pub daemon: Option<asm_core::hub::daemon::DaemonStatus>,
    pub hub_checked: Option<std::time::Instant>,
    /// The hub view: every session here and there, with what to do about it.
    pub hub_view: bool,
    pub hub_cursor: usize,
    /// Hide what is already in sync, to see only what wants doing.
    pub hub_only_needing: bool,
    hub_states: HashMap<(AgentKind, String), RowState>,
    pending_pull: Option<(AgentKind, String)>,
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
            places: Vec::new(),
            tree: false,
            toggled: HashSet::new(),
            parent_ix: Vec::new(),
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
            pending_below: 0,
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
            here: None,
            all_worktrees: false,
            picker: None,
            incoming: Vec::new(),
            problems: Vec::new(),
            streaming: true,
            rescan_pending: false,
            status_sticky: false,
            in_flight: VecDeque::new(),
            indexing: None,
            hub: None,
            hub_checking: false,
            hub_stale: false,
            daemon: None,
            hub_checked: None,
            hub_view: false,
            hub_cursor: 0,
            hub_only_needing: false,
            hub_states: HashMap::new(),
            pending_pull: None,
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

    /// Ask the hub how things stand. One question at a time: a hub that is
    /// slow to answer must not queue a second behind the first.
    pub fn request_hub(&mut self) {
        if self.hub_checking {
            return;
        }
        self.hub_checking = true;
        // What the hub compares is what `asm push` would send: a subagent
        // travels in its parent's bundle, and listed on its own it would be
        // a "Local" row nobody could push.
        let roots = self.sessions.iter().filter(|s| s.parent.is_none()).cloned().collect();
        let _ = self.worker.tx.send(Request::Hub(roots));
    }

    pub fn hub_joined(&self) -> bool {
        self.hub.as_ref().is_some_and(|h| h.joined)
    }

    /// Whether the hub has said anything about any session yet.
    pub fn has_sync_states(&self) -> bool {
        !self.hub_states.is_empty()
    }

    /// Where a session stands with the hub, if the hub has said.
    pub fn sync_of(&self, session: &Session) -> Option<RowState> {
        self.hub_states.get(&Self::key_of(session)).copied()
    }

    fn take_hub(&mut self, status: HubStatus) {
        self.hub_checking = false;
        if !status.joined && self.hub_view {
            self.hub_view = false;
            self.status =
                "this machine has not joined a hub: ASM_JOIN_TOKEN=<token> asm join <url>".to_string();
            self.status_sticky = true;
        }
        let had_rows = self.hub.as_ref().is_some_and(|h| !h.rows.is_empty());
        if status.joined && !status.connected && had_rows {
            // A hub that stops answering must not wipe what was known: keep
            // the last picture and say it is old.
            let prev = self.hub.take().unwrap_or_default();
            self.hub = Some(HubStatus { connected: false, error: status.error, detail: status.detail, ..prev });
            self.hub_stale = true;
            return;
        }
        self.hub_stale = false;
        if status.connected {
            self.hub_checked = Some(std::time::Instant::now());
        }
        self.hub_states = status
            .rows
            .iter()
            .map(|r| ((r.agent, r.id.clone()), r.state))
            .collect();
        self.hub = Some(status);
        let rows = self.hub_rows().len();
        self.hub_cursor = self.hub_cursor.min(rows.saturating_sub(1));
    }

    /// The hub view's rows: what most needs a decision first, what is already
    /// level last.
    pub fn hub_rows(&self) -> Vec<&Row> {
        let priority = |s: RowState| match s {
            RowState::Diverged => 0,
            RowState::Behind => 1,
            RowState::Remote => 2,
            RowState::Untracked => 3,
            RowState::Ahead => 4,
            RowState::Local => 5,
            RowState::InSync => 6,
        };
        let mut rows: Vec<&Row> = self
            .hub
            .iter()
            .flat_map(|h| h.rows.iter())
            .filter(|r| !self.hub_only_needing || r.action.is_some())
            .collect();
        rows.sort_by_key(|r| priority(r.state));
        rows
    }

    fn open_hub_view(&mut self) {
        // Before the first answer there is nothing to refuse yet: open the
        // view and let it say it is asking, rather than send the next key to
        // the list (where `r` is rename).
        let asking = self.hub.is_none() && self.hub_checking;
        if !self.hub_joined() && !asking {
            self.status =
                "this machine has not joined a hub: ASM_JOIN_TOKEN=<token> asm join <url>".to_string();
            self.status_sticky = true;
            return;
        }
        self.hub_view = true;
        self.hub_cursor = 0;
        self.request_hub();
    }

    /// Do what the highlighted hub row needs. `only` restricts it to one
    /// action (`p` pushes, and only pushes).
    fn hub_act(&mut self, only: Option<SyncAction>) {
        let Some(row) = self.hub_rows().get(self.hub_cursor).map(|r| (*r).clone()) else { return };
        self.status_sticky = true;
        if self.hub.as_ref().is_some_and(|h| !h.connected) {
            self.status = "the hub cannot be reached — r tries again".to_string();
            return;
        }
        if let Some(only) = only
            && row.action != Some(only)
        {
            self.status = format!("{}: {}", row.label, row.hint);
            return;
        }
        match row.action {
            None => self.status = "already in sync with the hub".to_string(),
            Some(SyncAction::Resolve) => self.status = row.hint.to_string(),
            Some(SyncAction::Push) => {
                let key = (row.agent, row.id.clone());
                match self.sessions.iter().find(|s| Self::key_of(s) == key).cloned() {
                    Some(session) => {
                        self.dispatch(format!("pushing {}…", row.short_id), Request::Bulk(vec![session], BulkAction::Push));
                    }
                    None => self.status = "that session is not on this machine".to_string(),
                }
            }
            Some(SyncAction::Pull) if !row.restorable => {
                self.status = format!("{} sessions are backed up on the hub but cannot be restored yet", row.agent);
            }
            Some(SyncAction::Pull) => {
                self.dispatch(format!("pulling {}…", row.short_id), Request::Pull(row.agent, row.id, None));
            }
        }
    }

    fn on_hub_view_key(&mut self, key: KeyEvent) -> Option<LoopOutcome> {
        let len = self.hub_rows().len();
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Some(LoopOutcome::Quit);
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('H') => self.hub_view = false,
            KeyCode::Down | KeyCode::Char('j') => self.hub_cursor = (self.hub_cursor + 1).min(len.saturating_sub(1)),
            KeyCode::Up | KeyCode::Char('k') => self.hub_cursor = self.hub_cursor.saturating_sub(1),
            KeyCode::Char('g') | KeyCode::Home => self.hub_cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.hub_cursor = len.saturating_sub(1),
            KeyCode::Char('f') => {
                self.hub_only_needing = !self.hub_only_needing;
                self.hub_cursor = 0;
            }
            KeyCode::Char('r') => self.request_hub(),
            KeyCode::Enter => self.hub_act(None),
            KeyCode::Char('p') => self.hub_act(Some(SyncAction::Push)),
            _ => {}
        }
        None
    }

    fn on_pull_dir_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.pending_pull = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                let dir = self.input.trim().to_string();
                if let Some((agent, id)) = self.pending_pull.take()
                    && !dir.is_empty()
                {
                    self.dispatch("pulling…".to_string(), Request::Pull(agent, id, Some(dir.into())));
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    /// How many sessions there are, and whether more are still coming.
    pub fn found(&self) -> usize {
        self.sessions.len().max(self.incoming.len())
    }

    /// Hand the worker something the user asked for. It may wait behind a
    /// scan or a hub check, so what it says stays up — over the scan's
    /// count — until its own answer comes back.
    fn dispatch(&mut self, label: String, request: Request) {
        self.status = label.clone();
        self.status_sticky = true;
        self.in_flight.push_back(label);
        let _ = self.worker.tx.send(request);
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
        // The projects and the stores' health come back with it.
        let _ = self.worker.tx.send(Request::Scan);
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
            0 => self.count_status(),
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
        self.pending_below = if matches!(action, BulkAction::Delete) { self.deleted_with(&batch) } else { 0 };
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
            let label = format!("{} {} session(s)…", action.verb().to_lowercase(), sessions.len());
            self.dispatch(label, Request::Bulk(sessions, action));
        }
        self.mode = Mode::Normal;
    }

    /// Sessions that deleting `batch` takes along without being in it: the
    /// ones under a session of an agent that deletes its whole tree.
    fn deleted_with(&self, batch: &[Session]) -> usize {
        let named: HashSet<(AgentKind, &str)> =
            batch.iter().map(|s| (s.handle.agent, s.handle.native_id.as_str())).collect();
        let flat = ops::flatten(&ops::group_children(&self.sessions), &|_| true);
        let mut taken: HashSet<usize> = HashSet::new();
        for (at, &(i, depth)) in flat.iter().enumerate() {
            let s = &self.sessions[i];
            if named.contains(&(s.handle.agent, s.handle.native_id.as_str())) && ops::deletes_descendants(s.handle.agent) {
                taken.extend(flat[at + 1..].iter().take_while(|&&(_, d)| d > depth).map(|&(j, _)| j));
            }
        }
        taken
            .into_iter()
            .filter(|&j| {
                let s = &self.sessions[j];
                !named.contains(&(s.handle.agent, s.handle.native_id.as_str()))
            })
            .count()
    }

    /// What a search hit's parent is called, if the hit is inside a
    /// subagent: its title, or its short id when the list has not got it.
    pub fn parent_label(&self, hit: &asm_core::index::SearchHit) -> Option<String> {
        let parent = hit.parent.as_deref()?;
        let agent = AgentKind::parse(&hit.agent)?;
        Some(match self.sessions.iter().find(|s| s.handle.agent == agent && s.handle.native_id == parent) {
            Some(s) => s.title.clone().unwrap_or_else(|| s.short_id().to_string()),
            None => asm_core::model::short_id_of(agent, parent, None).to_string(),
        })
    }

    /// How many sessions the delete being confirmed takes along.
    pub fn delete_takes(&self) -> usize {
        self.pending_below
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
        if project
            && let Some(here) = &self.here
            && !here.contains(&session.project_root, self.all_worktrees)
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
        if let Some(here) = &self.here {
            let at = crate::ui::shorten(&crate::ui::home_relative(&here.worktree), 40);
            active.push(match (self.all_worktrees, here.worktrees.len()) {
                (true, n) if n > 1 => format!("here: {at}, all {n} worktrees"),
                _ => format!("here: {at} (this worktree)"),
            });
        }
        if !self.filter.is_empty() {
            active.push(format!("\"{}\"", self.filter));
        }
        active
    }

    pub fn clear_filters(&mut self) -> bool {
        let had = !self.agents.is_empty() || self.project.is_some() || self.here.is_some() || !self.filter.is_empty();
        self.agents.clear();
        self.project = None;
        self.here = None;
        self.filter.clear();
        if had {
            self.apply_filter();
            self.status = self.count_status();
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
                    self.rows_of(|s| s.handle.agent == agent && self.matches_but_agents(s))
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
                    self.rows_of(|s| self.in_project(s, root) && self.matches_but_project(s))
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
                        count: self.rows_of(|s| self.matches_but_project(s)),
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
    pub fn set_hub_for_test(&mut self, status: HubStatus) {
        self.take_hub(status);
    }

    #[cfg(test)]
    pub fn apply_filter_for_test(&mut self) {
        self.apply_filter();
    }

    #[cfg(test)]
    pub fn toggle_tree_for_test(&mut self) {
        self.toggle_tree();
    }

    /// `d` on a row, as a keystroke would.
    #[cfg(test)]
    pub fn begin_delete_for_test(&mut self, row: usize) {
        self.selected = row;
        self.on_normal_key(KeyEvent::from(KeyCode::Char('d')));
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
                self.here = None;
                self.all_worktrees = false;
                self.picker = None;
                self.mode = Mode::Normal;
            }
        }
        self.refilter();
    }

    fn refilter(&mut self) {
        self.apply_filter();
        self.status = match self.active_filters().as_slice() {
            [] => self.count_status(),
            filters => format!("{} of {} — {}", self.filtered.len(), self.total_rows(), filters.join(" · ")),
        };
    }

    /// `.`: narrow to the checkout asm runs in, or stop narrowing to it.
    fn toggle_here(&mut self) {
        if self.here.take().is_none() {
            match std::env::current_dir() {
                Ok(cwd) => {
                    self.here = Some(asm_core::ops::here(&cwd));
                    self.all_worktrees = false;
                    self.project = None;
                }
                Err(e) => return self.set_status(format!("the current directory is unreadable: {e}")),
            }
        }
        self.refilter();
    }

    /// `w`: between this worktree and all of the repository's. On a project
    /// picked with `P` (the whole repository) it narrows to the worktree asm
    /// runs in when that is one of the project's, else the main checkout.
    fn toggle_worktrees(&mut self) {
        if self.here.is_none()
            && let Some(project) = self.project.clone()
        {
            let worktrees = self.project_worktrees(&project);
            if worktrees.len() < 2 {
                return self.set_status("this project has only one worktree".to_string());
            }
            let cwd = std::env::current_dir().ok().map(|cwd| asm_core::ops::here(&cwd).worktree);
            let worktree = cwd.filter(|c| worktrees.contains(c)).unwrap_or(project);
            self.here = Some(asm_core::ops::Here { worktree, worktrees });
            self.all_worktrees = true;
            self.project = None;
        }
        let Some(here) = &self.here else {
            return self.set_status("w: narrow to a repository first, with . or P".to_string());
        };
        if here.worktrees.len() < 2 {
            return self.set_status("this repository has only one worktree".to_string());
        }
        self.all_worktrees = !self.all_worktrees;
        self.refilter();
    }

    fn project_worktrees(&self, project: &Path) -> Vec<PathBuf> {
        self.projects
            .iter()
            .find(|p| p.root == project)
            .map(|p| p.worktrees.iter().map(|w| w.path.clone()).collect())
            .unwrap_or_default()
    }

    /// What `w` would do next, for the footer: `None` where there is no
    /// second worktree to switch to.
    pub fn worktrees_hint(&self) -> Option<&'static str> {
        // On a `P` project, `w` narrows; on a here filter it flips.
        let (count, widen) = match (&self.here, &self.project) {
            (Some(here), _) => (here.worktrees.len(), !self.all_worktrees),
            (None, Some(project)) => (self.project_worktrees(project).len(), false),
            (None, None) => return None,
        };
        (count > 1).then_some(if widen { "worktrees" } else { "this worktree" })
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

    /// Rebuild the rows: what the filters match, grouped by parent, with
    /// the children of whatever is folded left out. Grouping happens among
    /// the matches, so a match whose parent did not match stands on its own
    /// instead of vanishing behind it.
    fn apply_filter(&mut self) {
        let matching: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| self.matches(s))
            .map(|(i, _)| i)
            .collect();
        let list: Vec<&Session> = matching.iter().map(|&i| &self.sessions[i]).collect();
        let tree = ops::group_children(&list);
        let mut below = vec![0; list.len()];
        fn count(nodes: &[ops::Node], below: &mut [usize]) {
            for node in nodes {
                below[node.index] = node.descendants();
                count(&node.children, below);
            }
        }
        count(&tree, &mut below);
        let open = |k: usize| self.is_open(list[k]);
        let flat = ops::flatten(&tree, &open);
        let places: Vec<Place> = flat
            .iter()
            .map(|&(k, depth)| Place { depth: depth as u16, below: below[k], open: below[k] > 0 && open(k) })
            .collect();
        let rows: Vec<usize> = flat.iter().map(|&(k, _)| matching[k]).collect();
        let parents = self.index_parents();
        self.filtered = rows;
        self.places = places;
        self.parent_ix = parents;
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
        self.sync_preview();
    }

    fn index_parents(&self) -> Vec<Option<usize>> {
        let at: HashMap<(AgentKind, &str), usize> = self
            .sessions
            .iter()
            .enumerate()
            .map(|(i, s)| ((s.handle.agent, s.handle.native_id.as_str()), i))
            .collect();
        self.sessions
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let parent = s.parent.as_deref()?;
                at.get(&(s.handle.agent, parent)).copied().filter(|&p| p != i)
            })
            .collect()
    }

    /// Whether a session's children are shown: the global choice, unless
    /// this one has been flipped from it.
    fn is_open(&self, session: &Session) -> bool {
        self.tree != (!self.toggled.is_empty() && self.toggled.contains(&Self::key_of(session)))
    }

    /// What the list shows of the row at `row` besides its session.
    pub fn place(&self, row: usize) -> Place {
        self.places.get(row).copied().unwrap_or_default()
    }

    /// Whether any row is a parent or a child, so the list needs the room
    /// for its fold marks and the indent.
    pub fn grouped(&self) -> bool {
        self.places.iter().any(|p| p.below > 0 || p.depth > 0)
    }

    /// Sessions that are not under another one, over every session.
    fn top_level(&self) -> usize {
        // Before the first grouping every session is its own.
        if self.parent_ix.len() != self.sessions.len() {
            return self.sessions.len();
        }
        self.parent_ix.iter().filter(|p| p.is_none()).count()
    }

    /// Sessions that are under another one in the list, over every session.
    pub fn subagents(&self) -> usize {
        self.sessions.len() - self.top_level()
    }

    /// "142 sessions", and how many more are under them.
    fn count_status(&self) -> String {
        match self.subagents() {
            0 => format!("{} sessions", self.sessions.len()),
            n => format!("{} sessions (+{n} subagent)", self.top_level()),
        }
    }

    /// How many rows the list has with no filter on: every session when
    /// they are shown as a tree, the top-level ones otherwise.
    pub fn total_rows(&self) -> usize {
        if self.tree { self.sessions.len() } else { self.top_level() }
    }

    /// How many rows the list would have if `keep` chose the sessions: a
    /// picker's count. Subagents count only when they are shown, and are
    /// left out when their parent is among the chosen (they are folded
    /// under it).
    fn rows_of(&self, keep: impl Fn(&Session) -> bool) -> usize {
        let kept: Vec<bool> = self.sessions.iter().map(keep).collect();
        // ponytail: a parent cycle in the data counts both members as
        // children; the list itself cuts it (ops::group_children).
        (0..kept.len())
            .filter(|&i| {
                kept[i] && (self.tree || !self.parent_ix.get(i).copied().flatten().is_some_and(|p| kept[p]))
            })
            .count()
    }

    /// Rebuild the rows after something changed what is folded, keeping
    /// the cursor on the session it was on.
    fn regroup(&mut self) {
        let under_cursor = self.selected_session().map(Self::key_of);
        self.apply_filter();
        self.keep_cursor_on(under_cursor);
        self.sync_preview();
    }

    /// `z`: fold or unfold the subagents under the selected session. On a
    /// subagent with none of its own, fold the session it belongs to and
    /// move up to it, the way a file manager's left arrow does.
    fn toggle_fold(&mut self) {
        let Some(session) = self.selected_session() else { return };
        let key = Self::key_of(session);
        let place = self.place(self.selected);
        if place.below > 0 {
            if !self.toggled.remove(&key) {
                self.toggled.insert(key);
            }
            self.regroup();
            return;
        }
        let parent = self
            .selected_parent_row()
            .map(|row| Self::key_of(&self.sessions[self.filtered[row]]));
        match parent {
            Some(parent) => {
                // It is shown, so it is open: folding is flipping it from
                // the global choice.
                if self.tree {
                    self.toggled.insert(parent.clone());
                } else {
                    self.toggled.remove(&parent);
                }
                self.regroup();
                self.keep_cursor_on(Some(parent));
                self.sync_preview();
            }
            None => self.status = "no subagent sessions under this one (T shows them all)".to_string(),
        }
    }

    /// The row of the session the selected one is under, if it is listed
    /// under one.
    fn selected_parent_row(&self) -> Option<usize> {
        let depth = self.place(self.selected).depth;
        if depth == 0 {
            return None;
        }
        (0..self.selected).rev().find(|&row| self.place(row).depth + 1 == depth)
    }

    /// `T`: subagent sessions shown as a tree under their parent, or
    /// hidden behind it. Choosing resets every fold made by hand.
    fn toggle_tree(&mut self) {
        self.tree = !self.tree;
        self.toggled.clear();
        self.regroup();
        self.status = if self.tree {
            "subagent sessions shown as a tree (z folds one, T hides them)".to_string()
        } else {
            "subagent sessions hidden under their parent (z opens one, T shows them all)".to_string()
        };
        self.status_sticky = true;
    }

    /// Unfold whatever hides a session, so the list has a row for it.
    /// Walks up through its parents; a loop in the data ends the walk.
    fn reveal(&mut self, agent: AgentKind, id: &str) {
        let mut at = self.sessions.iter().find(|s| s.handle.agent == agent && s.handle.native_id == id);
        let mut flips = HashSet::new();
        for _ in 0..self.sessions.len() {
            let Some(parent) = at.and_then(|s| s.parent.as_deref()) else { break };
            at = self.sessions.iter().find(|s| s.handle.agent == agent && s.handle.native_id == parent);
            match at {
                Some(session) if !self.is_open(session) => {
                    flips.insert(Self::key_of(session));
                }
                Some(_) => {}
                None => break,
            }
        }
        if flips.is_empty() {
            return;
        }
        for key in flips {
            if !self.toggled.remove(&key) {
                self.toggled.insert(key);
            }
        }
        self.apply_filter();
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
        let label = format!("sending to {}… (esc stops)", session.handle.agent);
        self.dispatch(label, Request::Send(Box::new(session), message));
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
                self.in_flight.pop_front();
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
                    if !self.status_sticky && self.in_flight.is_empty() {
                        self.status = self.count_status();
                    }
                    // Now that the list is up, keep the index current and ask
                    // the hub how it compares.
                    let _ = self.worker.index_tx.send(());
                    self.request_hub();
                    if std::mem::take(&mut self.rescan_pending) {
                        self.request_scan();
                    }
                }
                // Cleared by `Indexed`, not by the last count: the final
                // batch is still on its way into the database.
                Response::Indexing(progress) => self.indexing = Some(progress),
                Response::Indexed(reindexed, failed) => {
                    self.indexing = None;
                    if reindexed > 0 && !self.status_sticky && self.in_flight.is_empty() {
                        self.status = format!(
                            "{} — indexed {reindexed}{}",
                            self.count_status(),
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
                Response::Hub(status, daemon) => {
                    self.daemon = daemon;
                    self.take_hub(*status)
                }
                Response::PullFailed { agent, id, message, needs_dir } => {
                    self.in_flight.pop_front();
                    self.status = format!("pull failed: {message}");
                    self.status_sticky = true;
                    if needs_dir {
                        // Its own directory is not here: ask where to put it.
                        self.input = std::env::current_dir()
                            .map(|d| d.display().to_string())
                            .unwrap_or_default();
                        self.pending_pull = Some((agent, id));
                        self.mode = Mode::PullDir;
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
                    self.in_flight.pop_front();
                    self.status = message;
                    self.status_sticky = true;
                    self.request_scan();
                }
                Response::Bulk(verb, report) => {
                    self.in_flight.pop_front();
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
                Response::Failed(message) => {
                    self.in_flight.pop_front();
                    self.status = format!("error: {message}");
                    self.status_sticky = true;
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
                Mode::PullDir => self.on_pull_dir_key(key),
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
        // A hit inside a subagent lands on it, unfolding what hides it.
        if let Some(agent) = AgentKind::parse(&target.0) {
            self.reveal(agent, &target.1);
        }
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
        if self.hub_view {
            return self.on_hub_view_key(key);
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
            KeyCode::Char('H') => self.open_hub_view(),
            KeyCode::Char('z') => self.toggle_fold(),
            KeyCode::Char('T') => self.toggle_tree(),
            KeyCode::Char('A') => self.open_picker(PickerKind::Agent),
            KeyCode::Char('P') => self.open_picker(PickerKind::Project),
            KeyCode::Char('.') => self.toggle_here(),
            KeyCode::Char('w') => self.toggle_worktrees(),
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
            KeyCode::Char('R') => {
                // Asked for, so git is asked again too.
                crate::worker::forget_git();
                self.request_scan();
            }
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
                    self.dispatch("pushing…".to_string(), Request::Bulk(vec![session], BulkAction::Push));
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
                    self.dispatch("working…".to_string(), request);
                }
            }
            KeyCode::Char('d') => {
                if !self.selection.is_empty() {
                    self.begin_bulk(BulkAction::Delete, None);
                } else if let Some(session) = self.selected_session().cloned() {
                    self.pending_below = self.deleted_with(std::slice::from_ref(&session));
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
                    self.dispatch("exporting…".to_string(), Request::Export(Box::new(session), path));
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
                    self.dispatch("moving…".to_string(), Request::Move(Box::new(session), dir));
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
                    self.dispatch("importing…".to_string(), Request::Import(Box::new(session), target));
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
                    let request = Request::Rename(Box::new(session), self.input.clone());
                    self.dispatch("renaming…".to_string(), request);
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
                    self.dispatch("deleting…".to_string(), Request::Delete(Box::new(session)));
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

    fn here(worktree: &str, worktrees: &[&str]) -> asm_core::ops::Here {
        asm_core::ops::Here { worktree: worktree.into(), worktrees: worktrees.iter().map(PathBuf::from).collect() }
    }

    /// `.` narrows to where asm runs and back, and it and `P` replace each other.
    #[test]
    fn dot_toggles_the_here_filter_and_p_replaces_it() {
        let mut app = app();
        app.project = Some("/w/atlas".into());
        press(&mut app, KeyCode::Char('.'));
        let cwd = asm_core::ops::here(&std::env::current_dir().unwrap());
        assert_eq!(app.here.as_ref(), Some(&cwd));
        assert!(app.project.is_none(), ". replaces the picked project");
        assert!(app.active_filters().iter().any(|f| f.starts_with("here: ") && f.ends_with("(this worktree)")));
        press(&mut app, KeyCode::Char('.'));
        assert!(app.here.is_none() && app.project.is_none(), "and off again");

        press(&mut app, KeyCode::Char('.'));
        press(&mut app, KeyCode::Char('P'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(app.project.is_some() && app.here.is_none(), "P replaces it");
    }

    /// `w` widens `here` to every worktree and back, and on a P project
    /// narrows the whole repository to one checkout.
    #[test]
    fn w_switches_between_this_worktree_and_all_of_them() {
        let mut app = test_app(vec![
            session(AgentKind::ClaudeCode, "main", "/w/mercury/src"),
            session(AgentKind::ClaudeCode, "wt", "/w/mercury-wt"),
            session(AgentKind::ClaudeCode, "near", "/w/mercury2"),
        ]);
        app.here = Some(here("/w/mercury-wt", &["/w/mercury", "/w/mercury-wt"]));
        app.apply_filter();
        assert_eq!(app.filtered.len(), 1, "only the worktree asm runs in");
        press(&mut app, KeyCode::Char('w'));
        assert!(app.all_worktrees);
        assert_eq!(app.filtered.len(), 2, "both worktrees, and not the look-alike sibling");
        assert!(app.active_filters().contains(&"here: /w/mercury-wt, all 2 worktrees".to_string()));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.filtered.len(), 1);

        // One worktree: nothing to switch between, and the status says so.
        app.here = Some(here("/w/atlas", &["/w/atlas"]));
        press(&mut app, KeyCode::Char('w'));
        assert!(!app.all_worktrees && app.status.contains("only one worktree"), "{}", app.status);

        // A P project is the whole repository; w narrows it to a checkout
        // (the main one here: the tests do not run inside /w/mercury).
        let mut repo = project("/w/mercury");
        repo.worktrees.push(asm_core::model::ProjectWorktree {
            path: "/w/mercury-wt".into(),
            branch: None,
            is_main: false,
            session_count: 1,
        });
        app.projects = vec![repo];
        app.here = None;
        app.project = Some("/w/mercury".into());
        press(&mut app, KeyCode::Char('w'));
        assert!(app.project.is_none() && !app.all_worktrees);
        assert_eq!(app.here.as_ref().map(|h| h.worktree.clone()), Some("/w/mercury".into()));
        assert_eq!(app.filtered.len(), 1);
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.filtered.len(), 2, "and back out to every worktree");
    }

    /// A worktree nested in the main checkout (Claude Code's
    /// `.claude/worktrees/<name>`) is its own: "this worktree" in the main
    /// one leaves it out, and `w` brings it in.
    #[test]
    fn here_in_the_main_checkout_leaves_out_a_worktree_nested_in_it() {
        let nested = "/w/mercury/.claude/worktrees/x";
        let mut app = test_app(vec![
            session(AgentKind::ClaudeCode, "main", "/w/mercury/src"),
            session(AgentKind::ClaudeCode, "nested", "/w/mercury/.claude/worktrees/x/src"),
        ]);
        let ids = |app: &App| -> Vec<String> {
            app.filtered.iter().map(|&i| app.sessions[i].handle.native_id.clone()).collect()
        };
        app.here = Some(here("/w/mercury", &["/w/mercury", nested]));
        app.apply_filter();
        assert_eq!(ids(&app), vec!["main"]);
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(ids(&app), vec!["main", "nested"]);
        app.here = Some(here(nested, &["/w/mercury", nested]));
        app.all_worktrees = false;
        app.apply_filter();
        assert_eq!(ids(&app), vec!["nested"]);
    }

    /// The footer's `w` says what pressing it would do, and is not offered
    /// where there is no second worktree.
    #[test]
    fn the_w_hint_says_what_w_does_next() {
        let mut app = app();
        assert_eq!(app.worktrees_hint(), None, "no repository narrowed to");
        app.here = Some(here("/w/atlas", &["/w/atlas"]));
        assert_eq!(app.worktrees_hint(), None, "one worktree");
        app.here = Some(here("/w/mercury", &["/w/mercury", "/w/mercury-wt"]));
        assert_eq!(app.worktrees_hint(), Some("worktrees"));
        app.all_worktrees = true;
        assert_eq!(app.worktrees_hint(), Some("this worktree"));

        // A P project is every worktree already, so w narrows, whatever
        // the earlier here filter left behind.
        app.projects[0].worktrees.push(asm_core::model::ProjectWorktree {
            path: "/w/mercury-wt".into(),
            branch: None,
            is_main: false,
            session_count: 0,
        });
        press(&mut app, KeyCode::Char('P'));
        for c in "merc".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.project.as_deref(), Some(Path::new("/w/mercury")), "{:?}", app.project);
        assert!(!app.all_worktrees, "P starts over");
        assert_eq!(app.worktrees_hint(), Some("this worktree"));
        app.project = Some("/w/atlas".into());
        assert_eq!(app.worktrees_hint(), None, "a project of one worktree");
    }

    /// `asm --here` is set before the first scan, so arriving rows are
    /// narrowed as they come.
    #[test]
    fn a_here_filter_from_the_command_line_narrows_the_first_scan() {
        let mut app = test_app(Vec::new());
        app.here = Some(here("/w/mercury", &["/w/mercury"]));
        app.all_worktrees = true;
        app.take_batch(vec![
            session(AgentKind::ClaudeCode, "in", "/w/mercury/src"),
            session(AgentKind::ClaudeCode, "out", "/w/atlas"),
        ]);
        assert_eq!(app.filtered.len(), 1);
        assert_eq!(app.active_filters(), vec!["here: /w/mercury (this worktree)".to_string()], "one worktree is not 'all 1'");
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

    /// A scan is one request. The projects and the stores' health used to
    /// be asked for beside it, each listing every store again, with
    /// everything else the worker was asked meanwhile queued behind them.
    #[test]
    fn a_scan_is_one_request_of_the_worker() {
        let (tx, requests) = std::sync::mpsc::channel();
        let worker = crate::worker::Worker {
            tx,
            index_tx: std::sync::mpsc::channel().0,
            rx: std::sync::mpsc::channel().1,
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let mut app = App::new(worker);
        app.request_scan();
        assert!(matches!(requests.try_recv(), Ok(Request::Scan)));
        assert!(requests.try_recv().is_err(), "projects and health come back with the scan");
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

    fn joined_with(app: &mut App, connected: bool) {
        let rows: Vec<Row> = [("aaa", RowState::Local), ("bbb", RowState::Diverged)]
            .into_iter()
            .enumerate()
            .map(|(i, (_, state))| {
                let s = &app.sessions[i];
                Row {
                    project: String::new(),
                    agent: s.handle.agent,
                    id: s.handle.native_id.clone(),
                    short_id: s.short_id().to_string(),
                    title: None,
                    machine: String::new(),
                    updated: None,
                    state,
                    action: state.action(),
                    label: state.label(),
                    hint: state.hint(),
                    restorable: true,
                    detail: Default::default(),
                }
            })
            .collect();
        app.set_hub_for_test(HubStatus { joined: true, connected, rows, ..Default::default() });
    }

    /// Nothing is sent to a hub that is not answering, and a diverged
    /// session is explained rather than pushed over.
    #[test]
    fn the_hub_view_acts_only_when_it_can_and_explains_when_it_cannot() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (_tx, resp) = std::sync::mpsc::channel();
        let worker = crate::worker::Worker {
            tx,
            index_tx: std::sync::mpsc::channel().0,
            rx: resp,
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let mut app = App::new(worker);
        app.sessions = super::tests::app().sessions;
        app.filtered = (0..app.sessions.len()).collect();

        // Not joined: H says how, and opens nothing.
        app.on_normal_key(KeyEvent::from(KeyCode::Char('H')));
        assert!(!app.hub_view);

        joined_with(&mut app, true);
        app.on_normal_key(KeyEvent::from(KeyCode::Char('H')));
        assert!(app.hub_view);
        while rx.try_recv().is_ok() {} // the refresh H asks for

        // The diverged session sorts first, and is explained, not pushed.
        press_hub(&mut app, KeyCode::Enter);
        assert!(app.status.contains("push --force"), "{}", app.status);
        assert!(rx.try_recv().is_err(), "nothing was sent for a diverged session");

        // Then the local one: Enter pushes it.
        press_hub(&mut app, KeyCode::Down);
        press_hub(&mut app, KeyCode::Enter);
        assert!(matches!(rx.try_recv(), Ok(Request::Bulk(_, BulkAction::Push))));

        // The hub stops answering: the same key sends nothing, and says why.
        joined_with(&mut app, false);
        press_hub(&mut app, KeyCode::Enter);
        assert!(rx.try_recv().is_err());
        assert!(app.status.contains("cannot be reached"), "{}", app.status);

        // Esc leaves the view.
        press_hub(&mut app, KeyCode::Esc);
        assert!(!app.hub_view);
    }

    fn press_hub(app: &mut App, code: KeyCode) {
        app.on_hub_view_key(KeyEvent::from(code));
    }

    /// A session at a time (seconds), under a parent or not.
    fn at(agent: AgentKind, id: &str, parent: Option<&str>, seconds: i64) -> Session {
        Session {
            parent: parent.map(str::to_string),
            updated: jiff::Timestamp::from_second(seconds).ok(),
            ..session(agent, id, "/w/app")
        }
    }

    /// Plan > Review > Nested, Plan > Explore, and another root, newest first.
    fn family() -> App {
        let mut app = test_app(vec![
            at(AgentKind::OpenCode, "plan", None, 100),
            at(AgentKind::OpenCode, "review", Some("plan"), 90),
            at(AgentKind::OpenCode, "nested", Some("review"), 80),
            at(AgentKind::OpenCode, "explore", Some("plan"), 70),
            at(AgentKind::ClaudeCode, "other", None, 60),
        ]);
        app.apply_filter();
        app
    }

    /// The rows, as `id`, with `+N` on a folded session and indented by depth.
    fn rows(app: &App) -> Vec<String> {
        app.filtered
            .iter()
            .enumerate()
            .map(|(row, &i)| {
                let place = app.place(row);
                let fold = match (place.below, place.open) {
                    (0, _) => String::new(),
                    (n, false) => format!("+{n}"),
                    (_, true) => "v".into(),
                };
                format!("{}{}{fold}", "-".repeat(place.depth as usize), app.sessions[i].handle.native_id)
            })
            .collect()
    }

    fn on(app: &mut App, id: &str) {
        app.selected = app.filtered.iter().position(|&i| app.sessions[i].handle.native_id == id).unwrap();
    }

    fn under_cursor(app: &App) -> String {
        app.selected_session().unwrap().handle.native_id.clone()
    }

    /// Subagents are behind their parent, with how many, until asked for.
    #[test]
    fn subagents_are_hidden_behind_their_parent_with_a_count() {
        let app = family();
        assert_eq!(rows(&app), ["plan+3", "other"]);
        assert_eq!(app.count_status(), "2 sessions (+3 subagent)");
        assert!(app.grouped());
        // A list with no subagents in it is the list it always was.
        let plain = super::tests::app();
        assert!(!plain.grouped());
        assert_eq!(plain.count_status(), "4 sessions");
    }

    #[test]
    fn z_unfolds_the_selected_session_one_level_and_again_folds_it() {
        let mut app = family();
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(rows(&app), ["planv", "-review+1", "-explore", "other"]);
        assert_eq!(under_cursor(&app), "plan", "the cursor stays on it");

        on(&mut app, "review");
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(rows(&app), ["planv", "-reviewv", "--nested", "-explore", "other"]);
        assert_eq!(under_cursor(&app), "review");

        press(&mut app, KeyCode::Char('z'));
        assert_eq!(rows(&app), ["planv", "-review+1", "-explore", "other"]);
        on(&mut app, "plan");
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(rows(&app), ["plan+3", "other"], "folded again, with the count");
    }

    /// `T` is the global choice between hiding subagents and showing them
    /// all; choosing it forgets what was folded by hand.
    #[test]
    fn t_shows_every_subagent_as_a_tree_and_back() {
        let mut app = family();
        press(&mut app, KeyCode::Char('z'));
        press(&mut app, KeyCode::Char('T'));
        assert!(app.tree);
        assert_eq!(rows(&app), ["planv", "-reviewv", "--nested", "-explore", "other"]);

        // In a tree, z folds the one it is on.
        on(&mut app, "review");
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(rows(&app), ["planv", "-review+1", "-explore", "other"]);

        press(&mut app, KeyCode::Char('T'));
        assert!(!app.tree);
        assert_eq!(rows(&app), ["plan+3", "other"], "back to hidden, and the fold by hand is forgotten");
    }

    /// The cursor is on a session, not on a row: rows come and go above it.
    #[test]
    fn the_cursor_stays_on_its_session_when_rows_appear_above_it() {
        let mut app = family();
        on(&mut app, "other");
        assert_eq!(app.selected, 1);
        press(&mut app, KeyCode::Char('T'));
        assert_eq!(app.selected, 4);
        assert_eq!(under_cursor(&app), "other");
        press(&mut app, KeyCode::Char('T'));
        assert_eq!(under_cursor(&app), "other");

        // Folding a parent while on its child moves up to the parent.
        press(&mut app, KeyCode::Char('T'));
        on(&mut app, "nested");
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(under_cursor(&app), "review", "z on a subagent with none folds its parent");
        assert_eq!(rows(&app), ["planv", "-review+1", "-explore", "other"]);
        on(&mut app, "explore");
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(under_cursor(&app), "plan");
        assert_eq!(rows(&app), ["plan+3", "other"]);
        on(&mut app, "other");
        press(&mut app, KeyCode::Char('z'));
        assert!(app.status.contains("no subagent sessions"), "{}", app.status);
    }

    /// A subagent that matches a filter its parent does not match is a row
    /// of its own, not a row nobody can see.
    #[test]
    fn a_match_whose_parent_did_not_match_stands_alone() {
        let mut app = family();
        app.filter = "nested".into();
        app.apply_filter();
        assert_eq!(rows(&app), ["nested"]);
        app.filter = "e".into();
        app.apply_filter();
        // plan, review, nested, explore, other all contain an e.
        assert_eq!(rows(&app), ["plan+3", "other"]);
        // review and nested match, plan does not: review is the root, with nested under it.
        app.sessions[2].title = Some("about review, nested".into());
        app.filter = "review".into();
        app.apply_filter();
        assert_eq!(rows(&app), ["review+1"]);
        assert_eq!(app.place(0).depth, 0);
    }

    fn hit(agent: &str, id: &str, parent: Option<&str>) -> asm_core::index::SearchHit {
        asm_core::index::SearchHit {
            agent: agent.into(),
            native_id: id.into(),
            title: None,
            project_root: None,
            parent: parent.map(str::to_string),
            role: "user".into(),
            seq: 3,
            timestamp: None,
            snippet: "x".into(),
            score: 0.0,
            status: "idle".into(),
            state: "idle".into(),
        }
    }

    /// A search hit in a subagent opens on it, however deep and however
    /// folded, and says whose it is.
    #[test]
    fn a_search_hit_on_a_subagent_unfolds_its_parents_and_lands_on_it() {
        let mut app = family();
        assert_eq!(rows(&app), ["plan+3", "other"]);
        app.hits = Some(vec![hit("opencode", "nested", Some("review"))]);
        assert_eq!(app.parent_label(&app.hits.as_ref().unwrap()[0]).as_deref(), Some("about review"));
        app.jump_to_hit();
        assert!(app.hits.is_none(), "back to the list");
        assert_eq!(under_cursor(&app), "nested");
        assert_eq!(rows(&app), ["planv", "-reviewv", "--nested", "-explore", "other"]);
        assert!(app.status.contains("jumped to match #3"), "{}", app.status);

        // A parent the list does not have is named by its id.
        let ghost = hit("opencode", "x", Some("ses_gone0000000"));
        assert_eq!(app.parent_label(&ghost).as_deref(), Some("ses_gone"));
        assert_eq!(app.parent_label(&hit("opencode", "x", None)), None);
    }

    /// The same hit in tree mode with something folded by hand.
    #[test]
    fn a_hit_unfolds_only_what_hides_it() {
        let mut app = family();
        press(&mut app, KeyCode::Char('T'));
        on(&mut app, "review");
        press(&mut app, KeyCode::Char('z'));
        press(&mut app, KeyCode::Char('z')); // and open again
        on(&mut app, "plan");
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(rows(&app), ["plan+3", "other"]);
        app.hits = Some(vec![hit("opencode", "explore", Some("plan"))]);
        app.jump_to_hit();
        assert_eq!(under_cursor(&app), "explore");
        assert_eq!(rows(&app), ["planv", "-reviewv", "--nested", "-explore", "other"]);
    }

    /// A hub compares what a push would send: a subagent travels in its
    /// parent's bundle, so listing it would give a "Local" row that cannot
    /// be pushed.
    #[test]
    fn the_hub_is_asked_about_top_level_sessions_only() {
        let (tx, requests) = std::sync::mpsc::channel();
        let worker = crate::worker::Worker {
            tx,
            index_tx: std::sync::mpsc::channel().0,
            rx: std::sync::mpsc::channel().1,
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let mut app = App::new(worker);
        app.sessions = family().sessions;
        app.apply_filter();
        app.request_hub();
        let Ok(Request::Hub(sent)) = requests.try_recv() else { panic!("the hub was not asked") };
        let ids: Vec<&str> = sent.iter().map(|s| s.handle.native_id.as_str()).collect();
        assert_eq!(ids, ["plan", "other"]);
        // Even with everything shown.
        app.hub_checking = false;
        app.tree = true;
        app.apply_filter();
        app.request_hub();
        let Ok(Request::Hub(sent)) = requests.try_recv() else { panic!("the hub was not asked") };
        assert_eq!(sent.len(), 2);
    }

    /// Deleting a session of an agent that deletes its tree says how many
    /// go with it; the batch counts what is not already named in it.
    #[test]
    fn a_delete_counts_the_sessions_it_takes_along() {
        let mut app = family();
        on(&mut app, "plan");
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.mode, Mode::ConfirmDelete);
        assert_eq!(app.delete_takes(), 3);
        app.mode = Mode::Normal;

        // A session of an agent that keeps its children takes none.
        on(&mut app, "other");
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.delete_takes(), 0);
        app.mode = Mode::Normal;

        // plan and its review ticked: the nested and explore go too, review is already named.
        press(&mut app, KeyCode::Char('T'));
        on(&mut app, "plan");
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.selection.len(), 2);
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.mode, Mode::ConfirmBulk);
        assert_eq!(app.delete_takes(), 2);
        let jcode = {
            let mut jcode = test_app(vec![
                at(AgentKind::JCode, "main", None, 9),
                at(AgentKind::JCode, "kid", Some("main"), 8),
            ]);
            jcode.apply_filter();
            jcode
        };
        assert_eq!(jcode.deleted_with(&jcode.sessions[..1]), 0, "jcode keeps the children");
    }

    /// A picker counts the rows picking it would leave.
    #[test]
    fn pickers_count_rows_not_subagents() {
        let mut app = family();
        app.open_picker(PickerKind::Agent);
        let count = |app: &App, label: &str| app.picker_rows().iter().find(|r| r.label == label).unwrap().count;
        assert_eq!(count(&app, "opencode"), 1, "plan, with three behind it");
        assert_eq!(count(&app, "claude-code"), 1);
        app.tree = true;
        app.apply_filter();
        assert_eq!(count(&app, "opencode"), 4, "all of them, as they are shown");
    }

    /// Subagents arriving before their parent are rows of their own until
    /// it arrives, and the cursor does not move.
    #[test]
    fn a_scan_that_delivers_a_subagent_first_regroups_when_the_parent_comes() {
        let mut app = test_app(Vec::new());
        app.scanning = true;
        app.take_batch(vec![at(AgentKind::OpenCode, "other", None, 5), at(AgentKind::OpenCode, "kid", Some("plan"), 50)]);
        assert_eq!(rows(&app), ["kid", "other"]);
        on(&mut app, "other");
        app.take_batch(vec![at(AgentKind::OpenCode, "plan", None, 100)]);
        assert_eq!(rows(&app), ["plan+1", "other"]);
        assert_eq!(under_cursor(&app), "other");
    }

    /// A push queued behind a scan stays on screen until its own answer
    /// arrives, and a second one is counted rather than hidden.
    #[test]
    fn what_is_in_flight_outlasts_the_scan_in_front_of_it() {
        let (respond, rx) = std::sync::mpsc::channel();
        let mut app = app();
        app.worker.rx = rx;
        let text = |app: &App| {
            crate::ui::status_line_of(app).spans.iter().map(|s| s.content.to_string()).collect::<String>()
        };
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.in_flight.len(), 1);
        // A key in between clears an ordinary message, not this one.
        press(&mut app, KeyCode::Down);
        respond.send(Response::ScanDone(Vec::new())).unwrap();
        app.drain_worker();
        assert_eq!(app.status, "pushing…", "the scan's count did not overwrite it");

        press(&mut app, KeyCode::Char('p'));
        assert!(text(&app).contains("⟳ 2 queued"), "{}", text(&app));
        respond.send(Response::Failed("no hub".into())).unwrap();
        app.drain_worker();
        assert!(text(&app).contains("⟳ pushing…"), "{}", text(&app));
        respond.send(Response::Bulk("Push".into(), BulkReport::default())).unwrap();
        app.drain_worker();
        assert!(app.in_flight.is_empty());
        assert!(!text(&app).contains("pushing"), "{}", text(&app));
    }
}
