//! Running one verb over many sessions.
//!
//! The TUI and the web UI both let you select a set and act on it. Doing
//! that by looping in each frontend would give two subtly different
//! answers to the questions a batch raises — what happens after the third
//! of five fails, whether an unsupported agent is an error or a skip — so
//! the loop lives here and both call it.
//!
//! Three rules the callers depend on:
//!
//! - **Every item is attempted.** A failure never stops the batch, so a
//!   later session can succeed after an earlier one failed. That is why the
//!   report is per item and not a count of how far it got.
//! - **Order is the order given.** Sequential, never parallel: these verbs
//!   move files and rewrite rows in stores other programs own.
//! - **Everything goes through [`crate::ops`].** The single-session verbs
//!   already refuse to touch a live session and back up before destroying
//!   anything; reaching past them into the adapters would quietly drop
//!   those guarantees for batches only.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::adapter::{Adapter, AgentRead, SessionFilter};
use crate::model::{AgentKind, Session};
use crate::ops;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BulkAction {
    Archive,
    Unarchive,
    Delete,
    Move { dir: PathBuf },
    /// One `<short-id>.ir.json` per session, written into this directory.
    Export { dir: PathBuf },
    Import { to: AgentKind },
}

impl BulkAction {
    /// Present tense, for confirmation prompts: "Archive 4 sessions?".
    pub fn verb(&self) -> &'static str {
        match self {
            BulkAction::Archive => "Archive",
            BulkAction::Unarchive => "Unarchive",
            BulkAction::Delete => "Delete",
            BulkAction::Move { .. } => "Move",
            BulkAction::Export { .. } => "Export",
            BulkAction::Import { .. } => "Import",
        }
    }

    /// Whether this action destroys or relocates data, and so deserves a
    /// confirmation step in front of it.
    pub fn is_destructive(&self) -> bool {
        matches!(self, BulkAction::Delete | BulkAction::Move { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ItemOutcome {
    /// Done, with a line worth showing.
    Ok { note: String },
    /// Deliberately not attempted — the agent cannot do this, or there was
    /// nothing to do. Not a failure, and not counted as one.
    Skipped { reason: String },
    Failed { error: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkItem {
    pub agent: AgentKind,
    pub native_id: String,
    /// What the user saw in the list, so the report is readable.
    pub label: String,
    pub outcome: ItemOutcome,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BulkReport {
    pub items: Vec<BulkItem>,
}

impl BulkReport {
    pub fn ok(&self) -> usize {
        self.items.iter().filter(|i| matches!(i.outcome, ItemOutcome::Ok { .. })).count()
    }

    pub fn skipped(&self) -> usize {
        self.items.iter().filter(|i| matches!(i.outcome, ItemOutcome::Skipped { .. })).count()
    }

    pub fn failed(&self) -> usize {
        self.items.iter().filter(|i| matches!(i.outcome, ItemOutcome::Failed { .. })).count()
    }

    /// One line for a status bar. Mentions failures and skips only when
    /// there are some, so the common case stays quiet.
    pub fn summary(&self, verb: &str) -> String {
        let mut s = format!("{verb}: {} of {} succeeded", self.ok(), self.items.len());
        if self.skipped() > 0 {
            s.push_str(&format!(", {} skipped", self.skipped()));
        }
        if self.failed() > 0 {
            s.push_str(&format!(", {} failed", self.failed()));
        }
        s
    }

    /// The lines a UI should show under the summary: everything that did
    /// not simply work.
    pub fn problems(&self) -> Vec<String> {
        self.items
            .iter()
            .filter_map(|i| match &i.outcome {
                ItemOutcome::Ok { .. } => None,
                ItemOutcome::Skipped { reason } => Some(format!("{}: skipped — {reason}", i.label)),
                ItemOutcome::Failed { error } => Some(format!("{}: failed — {error}", i.label)),
            })
            .collect()
    }
}

fn capabilities_of(agent: AgentKind) -> Option<crate::adapter::Capabilities> {
    Adapter::available().into_iter().find(|a| a.kind() == agent).map(|a| a.capabilities())
}

/// Does `agent` support this action at all? A "no" is a skip with a
/// reason, not a failure — asking jcode to be an import destination is a
/// property of jcode, not a fault in the batch.
fn unsupported(agent: AgentKind, action: &BulkAction) -> Option<String> {
    let caps = capabilities_of(agent)?;
    let ok = match action {
        BulkAction::Archive | BulkAction::Unarchive => caps.archive,
        BulkAction::Delete => caps.delete,
        BulkAction::Move { .. } => caps.relocate,
        BulkAction::Export { .. } => caps.export_ir,
        // Import reads from this session; the destination is checked
        // separately, once, before the loop.
        BulkAction::Import { .. } => caps.export_ir,
    };
    (!ok).then(|| format!("{agent} does not support {}", action.verb().to_lowercase()))
}

/// Run `action` over `sessions`, in order, attempting every one.
pub fn run(sessions: &[Session], action: &BulkAction) -> BulkReport {
    // The destination's own capability is a property of the batch, not of
    // any one session, so it is resolved once and turns every item into a
    // skip rather than repeating the same failure N times.
    let dest_refuses = match action {
        BulkAction::Import { to } => match capabilities_of(*to) {
            Some(caps) if !caps.import_ir => {
                Some(format!("{to} cannot be an import destination"))
            }
            None => Some(format!("{to} is not installed")),
            _ => None,
        },
        _ => None,
    };

    let mut report = BulkReport::default();
    for session in sessions {
        let label = format!("{} {}", session.handle.agent, session.short_id());
        let outcome = if let Some(reason) = dest_refuses.clone() {
            ItemOutcome::Skipped { reason }
        } else if let Some(reason) = unsupported(session.handle.agent, action) {
            ItemOutcome::Skipped { reason }
        } else {
            apply(session, action)
        };
        report.items.push(BulkItem {
            agent: session.handle.agent,
            native_id: session.handle.native_id.clone(),
            label,
            outcome,
        });
    }
    report
}

fn apply(session: &Session, action: &BulkAction) -> ItemOutcome {
    match action {
        BulkAction::Archive => match ops::archive(session) {
            Ok(_) => ItemOutcome::Ok { note: "archived".into() },
            Err(e) => ItemOutcome::Failed { error: e.to_string() },
        },
        BulkAction::Unarchive => {
            // Unarchive resolves by reference because a Claude session may
            // only exist in our archive store by then.
            let query = format!("{}:{}", session.handle.agent, session.handle.native_id);
            match ops::unarchive(&query, &SessionFilter::default()) {
                Ok(_) => ItemOutcome::Ok { note: "unarchived".into() },
                Err(e) => ItemOutcome::Failed { error: e.to_string() },
            }
        }
        BulkAction::Delete => match ops::delete(session) {
            Ok(_) => ItemOutcome::Ok { note: "deleted, backed up first".into() },
            Err(e) => ItemOutcome::Failed { error: e.to_string() },
        },
        BulkAction::Move { dir } => {
            if session.project_root == *dir {
                return ItemOutcome::Skipped { reason: "already in that directory".into() };
            }
            match ops::relocate(session, dir) {
                Ok(_) => ItemOutcome::Ok { note: format!("moved to {}", dir.display()) },
                Err(e) => ItemOutcome::Failed { error: e.to_string() },
            }
        }
        BulkAction::Export { dir } => export_one(session, dir),
        BulkAction::Import { to } => {
            if session.handle.agent == *to {
                return ItemOutcome::Skipped { reason: format!("already a {to} session") };
            }
            // Full mode, into the source session's own project — the same
            // defaults the single-session verb uses.
            let opts = crate::import::ImportOpts {
                mode: crate::import::ImportMode::Full,
                project: None,
                dry_run: false,
            };
            match ops::import(session, *to, &opts) {
                Ok(outcome) if outcome.in_sync => {
                    ItemOutcome::Skipped { reason: "already imported".into() }
                }
                Ok(outcome) => ItemOutcome::Ok {
                    note: outcome
                        .target
                        .map(|t| format!("imported as {}", t.native_id))
                        .unwrap_or_else(|| "imported".into()),
                },
                Err(e) => ItemOutcome::Failed { error: e.to_string() },
            }
        }
    }
}

/// Export writes a file per session, so the batch owns the naming. The
/// native id keeps names unique where a short id would collide across
/// agents.
fn export_one(session: &Session, dir: &std::path::Path) -> ItemOutcome {
    let ir = match ops::export_ir(session) {
        Ok(ir) => ir,
        Err(e) => return ItemOutcome::Failed { error: e.to_string() },
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        return ItemOutcome::Failed { error: format!("{}: {e}", dir.display()) };
    }
    let safe: String = session
        .handle
        .native_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let path = dir.join(format!("{}-{safe}.ir.json", session.handle.agent));
    let json = match serde_json::to_string_pretty(&ir) {
        Ok(j) => j,
        Err(e) => return ItemOutcome::Failed { error: e.to_string() },
    };
    match std::fs::write(&path, json) {
        Ok(()) => ItemOutcome::Ok { note: format!("wrote {}", path.display()) },
        Err(e) => ItemOutcome::Failed { error: format!("{}: {e}", path.display()) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(outcomes: Vec<ItemOutcome>) -> BulkReport {
        BulkReport {
            items: outcomes
                .into_iter()
                .enumerate()
                .map(|(i, outcome)| BulkItem {
                    agent: AgentKind::ClaudeCode,
                    native_id: format!("id{i}"),
                    label: format!("claude-code id{i}"),
                    outcome,
                })
                .collect(),
        }
    }

    #[test]
    fn a_clean_run_says_nothing_about_skips_or_failures() {
        let r = report(vec![
            ItemOutcome::Ok { note: "archived".into() },
            ItemOutcome::Ok { note: "archived".into() },
        ]);
        assert_eq!(r.summary("Archive"), "Archive: 2 of 2 succeeded");
        assert!(r.problems().is_empty());
    }

    #[test]
    fn skips_are_not_counted_as_failures() {
        let r = report(vec![
            ItemOutcome::Ok { note: "n".into() },
            ItemOutcome::Skipped { reason: "jcode cannot be an import destination".into() },
        ]);
        assert_eq!(r.ok(), 1);
        assert_eq!(r.skipped(), 1);
        assert_eq!(r.failed(), 0);
        assert_eq!(r.summary("Import"), "Import: 1 of 2 succeeded, 1 skipped");
    }

    #[test]
    fn a_failure_partway_does_not_hide_the_successes_after_it() {
        // The batch continues past a failure, so "2 of 3" is not "it got
        // two in and stopped" — the third item ran too.
        let r = report(vec![
            ItemOutcome::Ok { note: "n".into() },
            ItemOutcome::Failed { error: "session is live".into() },
            ItemOutcome::Ok { note: "n".into() },
        ]);
        assert_eq!(r.summary("Delete"), "Delete: 2 of 3 succeeded, 1 failed");
        assert_eq!(r.problems(), vec!["claude-code id1: failed — session is live"]);
    }

    #[test]
    fn problems_lists_skips_and_failures_together_and_nothing_else() {
        let r = report(vec![
            ItemOutcome::Ok { note: "n".into() },
            ItemOutcome::Skipped { reason: "already imported".into() },
            ItemOutcome::Failed { error: "boom".into() },
        ]);
        assert_eq!(
            r.problems(),
            vec![
                "claude-code id1: skipped — already imported",
                "claude-code id2: failed — boom",
            ]
        );
    }

    #[test]
    fn destructive_actions_are_the_ones_that_lose_or_move_data() {
        assert!(BulkAction::Delete.is_destructive());
        assert!(BulkAction::Move { dir: "/tmp".into() }.is_destructive());
        assert!(!BulkAction::Archive.is_destructive());
        assert!(!BulkAction::Export { dir: "/tmp".into() }.is_destructive());
    }

    #[test]
    fn an_empty_batch_is_not_an_error() {
        let r = run(&[], &BulkAction::Archive);
        assert_eq!(r.items.len(), 0);
        assert_eq!(r.summary("Archive"), "Archive: 0 of 0 succeeded");
    }
}
