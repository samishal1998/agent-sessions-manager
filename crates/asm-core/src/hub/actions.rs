//! `push`, `pull`, and the listing that puts both machines side by side.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Serialize;

use super::bundle::{self, Bundle, InstallOutcome, Installed, Relation, Source};
use super::client::{PutOutcome, Remote};
use super::manifest::{Manifest, SCHEMA, files_hash, valid_id};
use super::state::{SyncState, Tracked};
use super::store::{Head, random_hex};
use crate::bulk::{BulkItem, BulkReport, ItemOutcome};
use crate::ir::PortablePath;
use crate::model::{AgentKind, Session, short_id_of};
use crate::{CoreError, fsutil, paths};

fn invalid(msg: impl Into<String>) -> CoreError {
    CoreError::Invalid { msg: msg.into() }
}

pub(super) fn key(agent: AgentKind, id: &str) -> String {
    format!("{agent}:{id}")
}

pub(super) fn scratch_dir(prefix: &str) -> Result<PathBuf, CoreError> {
    let dir = paths::tmp_dir()?.join(format!("{prefix}-{}", random_hex(8)?));
    std::fs::create_dir_all(&dir).map_err(|e| CoreError::io(&dir, e))?;
    Ok(dir)
}

/// Removes a scratch directory however the operation using it ends.
#[derive(Debug)]
pub(crate) struct Scratch(pub(crate) PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A project's git origin, asked of git once per directory per run.
pub(super) fn origin_of(root: &Path, origins: &mut HashMap<PathBuf, Option<String>>) -> Option<String> {
    if root.as_os_str().is_empty() {
        return None;
    }
    origins.entry(root.to_path_buf()).or_insert_with(|| crate::git::origin(root)).clone()
}

/// The key two machines agree on for "the same project": the repository's
/// origin when there is one, else the path with `${HOME}` tokenized.
fn project_key(root: &Path, origins: &mut HashMap<PathBuf, Option<String>>) -> String {
    origin_of(root, origins).unwrap_or_else(|| PortablePath::from_path(root).0)
}

pub(super) fn manifest_for(
    session: &Session,
    bundle: &Bundle,
    parent: Option<String>,
    origin: Option<String>,
) -> Manifest {
    Manifest {
        schema: SCHEMA,
        agent: session.handle.agent,
        id: session.handle.native_id.clone(),
        title: session.title.clone(),
        slug: session.slug.clone(),
        project_root: session.project_root.display().to_string(),
        project_root_portable: PortablePath::from_path(&session.project_root).0,
        git_origin: origin,
        git_branch: session.git_branch.clone(),
        agent_version: session.agent_version.clone(),
        parent: session.parent.clone(),
        created: session.created,
        updated: session.updated,
        machine: None,
        pushed_at: None,
        canonical: bundle.canonical.clone(),
        parent_rev: parent,
        files: bundle.files.iter().map(|f| f.entry.clone()).collect(),
        extra: bundle.extra.clone(),
    }
}

fn human(bytes: u64) -> String {
    crate::fmt::human_bytes(bytes)
}

/// Push each session, attempting every one: a failure never stops the rest.
///
/// A session is sent only when it changed since this machine last synced
/// it, and every push names the revision it is based on, so the hub refuses
/// one that would silently replace another machine's newer copy. `force`
/// names the hub's current head as the parent instead, making this copy the
/// head; the replaced one stays on the hub as a revision. `exact` (for a
/// move) reads every session and settles for nothing less than the hub
/// holding exactly this copy, sidecars included.
pub fn push(remote: &Remote, sessions: &[Session], force: bool, exact: bool) -> Result<BulkReport, CoreError> {
    push_collecting(remote, sessions, force, exact, &mut HashMap::new(), |_, _| {})
}

/// `push`, also saying which hub revision each session that went through is
/// now at (`key` → rev): the one just created, or the head that already held
/// exactly this copy. Remote control reports it so a pull can be pinned to it.
/// `progress(done, total)` is called after each session given.
pub fn push_collecting(
    remote: &Remote,
    sessions: &[Session],
    force: bool,
    exact: bool,
    revs: &mut HashMap<String, String>,
    mut progress: impl FnMut(usize, usize),
) -> Result<BulkReport, CoreError> {
    let heads: HashMap<String, Head> = remote
        .heads()?
        .into_iter()
        .map(|h| (key(h.manifest.agent, &h.manifest.id), h))
        .collect();
    // The same file named twice is one session. The same id filed in two
    // places is two copies, and pushing both would swap the hub's head
    // between them on every run — so is pushing either, which would back
    // up whichever a caller happened to list. Counted over everything here,
    // not only the batch: a daemon or a single row offers one copy.
    // Per agent in the batch, and a store that cannot be read only narrows
    // the check to the batch: it is no reason to refuse every push.
    let mut agents: Vec<AgentKind> = sessions.iter().map(|s| s.handle.agent).collect();
    agents.sort_by_key(|a| a.as_str());
    agents.dedup();
    let everything: Vec<Session> = agents
        .into_iter()
        .flat_map(|agent| {
            crate::ops::list_sessions(&crate::adapter::SessionFilter {
                agent: Some(agent),
                include_children: true,
                include_debug: true,
                ..Default::default()
            })
            .unwrap_or_default()
        })
        .collect();
    let mut places: HashMap<String, Vec<&crate::model::SessionLocation>> = HashMap::new();
    for s in sessions.iter().chain(&everything) {
        let here = places.entry(key(s.handle.agent, &s.handle.native_id)).or_default();
        if !here.contains(&&s.handle.location) {
            here.push(&s.handle.location);
        }
    }
    let mut state = SyncState::load(remote)?;
    let mut origins = HashMap::new();
    let mut report = BulkReport::default();
    let mut done = std::collections::HashSet::new();
    for (i, session) in sessions.iter().enumerate() {
        let (agent, id) = (session.handle.agent, &session.handle.native_id);
        let k = key(agent, id);
        if !done.insert(k.clone()) {
            progress(i + 1, sessions.len());
            continue;
        }
        let copies = places[&k].len();
        // An OpenCode subagent travels in its parent's bundle — if that
        // parent is here to carry it; an orphan goes as its own root.
        let carried = session.parent.as_ref().is_some_and(|p| {
            everything.iter().any(|s| s.handle.agent == AgentKind::OpenCode && &s.handle.native_id == p)
        });
        if agent == AgentKind::OpenCode && carried {
            report.items.push(BulkItem {
                agent,
                native_id: id.clone(),
                label: format!("{agent} {}", session.short_id()),
                outcome: ItemOutcome::Skipped { reason: "it travels with its parent session".into() },
            });
            progress(i + 1, sessions.len());
            continue;
        }
        let outcome = if copies > 1 {
            ItemOutcome::Failed {
                error: format!(
                    "session {id} exists in {copies} places here; resolve that first (asm \
                     doctor lists them)"
                ),
            }
        } else {
            push_one(remote, &mut state, &heads, &mut origins, session, force, exact, revs)
                .unwrap_or_else(|e| ItemOutcome::Failed { error: e.to_string() })
        };
        report.items.push(BulkItem {
            agent,
            native_id: id.clone(),
            label: format!("{agent} {}", session.short_id()),
            outcome,
        });
        progress(i + 1, sessions.len());
    }
    Ok(report)
}

fn in_sync() -> ItemOutcome {
    ItemOutcome::Ok { note: "in sync".into() }
}

// ponytail: eight arguments; fold them into a context struct if a ninth is needed.
#[allow(clippy::too_many_arguments)]
fn push_one(
    remote: &Remote,
    state: &mut SyncState,
    heads: &HashMap<String, Head>,
    origins: &mut HashMap<PathBuf, Option<String>>,
    session: &Session,
    force: bool,
    exact: bool,
    revs: &mut HashMap<String, String>,
) -> Result<ItemOutcome, CoreError> {
    let (agent, id) = (session.handle.agent, &session.handle.native_id);
    if !valid_id(id) {
        return Ok(ItemOutcome::Skipped { reason: "its id cannot be stored on a hub".into() });
    }
    let k = key(agent, id);
    let fingerprint = bundle::fingerprint(session);
    let tracked = state.get(&k).cloned();
    let head = heads.get(&k);

    // Nothing moved on either side: no need to read the session at all.
    // ponytail: the fingerprint covers what the agent writes on every turn
    // (a transcript, a database), so a sidecar that changes while that does
    // not waits for its next write. Claude Code writes them together (a tool
    // result and the record that names it); fold sidecar mtimes in if a case
    // turns up where it does not.
    if let (Some(t), Some(h)) = (&tracked, head)
        && !exact
        && t.fingerprint == fingerprint
        && t.hub_rev == h.rev
    {
        revs.insert(k.clone(), h.rev.clone());
        return Ok(in_sync());
    }

    let mut bundle = bundle::collect(session)?;
    let canonical = bundle.canonical.clone();
    let mut record = |state: &mut SyncState, rev: &str, files: String| {
        revs.insert(k.clone(), rev.to_string());
        let tracked = Tracked {
            hub_rev: rev.to_string(),
            canonical: canonical.clone(),
            fingerprint: fingerprint.clone(),
            files,
        };
        state.record(&k, tracked)
    };
    let files_now = |bundle: &Bundle| files_hash(bundle.files.iter().map(|f| &f.entry));
    let who = |h: &Head| h.manifest.machine.as_ref().map(|m| m.name.clone()).unwrap_or("another machine".into());
    // The head came from this machine: its last push reached the hub but
    // was never recorded here (a lost reply, a killed process). This copy
    // continues it, so it is a fast-forward, not a divergence with itself.
    let ours = |h: &Head| h.manifest.machine.as_ref().is_some_and(|m| m.id == remote.machine.id);
    // Whether the hub's head is this very copy. The listing leaves files
    // out, so unless `exact` the conversation alone decides; with it, the
    // head's own file list is fetched and must match too.
    let same_as = |h: &Head, bundle: &Bundle| -> Result<bool, CoreError> {
        if h.manifest.canonical != canonical {
            return Ok(false);
        }
        if !exact {
            return Ok(true);
        }
        let files = remote.history(agent.as_str(), id)?.map(|x| files_hash(&x.manifest.files));
        Ok(files.as_deref() == Some(files_now(bundle).as_str()))
    };

    let parent = match (&tracked, head) {
        (Some(t), Some(h)) if h.rev == t.hub_rev => {
            // With `exact`, the record is not trusted for the files: a
            // non-exact sync records this machine's list, not the hub's.
            if canonical == t.canonical && files_now(&bundle) == t.files && (!exact || same_as(h, &bundle)?) {
                record(state, &h.rev, t.files.clone())?;
                return Ok(in_sync());
            }
            Some(h.rev.clone())
        }
        (Some(t), Some(h)) => {
            if same_as(h, &bundle)? {
                record(state, &h.rev, files_now(&bundle))?;
                return Ok(in_sync());
            }
            // The hub moved only files since this machine synced (another
            // machine opened the session, or pushed a sidecar): the
            // conversation there is the one this copy continues.
            if h.manifest.canonical == t.canonical {
                Some(h.rev.clone())
            } else if !force && !ours(h) {
                if canonical == t.canonical {
                    return Ok(ItemOutcome::Skipped {
                        reason: format!("{} has a newer copy; `asm pull` it", who(h)),
                    });
                }
                return Ok(ItemOutcome::Failed {
                    error: format!(
                        "diverged: this machine and {} both continued it since they last \
                         synced. `asm push --force` makes this copy the head (the other stays \
                         on the hub as a revision)",
                        who(h)
                    ),
                });
            } else {
                Some(h.rev.clone())
            }
        }
        // Tracked here, gone from the hub (a new hub, or one reset): push
        // it as new.
        (Some(_), None) | (None, None) => None,
        (None, Some(h)) => {
            if same_as(h, &bundle)? {
                record(state, &h.rev, files_now(&bundle))?;
                return Ok(in_sync());
            }
            if !force && !ours(h) {
                return Ok(ItemOutcome::Failed {
                    error: format!(
                        "the hub already has this session from {}, with different content, and \
                         this machine has no record of syncing it. `asm pull` it, or `asm push \
                         --force` to make this copy the head",
                        who(h)
                    ),
                });
            }
            Some(h.rev.clone())
        }
    };

    // Upload what the hub does not have, then the manifest naming it.
    let shas: Vec<String> = bundle.files.iter().filter_map(|f| f.entry.sha256.clone()).collect();
    let missing: std::collections::HashSet<String> = remote.missing(&shas)?.into_iter().collect();
    let scratch = Scratch(scratch_dir("push")?);
    let mut uploaded = 0u64;
    let mut sent = std::collections::HashSet::new();
    for (n, file) in bundle.files.iter_mut().enumerate() {
        let Some(sha) = file.entry.sha256.clone() else { continue };
        if !missing.contains(&sha) {
            continue;
        }
        let tmp = scratch.0.join(n.to_string());
        match &file.source {
            // Copied, then the copy hashed and sent: a live session's
            // sidecar can grow between collecting and uploading, and what
            // is sent must be exactly what the manifest names.
            Source::Path(path) => {
                std::fs::copy(path, &tmp).map_err(|e| CoreError::io(path, e))?;
                let size = std::fs::metadata(&tmp).map_err(|e| CoreError::io(&tmp, e))?.len();
                file.entry.sha256 = Some(fsutil::sha256_file(&tmp)?);
                file.entry.size = size;
            }
            Source::Bytes(bytes) => std::fs::write(&tmp, bytes).map_err(|e| CoreError::io(&tmp, e))?,
            Source::Symlink => continue,
        }
        let sha = file.entry.sha256.clone().unwrap_or(sha);
        if sent.insert(sha.clone()) {
            remote.put_blob(&sha, &tmp)?;
            uploaded += file.entry.size;
        }
    }

    let origin = origin_of(&session.project_root, origins);
    let manifest = manifest_for(session, &bundle, parent, origin);
    match remote.put_revision(&manifest)? {
        PutOutcome::Created { rev } => {
            record(state, &rev, files_now(&bundle))?;
            Ok(ItemOutcome::Ok {
                note: format!(
                    "pushed {} ({} uploaded)",
                    human(manifest.total_size()),
                    human(uploaded)
                ),
            })
        }
        PutOutcome::Conflict { .. } => Ok(ItemOutcome::Failed {
            error: "another machine pushed it while this one was; run push again".into(),
        }),
    }
}

/// Resolve what the user typed against the hub's listing: an id, a unique
/// id prefix, `agent:prefix`, or the memorable name an agent uses.
pub fn resolve_head<'a>(heads: &'a [Head], query: &str) -> Result<&'a Head, CoreError> {
    let (agent, needle) = match query.split_once(':') {
        Some((a, rest)) if AgentKind::parse(a).is_some() => (AgentKind::parse(a), rest),
        _ => (None, query),
    };
    let candidates: Vec<&Head> = heads
        .iter()
        .filter(|h| agent.is_none_or(|a| h.manifest.agent == a))
        .filter(|h| {
            h.manifest.id.starts_with(needle) || h.manifest.slug.as_deref() == Some(needle)
        })
        .collect();
    if let Some(exact) = candidates.iter().find(|h| h.manifest.id == needle) {
        return Ok(exact);
    }
    match candidates.as_slice() {
        [one] => Ok(one),
        [] => Err(invalid(format!("no session on the hub matches {query:?}"))),
        many => Err(invalid(format!(
            "{query:?} matches {} sessions on the hub: {}",
            many.len(),
            many.iter().map(|h| key(h.manifest.agent, &h.manifest.id)).collect::<Vec<_>>().join(", ")
        ))),
    }
}

#[derive(Debug, Serialize)]
pub struct Pulled {
    pub agent: AgentKind,
    pub id: String,
    pub title: Option<String>,
    pub slug: Option<String>,
    /// The machine that pushed this revision.
    pub from: Option<String>,
    #[serde(flatten)]
    pub installed: Installed,
}

/// Bring one session from the hub onto this machine.
pub fn pull(remote: &Remote, query: &str, project_dir: Option<&Path>) -> Result<Pulled, CoreError> {
    let heads = remote.heads()?;
    pull_head(remote, resolve_head(&heads, query)?, project_dir)
}

/// Whether a session on the hub is one the filters ask for. A hub session
/// belongs to a project by where it lives on THIS machine — the pushing
/// machine's path resolved against this home — or by sharing its git
/// origin, so the same repository checked out elsewhere still matches.
/// `projects` empty means any project; otherwise any one of them will do.
fn head_matches(head: &Head, agent: Option<AgentKind>, projects: &[PathBuf]) -> bool {
    let m = &head.manifest;
    if agent.is_some_and(|a| a != m.agent) {
        return false;
    }
    if projects.is_empty() {
        return true;
    }
    let here = PortablePath(m.project_root_portable.clone()).resolve();
    projects.iter().any(|project| {
        here.starts_with(project)
            || match (&m.git_origin, crate::git::origin(project)) {
                (Some(theirs), Some(ours)) => *theirs == ours,
                _ => false,
            }
    })
}

/// Pull everything on the hub that the filters match, each session at its
/// own place. Every one is attempted: a session with nowhere to land here,
/// or one that diverged, is listed per session and stops nothing else.
/// `progress(done, total)` is called after each session.
pub fn pull_all(
    remote: &Remote,
    agent: Option<AgentKind>,
    projects: &[PathBuf],
    mut progress: impl FnMut(usize, usize),
) -> Result<BulkReport, CoreError> {
    let heads: Vec<Head> =
        remote.heads()?.into_iter().filter(|h| head_matches(h, agent, projects)).collect();
    let mut report = BulkReport::default();
    for head in &heads {
        let (agent, id) = (head.manifest.agent, head.manifest.id.clone());
        let outcome = match pull_head(remote, head, None) {
            Ok(pulled) => match pulled.installed.outcome {
                InstallOutcome::Diverged => ItemOutcome::Failed {
                    error: "diverged: continued here and on the other machine; \
                            `asm pull <id>` says more"
                        .into(),
                },
                InstallOutcome::InSync => ItemOutcome::Skipped { reason: "already in sync".into() },
                InstallOutcome::Ahead => ItemOutcome::Skipped {
                    reason: "this machine is ahead of the hub; `asm push` it".into(),
                },
                InstallOutcome::New => ItemOutcome::Ok { note: "installed".into() },
                InstallOutcome::FastForward { appended } => {
                    ItemOutcome::Ok { note: format!("updated, {} appended", human(appended)) }
                }
                InstallOutcome::Replaced => {
                    ItemOutcome::Ok { note: "updated, the older copy backed up".into() }
                }
                InstallOutcome::Renamed => ItemOutcome::Ok { note: "renamed to match".into() },
            },
            Err(e) => ItemOutcome::Failed { error: e.to_string() },
        };
        report.items.push(BulkItem {
            agent,
            native_id: id,
            label: format!(
                "{agent} {}",
                short_id_of(agent, &head.manifest.id, head.manifest.slug.as_deref())
            ),
            outcome,
        });
        progress(report.items.len(), heads.len());
    }
    Ok(report)
}

/// Bring the session this head describes onto this machine.
pub fn pull_head(
    remote: &Remote,
    head: &Head,
    project_dir: Option<&Path>,
) -> Result<Pulled, CoreError> {
    pull_head_rev(remote, head, project_dir, None)
}

/// `pull_head`, installing the named revision instead of the head, and
/// recording that revision as the one this machine last synced. A pull
/// pinned this way is exactly what was asked for even if another machine has
/// pushed since — and the next push from here is then refused as based on an
/// old revision, rather than silently replacing the newer copy.
pub fn pull_head_rev(
    remote: &Remote,
    head: &Head,
    project_dir: Option<&Path>,
    rev: Option<&str>,
) -> Result<Pulled, CoreError> {
    let (agent, id) = (head.manifest.agent, head.manifest.id.clone());
    if !bundle::restorable(agent) {
        return Err(invalid(format!(
            "{agent} sessions are backed up on the hub, but asm cannot restore them onto a \
             machine yet"
        )));
    }
    let history = remote
        .history(agent.as_str(), &id)?
        .ok_or_else(|| invalid(format!("{} is no longer on the hub", key(agent, &id))))?;
    let pinned;
    let (manifest, hub_rev) = match rev {
        Some(r) if r != history.head => {
            pinned = remote
                .revision(agent.as_str(), &id, r)?
                .ok_or_else(|| invalid(format!("revision {r} of {} is not on the hub", key(agent, &id))))?;
            (&pinned, r.to_string())
        }
        _ => (&history.manifest, history.head.clone()),
    };

    let scratch = Scratch(scratch_dir("pull")?);
    let blob = |sha: &str| -> Result<PathBuf, CoreError> {
        let path = scratch.0.join(sha);
        if !path.is_file() {
            remote.get_blob(sha, &path)?;
        }
        Ok(path)
    };
    // What this machine last synced of it, so each side's changes since
    // can be told apart. Losing the record only loses that refinement.
    let base = match SyncState::load(remote)?.get(&key(agent, &id)) {
        Some(t) => Some(bundle::Base {
            canonical: t.canonical.clone(),
            files: remote
                .revision(agent.as_str(), &id, &t.hub_rev)?
                .map(|m| m.files.into_iter().filter_map(|f| Some((f.path, f.sha256?))).collect())
                .unwrap_or_default(),
        }),
        None => None,
    };
    let installed = super::peer::install_bundle(manifest, &blob, project_dir, base.as_ref())?;

    if installed.outcome != InstallOutcome::Diverged {
        // Recorded after installing, from the installed file: an install
        // rewrites the file's mtime, and a fingerprint from before it would
        // make the next push re-send what was just pulled. A copy that is
        // ahead records no fingerprint, so the next push looks at it.
        let fingerprint = if installed.outcome == InstallOutcome::Ahead {
            String::new()
        } else {
            crate::ops::resolve_ref(&key(agent, &id), &Default::default())
                .map(|s| bundle::fingerprint(&s))
                .unwrap_or_default()
        };
        SyncState::load(remote)?.record(
            &key(agent, &id),
            Tracked {
                hub_rev,
                canonical: manifest.canonical.clone(),
                fingerprint,
                files: files_hash(&manifest.files),
            },
        )?;
    }
    Ok(Pulled {
        agent,
        id,
        title: manifest.title.clone(),
        slug: manifest.slug.clone(),
        from: manifest.machine.as_ref().map(|m| m.name.clone()),
        installed,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RowState {
    InSync,
    /// Changed here since the last sync; push it.
    Ahead,
    /// Changed on the hub since the last sync; pull it.
    Behind,
    Diverged,
    /// On this machine, never pushed.
    Local,
    /// On the hub, not on this machine.
    Remote,
    /// On both, but this machine has no record of syncing it.
    Untracked,
}

/// What a person should do about a session to bring it level with the hub.
/// One definition, read by the CLI table and both UIs: they had three, and
/// they disagreed about what to call each state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncAction {
    Push,
    Pull,
    /// Both sides moved; neither push nor pull is safe without a choice.
    Resolve,
}

impl RowState {
    /// Where a session stands once its content has shown which side is ahead.
    fn of_relation(relation: Relation) -> RowState {
        match relation {
            Relation::Same => RowState::InSync,
            Relation::LocalAhead => RowState::Ahead,
            Relation::HubAhead => RowState::Behind,
            Relation::Diverged => RowState::Diverged,
        }
    }

    pub fn action(self) -> Option<SyncAction> {
        match self {
            RowState::InSync => None,
            RowState::Ahead | RowState::Local => Some(SyncAction::Push),
            // Untracked is "pull" because pulling is what finds out: it
            // says in sync, ahead, newer or diverged, and records the base.
            // (A listing reads a small hub copy itself, and then the row is
            // not Untracked but Ahead, Behind or Diverged.)
            RowState::Behind | RowState::Remote | RowState::Untracked => Some(SyncAction::Pull),
            RowState::Diverged => Some(SyncAction::Resolve),
        }
    }

    /// A few words, for a column or a pill.
    pub fn label(self) -> &'static str {
        match self {
            RowState::InSync => "Synced",
            RowState::Ahead => "Needs push",
            RowState::Local => "Not on hub",
            RowState::Behind => "Needs pull",
            RowState::Remote => "New on hub",
            RowState::Diverged => "Diverged",
            RowState::Untracked => "Not compared",
        }
    }

    /// A sentence, for a tooltip or a status line.
    pub fn hint(self) -> &'static str {
        match self {
            RowState::InSync => "The hub has exactly this copy.",
            RowState::Ahead => "Changed here since the last sync. Push it.",
            RowState::Local => "The hub does not have this session yet. Push it.",
            RowState::Behind => "Another machine pushed a newer copy. Pull it.",
            RowState::Remote => "On the hub, not on this machine. Pull it.",
            RowState::Diverged => {
                "Continued both here and on another machine. `asm push --force` makes this copy \
                 the head; `asm pull <id>` says more."
            }
            RowState::Untracked => {
                "On the hub and here, but this machine has never compared the two. Pull it: identical \
                 copies become Synced, a hub copy that extends yours is applied, and one that moved \
                 on both sides is reported as diverged."
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    /// The project both machines agree on: the git origin, else the path.
    pub project: String,
    pub agent: AgentKind,
    pub id: String,
    pub short_id: String,
    pub title: Option<String>,
    /// The machine that pushed the hub's copy, or this one when it is only
    /// here.
    pub machine: String,
    pub updated: Option<Timestamp>,
    pub state: RowState,
    pub action: Option<SyncAction>,
    pub label: &'static str,
    pub hint: &'static str,
    /// Whether `asm pull` can bring it here.
    pub restorable: bool,
    #[serde(flatten)]
    pub detail: RowDetail,
}

/// What a detailed view shows beyond the state: where the session is, how
/// big, and which hub revision. For a hub row these describe the hub's copy
/// (as the pushing machine saw it); for a local-only row, this machine's.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RowDetail {
    /// This machine has the session.
    pub here: bool,
    /// The hub has it.
    pub on_hub: bool,
    pub size: Option<u64>,
    /// The hub's head revision, shortened.
    pub rev: Option<String>,
    pub pushed_at: Option<Timestamp>,
    pub branch: Option<String>,
    /// The project directory on the machine the copy came from.
    pub project_root: String,
    pub agent_version: Option<String>,
    /// The state's own name, whatever this row's label says: what a filter
    /// chip for the state is called.
    pub state_label: &'static str,
    /// The state's own explanation, for the same chip.
    pub state_hint: &'static str,
    /// What a shallow comparison found, for a session the two sides have but
    /// this machine never compared.
    pub compare: Option<Compare>,
}

/// How a session here relates to the hub's copy, found without downloading
/// or installing anything: this machine reads and hashes its own copy and
/// compares the conversation's identity with what the hub recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Shallow {
    /// Same conversation: nothing to push or pull.
    Identical,
    /// Different. Which side is ahead needs the content: `Compare::relation`
    /// has it when the hub's copy was read.
    Differs,
}

/// A guess at which side is newer, from the two copies' `updated` times and
/// sizes when both point the same way. ADVISORY: it is never a decision. The
/// times are each machine's own clock and the sizes cover different files, so
/// it only says what to look at first, and it never changes a row's state or
/// what the row offers to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Lean {
    Local,
    Hub,
}

/// `Local` when this machine's copy was updated after the hub's and is not
/// smaller; `Hub` the other way round; `None` when a time is missing, the
/// times are equal, or the size points against the time.
fn lean_of(local_updated: Option<Timestamp>, hub_updated: Option<Timestamp>, local_size: u64, hub_size: u64) -> Option<Lean> {
    match local_updated?.cmp(&hub_updated?) {
        std::cmp::Ordering::Greater if local_size >= hub_size => Some(Lean::Local),
        std::cmp::Ordering::Less if hub_size >= local_size => Some(Lean::Hub),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Compare {
    pub verdict: Shallow,
    pub local_size: u64,
    pub hub_size: u64,
    /// Which side is ahead, proven by reading the hub's copy and comparing it
    /// the way a pull would. `None` when it was not read (the hub's copy is
    /// over the size a listing reads, the agent has no such comparison, the
    /// hub did not answer), and then nothing is known beyond `verdict`.
    pub relation: Option<Relation>,
    /// Only when `relation` is `None`: the advisory guess, see [`Lean`].
    pub lean: Option<Lean>,
}

/// Sessions compared per listing. Each is read and hashed once, then
/// remembered, so a machine with thousands of uncompared sessions settles
/// over a few listings instead of stalling the first.
const COMPARE_BUDGET: usize = 25;

/// The largest hub copy a listing downloads to say which side is ahead of an
/// uncompared session: only the one file the agent compares (the transcript),
/// but a listing may meet 25 of them, so it stays small. A larger one is left
/// at "Differs" until it is compared on its own.
const RELATE_LISTING_MAX: u64 = 2 * 1024 * 1024;

/// The same for one session asked about by name (the web's Compare button).
const RELATE_COMPARE_MAX: u64 = 256 * 1024 * 1024;

/// Results already worked out, one per session: the same pair of copies gives
/// the same answer until either side changes, and a changed one replaces the
/// entry rather than adding to it. The flag says the hub's copy was read, or
/// could never be, so the answer is final; without it the entry only stands
/// for a caller that would not read the hub's copy either.
type CompareCache = std::sync::Mutex<HashMap<String, (String, Compare, bool)>>;

fn compare_cache() -> &'static CompareCache {
    static CACHE: std::sync::OnceLock<CompareCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// What a pair of copies is identified by: this side's fingerprint and the
/// hub's revision.
fn pair_key(session: &Session, head: &Head) -> (String, String) {
    (
        key(session.handle.agent, &session.handle.native_id),
        format!("{}|{}", bundle::fingerprint(session), head.rev),
    )
}

/// A result worked out before, if neither side has changed since, and it is
/// one this caller would reach too: either the hub's copy was read, or it is
/// over what the caller reads (`cap`), so reading again could not add to it.
fn cached_compare(session: &Session, head: &Head, cap: u64) -> Option<Compare> {
    let (k, pair) = pair_key(session, head);
    let cache = compare_cache().lock().ok()?;
    let (p, found, settled) = cache.get(&k)?;
    (*p == pair && (*settled || head.total_size > cap)).then_some(*found)
}

/// The shallow verdict from what both sides recorded. Only equal identities
/// prove anything. Sizes are shown, never read as direction: the two sides'
/// file sets differ (sidecars, subagent transcripts), so "bigger" does not
/// mean "newer".
pub(crate) fn verdict_of(local: &str, hub: &str) -> Shallow {
    if local == hub { Shallow::Identical } else { Shallow::Differs }
}

/// What an uncompared session is called once it has been looked at.
fn compared_label(state: RowState, compare: Option<Compare>) -> &'static str {
    match (state, compare.map(|c| c.verdict)) {
        (RowState::Untracked, Some(Shallow::Differs)) => "Differs",
        _ => state.label(),
    }
}

fn compared_hint(state: RowState, compare: Option<Compare>) -> &'static str {
    let found = compare.and_then(|c| c.relation);
    let lean = compare.and_then(|c| c.lean);
    match (state, found, lean, compare.map(|c| c.verdict)) {
        // Read from the hub's copy, not from a record of the last sync.
        (RowState::Ahead, Some(Relation::LocalAhead), ..) => {
            "This machine's copy goes on from the hub's: the hub has an earlier part of it. Push it."
        }
        (RowState::Behind, Some(Relation::HubAhead), ..) => {
            "The hub's copy goes on from this machine's: this machine has an earlier part of it. Pull it."
        }
        (RowState::Diverged, Some(Relation::Diverged), ..) => {
            "Each side has something the other lacks. `asm push --force` makes this copy the head; \
             `asm pull <id>` says more."
        }
        // Unproven: only a guess from times and sizes, said to be one.
        (RowState::Untracked, _, Some(Lean::Local), _) => {
            "The two copies differ. This machine's copy looks newer (updated later than the hub's, and not smaller), \
             but that is only a guess. Pull it to confirm: nothing changes if this copy is ahead."
        }
        (RowState::Untracked, _, Some(Lean::Hub), _) => {
            "The two copies differ. The hub's copy looks newer (updated later than this machine's, and not smaller), \
             but that is only a guess. Pull it to confirm."
        }
        (RowState::Untracked, _, _, Some(Shallow::Differs)) => {
            "The two copies differ, and which is ahead takes the content to tell. Pull it: a hub copy that extends yours is applied, a copy that is ahead of the hub's can then be pushed, and one that moved on both sides is reported as diverged."
        }
        _ => state.hint(),
    }
}

/// Read this machine's copy and compare it with the hub's head. Identical
/// copies are recorded as synced, exactly as a push or pull would have.
/// A size is a hint, never a verdict: only `Identical` is a proof.
///
/// A copy that differs has the hub's copy read (the one file the agent
/// compares, up to [`RELATE_COMPARE_MAX`]) and related to this one by the same
/// comparison a pull makes, so the answer says which side is ahead. A copy
/// found ahead is recorded the way a pull records one, so a push from here has
/// the hub's revision as its parent. When the hub's copy cannot be read the
/// answer is the shallow one, not an error.
pub fn compare_session(
    remote: &Remote,
    state: &mut SyncState,
    session: &Session,
    head: &Head,
) -> Result<Compare, CoreError> {
    compare_within(remote, state, session, head, RELATE_COMPARE_MAX, &std::cell::Cell::new(false))
}

/// `compare_session`, reading the hub's copy only up to `cap` bytes.
/// `unreachable` is set when a request to the hub failed, so a listing can stop
/// asking one that is not answering.
fn compare_within(
    remote: &Remote,
    state: &mut SyncState,
    session: &Session,
    head: &Head,
    cap: u64,
    unreachable: &std::cell::Cell<bool>,
) -> Result<Compare, CoreError> {
    if let Some(found) = cached_compare(session, head, cap) {
        return Ok(found);
    }
    let (k, pair) = pair_key(session, head);
    let fingerprint = bundle::fingerprint(session);
    let bundle = bundle::collect(session)?;
    let local_size = bundle.files.iter().map(|f| f.entry.size).sum();
    let mut found = Compare {
        verdict: verdict_of(&bundle.canonical, &head.manifest.canonical),
        local_size,
        hub_size: head.total_size,
        relation: None,
        lean: None,
    };
    // Recording is what makes this permanent, but the answer is true
    // whether or not it could be written (a read-only data dir): the
    // cache below keeps it from being worked out again each listing.
    let record_synced = |state: &mut SyncState| {
        let _ = state.record(
            &k,
            Tracked {
                hub_rev: head.rev.clone(),
                canonical: bundle.canonical.clone(),
                fingerprint: fingerprint.clone(),
                files: files_hash(bundle.files.iter().map(|f| &f.entry)),
            },
        );
    };
    // Whether the hub's copy was read, or could never be: the answer is then final.
    let mut settled = false;
    if found.verdict == Shallow::Identical {
        record_synced(state);
        found.relation = Some(Relation::Same);
        settled = true;
    } else if head.total_size <= cap && !unreachable.get() {
        // ponytail: a blob download has no wall-clock limit, only a floor on
        // throughput (client::Profile::Transfer), and `history` is always the
        // Control profile (60 s, 3 retries) even for a quick `hub_status`. A
        // hub that lists sessions but stalls on these can hold one listing for
        // a minute or more before `unreachable` stops the rest; the size cap
        // keeps that rare. A deadline on both if it is seen.
        if let Ok((relation, hub_files)) = read_hub_copy(remote, session, head, unreachable) {
            settled = true;
            found.relation = relation;
            match relation {
                Some(Relation::Same) => record_synced(state),
                // Exactly what a pull records for a copy that is ahead: the
                // hub's revision and conversation, no fingerprint (so the
                // next push looks at this copy), the hub's files. A push
                // from here then names that revision as its parent.
                Some(Relation::LocalAhead) => {
                    let recorded = state.record(
                        &k,
                        Tracked {
                            hub_rev: head.rev.clone(),
                            canonical: head.manifest.canonical.clone(),
                            fingerprint: String::new(),
                            files: files_hash(&hub_files),
                        },
                    );
                    // Without the record a push is refused as unknown, so
                    // "needs push" would be advice that cannot be followed.
                    if recorded.is_err() {
                        found.relation = None;
                    }
                }
                _ => {}
            }
        }
    }
    if found.relation.is_none() {
        found.lean = lean_of(session.updated, head.manifest.updated, local_size, head.total_size);
    }
    if let Ok(mut c) = compare_cache().lock() {
        c.insert(k, (pair, found, settled));
    }
    Ok(found)
}

/// The hub's copy of a session related to this machine's, and the files that
/// copy holds. Reads only what the agent's comparison needs.
fn read_hub_copy(
    remote: &Remote,
    session: &Session,
    head: &Head,
    unreachable: &std::cell::Cell<bool>,
) -> Result<(Option<Relation>, Vec<super::manifest::FileEntry>), CoreError> {
    let (agent, id) = (head.manifest.agent, &head.manifest.id);
    let history = remote
        .history(agent.as_str(), id)
        .inspect_err(|_| unreachable.set(true))?
        .ok_or_else(|| invalid(format!("{} is no longer on the hub", key(agent, id))))?;
    if history.head != head.rev {
        return Err(invalid(format!("{} moved on the hub while it was being compared", key(agent, id))));
    }
    let scratch = Scratch(scratch_dir("compare")?);
    let blob = |sha: &str| -> Result<PathBuf, CoreError> {
        let path = scratch.0.join(sha);
        if !path.is_file() {
            remote.get_blob(sha, &path).inspect_err(|_| unreachable.set(true))?;
        }
        Ok(path)
    };
    let relation = bundle::relate(session, &history.manifest, &blob)?;
    Ok((relation, history.manifest.files))
}

/// Every session on this machine and on the hub, grouped by project. The
/// same session on both is one row, with how the two copies relate.
pub fn remote_list(remote: &Remote, local: &[Session]) -> Result<Vec<Row>, CoreError> {
    let heads = remote.heads()?;
    let mut state = SyncState::load(remote)?;
    let mut budget = COMPARE_BUDGET;
    let unreachable = std::cell::Cell::new(false);
    let mut origins = HashMap::new();
    let by_key: HashMap<String, &Session> =
        local.iter().map(|s| (key(s.handle.agent, &s.handle.native_id), s)).collect();

    let mut rows = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for head in &heads {
        let m = &head.manifest;
        let k = key(m.agent, &m.id);
        seen.insert(k.clone());
        let mut compare = None;
        let row_state = match (by_key.get(&k), state.get(&k).cloned()) {
            (None, _) => RowState::Remote,
            (Some(s), None) => {
                // Both sides have it and this machine never compared them:
                // look, rather than leave the question to a pull. Anything
                // already worked out is free; new reads are budgeted. A small
                // hub copy is read too, to say which side is ahead; once the
                // hub fails to answer, none is.
                let found = cached_compare(s, head, RELATE_LISTING_MAX).or_else(|| {
                    (budget > 0).then(|| {
                        budget -= 1;
                        compare_within(remote, &mut state, s, head, RELATE_LISTING_MAX, &unreachable).ok()
                    })?
                });
                compare = found;
                match found {
                    Some(c) if c.verdict == Shallow::Identical => RowState::InSync,
                    Some(Compare { relation: Some(relation), .. }) => RowState::of_relation(relation),
                    _ => RowState::Untracked,
                }
            }
            (Some(s), Some(t)) => {
                let changed_here = bundle::fingerprint(s) != t.fingerprint;
                let moved_there = head.rev != t.hub_rev;
                match (changed_here, moved_there) {
                    (false, false) => RowState::InSync,
                    (true, false) => RowState::Ahead,
                    (false, true) => RowState::Behind,
                    (true, true) => RowState::Diverged,
                }
            }
        };
        rows.push(Row {
            project: m.git_origin.clone().unwrap_or_else(|| m.project_root_portable.clone()),
            agent: m.agent,
            id: m.id.clone(),
            short_id: short_id_of(m.agent, &m.id, m.slug.as_deref()).to_string(),
            title: m.title.clone(),
            machine: m.machine.as_ref().map(|x| x.name.clone()).unwrap_or_default(),
            updated: m.updated,
            state: row_state,
            action: row_state.action(),
            label: compared_label(row_state, compare),
            hint: compared_hint(row_state, compare),
            restorable: bundle::restorable(m.agent),
            detail: RowDetail {
                here: by_key.contains_key(&k),
                on_hub: true,
                size: Some(head.total_size),
                rev: Some(head.rev.chars().take(8).collect()),
                pushed_at: m.pushed_at,
                branch: m.git_branch.clone(),
                project_root: m.project_root.clone(),
                agent_version: m.agent_version.clone(),
                compare,
                state_label: row_state.label(),
                state_hint: row_state.hint(),
            },
        });
    }
    for s in local {
        let k = key(s.handle.agent, &s.handle.native_id);
        if seen.contains(&k) {
            continue;
        }
        rows.push(Row {
            project: project_key(&s.project_root, &mut origins),
            agent: s.handle.agent,
            id: s.handle.native_id.clone(),
            short_id: s.short_id().to_string(),
            title: s.title.clone(),
            machine: remote.machine.name.clone(),
            updated: s.updated,
            state: RowState::Local,
            action: RowState::Local.action(),
            label: RowState::Local.label(),
            hint: RowState::Local.hint(),
            restorable: bundle::restorable(s.handle.agent),
            detail: RowDetail {
                here: true,
                on_hub: false,
                size: s.size_bytes,
                rev: None,
                pushed_at: None,
                branch: s.git_branch.clone(),
                project_root: s.project_root.display().to_string(),
                agent_version: s.agent_version.clone(),
                compare: None,
                state_label: RowState::Local.label(),
                state_hint: RowState::Local.hint(),
            },
        });
    }
    rows.sort_by(|a, b| a.project.cmp(&b.project).then(b.updated.cmp(&a.updated)));
    Ok(rows)
}

/// How many sessions want each kind of attention: what a status line says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub synced: usize,
    pub to_push: usize,
    pub to_pull: usize,
    pub to_resolve: usize,
    /// On the hub and nowhere on this machine (a subset of `to_pull`).
    pub new_on_hub: usize,
}

pub fn summarize(rows: &[Row]) -> Summary {
    let mut s = Summary::default();
    for row in rows {
        match row.action {
            None => s.synced += 1,
            Some(SyncAction::Push) => s.to_push += 1,
            Some(SyncAction::Pull) => s.to_pull += 1,
            Some(SyncAction::Resolve) => s.to_resolve += 1,
        }
        if row.state == RowState::Remote {
            s.new_on_hub += 1;
        }
    }
    s
}

/// Everything a UI needs to say how this machine stands with the hub, in
/// one call: whether there is a hub, whether it answered, and what is out
/// of step. An unreachable hub is a status, not an error — the UI shows it
/// and offers a retry.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HubStatus {
    pub joined: bool,
    pub connected: bool,
    pub url: Option<String>,
    /// This machine's name on the hub.
    pub machine: Option<String>,
    /// Why it is not connected, and what to try — not curl's stderr.
    pub error: Option<String>,
    /// What actually went wrong, for a tooltip or a bug report.
    pub detail: Option<String>,
    pub rows: Vec<Row>,
    pub summary: Summary,
    /// Every machine that has joined the hub, this one included.
    pub machines: Vec<super::store::Machine>,
}

pub fn hub_status(local: &[Session]) -> HubStatus {
    if !super::client::is_joined() {
        return HubStatus::default();
    }
    let remote = match super::client::load() {
        Ok(mut remote) => {
            remote.quick = true;
            remote
        }
        Err(e) => {
            let raw = e.to_string();
            return HubStatus { joined: true, error: Some(raw.clone()), detail: Some(raw), ..Default::default() };
        }
    };
    let mut status = HubStatus {
        joined: true,
        url: Some(remote.url.clone()),
        machine: Some(remote.machine.name.clone()),
        ..Default::default()
    };
    match remote_list(&remote, local) {
        Ok(rows) => {
            status.connected = true;
            status.summary = summarize(&rows);
            status.rows = rows;
            // Nice to have: a failure here must not turn a good answer into
            // an error.
            status.machines = remote.machines().unwrap_or_default();
        }
        Err(e) => {
            let raw = e.to_string();
            // A refusal already says what to do (`asm join` again). A
            // connection failure says only what curl saw, so say the rest.
            status.error = Some(if raw.starts_with("could not reach the hub") {
                format!(
                    "Could not connect to {}. Is `asm hub serve` running there, and can this \
                     machine reach it?",
                    host_of(&remote.url)
                )
            } else {
                raw.clone()
            });
            status.detail = Some(raw);
        }
    }
    status
}

/// `host:port` out of a URL, for a sentence.
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::manifest::Manifest;

    /// A pushed subagent carries the id of the session it belongs to.
    #[test]
    fn a_pushed_manifest_names_the_sessions_parent() {
        let loc = crate::model::SessionLocation::JsonlFile { path: "/x".into() };
        let mut child = crate::hub::bundle::session_at(AgentKind::OpenCode, "ses_kid", loc);
        let bundle = Bundle { files: Vec::new(), canonical: "c".into(), extra: serde_json::Value::Null };
        assert_eq!(manifest_for(&child, &bundle, None, None).parent, None);
        child.parent = Some("ses_boss".into());
        assert_eq!(manifest_for(&child, &bundle, None, None).parent.as_deref(), Some("ses_boss"));
    }

    fn head(agent: AgentKind, id: &str, slug: Option<&str>) -> Head {
        Head {
            rev: "r".into(),
            file_count: 0,
            total_size: 0,
            manifest: Manifest {
                schema: SCHEMA,
                agent,
                id: id.into(),
                title: None,
                slug: slug.map(String::from),
                project_root: String::new(),
                project_root_portable: String::new(),
                git_origin: None,
                git_branch: None,
                agent_version: None,
                parent: None,
                created: None,
                updated: None,
                machine: None,
                pushed_at: None,
                canonical: String::new(),
                parent_rev: None,
                files: vec![],
                extra: serde_json::Value::Null,
            },
        }
    }

    #[test]
    fn a_hub_session_resolves_by_id_prefix_agent_and_memorable_name() {
        let heads = vec![
            head(AgentKind::ClaudeCode, "7f3a1c88-aaaa", None),
            head(AgentKind::ClaudeCode, "7f3b0000-bbbb", None),
            head(AgentKind::JCode, "session_boar_17887_d6ef", Some("boar")),
        ];
        assert_eq!(resolve_head(&heads, "7f3a").unwrap().manifest.id, "7f3a1c88-aaaa");
        assert!(resolve_head(&heads, "7f3").is_err(), "ambiguous prefix");
        assert_eq!(resolve_head(&heads, "boar").unwrap().manifest.agent, AgentKind::JCode);
        assert!(resolve_head(&heads, "codex:7f3a").is_err());
        assert!(resolve_head(&heads, "nothing").is_err());
    }

    /// The hub's sessions are filtered the way local ones are: by agent,
    /// and by project — where the session would live on THIS machine, or
    /// the same repository by its origin. Everything here lives in a temp
    /// directory: no `${HOME}` token, so nothing resolves into a real home.
    #[test]
    fn hub_sessions_are_matched_by_agent_and_project() {
        let dir = tempfile::tempdir().unwrap();
        let mine = dir.path().join("code/mercury");
        let at = |agent, id: &str, root: &std::path::Path| {
            let mut h = head(agent, id, None);
            h.manifest.project_root_portable = root.display().to_string();
            h
        };
        let in_mercury = at(AgentKind::ClaudeCode, "a", &mine.join("crates/core"));
        let at_tls = at(AgentKind::OpenCode, "b", &dir.path().join("code/mercury-tls"));

        assert!(head_matches(&in_mercury, None, &[]), "no filter matches everything");
        assert!(head_matches(&in_mercury, Some(AgentKind::ClaudeCode), &[]));
        assert!(!head_matches(&in_mercury, Some(AgentKind::OpenCode), &[]));

        // A subdirectory of the project is in it; a sibling that merely
        // shares a prefix is not.
        assert!(head_matches(&in_mercury, None, std::slice::from_ref(&mine)));
        assert!(!head_matches(&at_tls, None, std::slice::from_ref(&mine)));
        // Both filters have to hold.
        assert!(!head_matches(&in_mercury, Some(AgentKind::OpenCode), std::slice::from_ref(&mine)));

        // The same repository checked out somewhere else still matches.
        let origin = "https://example.com/acme/mercury.git";
        let clone = dir.path().join("elsewhere");
        for repo in [&mine, &clone] {
            std::fs::create_dir_all(repo).unwrap();
            for args in [vec!["init", "-q"], vec!["remote", "add", "origin", origin]] {
                std::process::Command::new("git").arg("-C").arg(repo).args(args).status().unwrap();
            }
        }
        let mut cloned = at(AgentKind::JCode, "c", std::path::Path::new("/nowhere/at/all"));
        cloned.manifest.git_origin = crate::git::origin(&clone);
        assert!(cloned.manifest.git_origin.is_some(), "git is available to this test");
        assert!(head_matches(&cloned, None, std::slice::from_ref(&mine)), "same origin, different path");
        assert!(!head_matches(&at_tls, None, std::slice::from_ref(&clone)), "no origin, no match");
    }

    const STATES: [RowState; 7] = [
        RowState::InSync,
        RowState::Ahead,
        RowState::Behind,
        RowState::Diverged,
        RowState::Local,
        RowState::Remote,
        RowState::Untracked,
    ];

    /// Equal identity is the only proof; a difference says nothing about
    /// direction, because the two sides' sizes cover different files.
    #[test]
    fn a_shallow_compare_proves_only_equality() {
        assert_eq!(verdict_of("a", "a"), Shallow::Identical);
        assert_eq!(verdict_of("a", "b"), Shallow::Differs);
        let looked = Some(Compare { verdict: Shallow::Differs, local_size: 1, hub_size: 2, relation: None, lean: None });
        assert_eq!(compared_label(RowState::Untracked, looked), "Differs");
        assert_eq!(compared_label(RowState::Untracked, None), "Not compared");
        // Only an uncompared session is renamed by having been looked at.
        assert_eq!(compared_label(RowState::Behind, looked), "Needs pull");
        assert_ne!(compared_hint(RowState::Untracked, looked), RowState::Untracked.hint());
    }

    fn looked(relation: Option<Relation>, lean: Option<Lean>) -> Option<Compare> {
        Some(Compare { verdict: Shallow::Differs, local_size: 1, hub_size: 2, relation, lean })
    }

    /// A lean needs a time and a size that point the same way, and is a guess:
    /// it is never a state, and no state's action depends on it.
    #[test]
    fn a_lean_needs_time_and_size_to_agree() {
        let at = |s: &str| Some(s.parse::<Timestamp>().unwrap());
        let (old, new) = (at("2026-09-01T00:00:00Z"), at("2026-09-15T00:00:00Z"));
        // Local newer and larger, or the same size.
        assert_eq!(lean_of(new, old, 10, 5), Some(Lean::Local));
        assert_eq!(lean_of(new, old, 5, 5), Some(Lean::Local));
        // Newer but smaller: the size points against the time.
        assert_eq!(lean_of(new, old, 4, 5), None);
        // The hub's the newer and the larger.
        assert_eq!(lean_of(old, new, 5, 10), Some(Lean::Hub));
        assert_eq!(lean_of(old, new, 10, 5), None);
        // The same time, or a time missing on either side.
        assert_eq!(lean_of(old, old, 10, 5), None);
        assert_eq!(lean_of(None, old, 10, 5), None);
        assert_eq!(lean_of(new, None, 10, 5), None);
    }

    /// What the content proved names the row's state; what is only guessed
    /// leaves the state, its label and its action alone and changes the words.
    #[test]
    fn a_proof_sets_the_state_and_a_guess_only_the_hint() {
        use RowState::*;
        assert_eq!(RowState::of_relation(Relation::Same), InSync);
        assert_eq!(RowState::of_relation(Relation::LocalAhead), Ahead);
        assert_eq!(RowState::of_relation(Relation::HubAhead), Behind);
        assert_eq!(RowState::of_relation(Relation::Diverged), Diverged);

        // Proven: the row is Ahead, Behind or Diverged, labelled as those are,
        // and says it was read from the hub's copy.
        for (state, relation, said) in [
            (Ahead, Relation::LocalAhead, "goes on from the hub's"),
            (Behind, Relation::HubAhead, "goes on from this machine's"),
            (Diverged, Relation::Diverged, "Each side has something"),
        ] {
            let c = looked(Some(relation), None);
            assert_eq!(compared_label(state, c), state.label());
            assert!(compared_hint(state, c).contains(said), "{state:?}");
        }

        // Guessed: still "Differs", still a pull, and the hint says it is a guess.
        let local = looked(None, Some(Lean::Local));
        let hub = looked(None, Some(Lean::Hub));
        let unknown = looked(None, None);
        for c in [local, hub, unknown] {
            assert_eq!(compared_label(Untracked, c), "Differs");
        }
        assert_eq!(Untracked.action(), Some(SyncAction::Pull));
        assert!(compared_hint(Untracked, local).contains("This machine's copy looks newer"));
        assert!(compared_hint(Untracked, local).contains("only a guess"));
        assert!(compared_hint(Untracked, hub).contains("The hub's copy looks newer"));
        assert!(compared_hint(Untracked, unknown).contains("takes the content to tell"));
        // Nothing but a lean on an uncompared session is reworded.
        assert_eq!(compared_hint(InSync, local), InSync.hint());
        assert_eq!(compared_hint(Untracked, None), Untracked.hint());
    }

    #[test]
    fn a_comparison_serializes_what_it_proved_and_what_it_guessed() {
        let json = serde_json::to_value(looked(Some(Relation::LocalAhead), None).unwrap()).unwrap();
        assert_eq!(json["verdict"], "differs");
        assert_eq!(json["relation"], "local_ahead");
        assert!(json["lean"].is_null());
        let json = serde_json::to_value(looked(None, Some(Lean::Hub)).unwrap()).unwrap();
        assert!(json["relation"].is_null());
        assert_eq!(json["lean"], "hub");
    }

    /// A result stands for a caller that would not read the hub's copy
    /// either; one that would read it is not answered from a result that did
    /// not. A copy that was read, or never could be, is final.
    #[test]
    fn a_remembered_comparison_is_reused_only_when_asking_again_could_not_add_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "{}\n").unwrap();
        let session = bundle::session_at(AgentKind::ClaudeCode, "cache-test", crate::model::SessionLocation::JsonlFile { path });
        let mut hub = head(AgentKind::ClaudeCode, "cache-test", None);
        hub.rev = "rev-1".into();
        hub.total_size = 5 * 1024 * 1024;
        let (k, pair) = pair_key(&session, &hub);
        let put = |pair: &str, settled: bool| {
            compare_cache().lock().unwrap().insert(k.clone(), (pair.to_string(), looked(None, None).unwrap(), settled));
        };

        put(&pair, false);
        assert!(cached_compare(&session, &hub, RELATE_LISTING_MAX).is_some(), "over what a listing reads");
        assert!(cached_compare(&session, &hub, RELATE_COMPARE_MAX).is_none(), "but the explicit compare reads it");
        put(&pair, true);
        assert!(cached_compare(&session, &hub, RELATE_COMPARE_MAX).is_some(), "read once, final");
        // Either side moved on: a different pair, a different answer.
        put("some-other-pair", true);
        assert!(cached_compare(&session, &hub, RELATE_LISTING_MAX).is_none());
        compare_cache().lock().unwrap().remove(&k);
    }

    /// Every state says what to do about it, in words; two states never
    /// share a label, or a pill could not tell them apart.
    #[test]
    fn every_sync_state_has_a_distinct_label_and_a_next_step() {
        let mut labels: Vec<&str> = STATES.iter().map(|s| s.label()).collect();
        labels.sort();
        labels.dedup();
        assert_eq!(labels.len(), STATES.len());
        assert!(STATES.iter().all(|s| !s.hint().is_empty()));
        // Not on the hub, and changed since the last push: both want a push.
        assert_eq!(RowState::Local.action(), Some(SyncAction::Push));
        assert_eq!(RowState::Ahead.action(), Some(SyncAction::Push));
        assert_eq!(RowState::Behind.action(), Some(SyncAction::Pull));
        assert_eq!(RowState::Remote.action(), Some(SyncAction::Pull));
        assert_eq!(RowState::Diverged.action(), Some(SyncAction::Resolve));
        assert_eq!(RowState::InSync.action(), None);
        // Pulling an uncompared session is how it gets compared, but it is
        // not "needs pull": nothing is known to be newer on the hub.
        assert_eq!(RowState::Untracked.action(), Some(SyncAction::Pull));
        assert_eq!(RowState::Untracked.label(), "Not compared");
        assert_ne!(RowState::Untracked.label(), RowState::Behind.label());
    }

    fn row(state: RowState) -> Row {
        Row {
            project: String::new(),
            agent: AgentKind::ClaudeCode,
            id: "x".into(),
            short_id: "x".into(),
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
    }

    #[test]
    fn the_summary_counts_what_wants_attention() {
        let rows: Vec<Row> = [
            RowState::InSync,
            RowState::InSync,
            RowState::Ahead,
            RowState::Local,
            RowState::Behind,
            RowState::Remote,
            RowState::Remote,
            RowState::Diverged,
        ]
        .into_iter()
        .map(row)
        .collect();
        assert_eq!(
            summarize(&rows),
            Summary { synced: 2, to_push: 2, to_pull: 3, to_resolve: 1, new_on_hub: 2 }
        );
    }

    #[test]
    fn a_hub_address_reads_as_host_and_port() {
        assert_eq!(host_of("http://127.0.0.1:7450"), "127.0.0.1:7450");
        assert_eq!(host_of("https://hub.example.ts.net/"), "hub.example.ts.net");
        assert_eq!(host_of("hub.local"), "hub.local");
    }
}
