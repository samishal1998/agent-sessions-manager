//! `push`, `pull`, and the listing that puts both machines side by side.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Serialize;

use super::bundle::{self, Bundle, InstallOutcome, Installed, Source};
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

fn key(agent: AgentKind, id: &str) -> String {
    format!("{agent}:{id}")
}

fn scratch_dir(prefix: &str) -> Result<PathBuf, CoreError> {
    let dir = paths::tmp_dir()?.join(format!("{prefix}-{}", random_hex(8)?));
    std::fs::create_dir_all(&dir).map_err(|e| CoreError::io(&dir, e))?;
    Ok(dir)
}

/// Removes a scratch directory however the operation using it ends.
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A project's git origin, asked of git once per directory per run.
fn origin_of(root: &Path, origins: &mut HashMap<PathBuf, Option<String>>) -> Option<String> {
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

fn manifest_for(
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
/// head; the replaced one stays on the hub as a revision.
pub fn push(remote: &Remote, sessions: &[Session], force: bool) -> Result<BulkReport, CoreError> {
    let heads: HashMap<String, Head> = remote
        .heads()?
        .into_iter()
        .map(|h| (key(h.manifest.agent, &h.manifest.id), h))
        .collect();
    // The same file named twice is one session. The same id filed in two
    // places is two copies, and pushing both would swap the hub's head
    // between them on every run.
    let mut places: HashMap<String, Vec<&crate::model::SessionLocation>> = HashMap::new();
    for s in sessions {
        let here = places.entry(key(s.handle.agent, &s.handle.native_id)).or_default();
        if !here.contains(&&s.handle.location) {
            here.push(&s.handle.location);
        }
    }
    let mut state = SyncState::load(remote)?;
    let mut origins = HashMap::new();
    let mut report = BulkReport::default();
    let mut done = std::collections::HashSet::new();
    for session in sessions {
        let (agent, id) = (session.handle.agent, &session.handle.native_id);
        let k = key(agent, id);
        if !done.insert(k.clone()) {
            continue;
        }
        let copies = places[&k].len();
        let outcome = if copies > 1 {
            ItemOutcome::Failed {
                error: format!(
                    "session {id} exists in {copies} places here; resolve that first (asm \
                     doctor lists them)"
                ),
            }
        } else {
            push_one(remote, &mut state, &heads, &mut origins, session, force)
                .unwrap_or_else(|e| ItemOutcome::Failed { error: e.to_string() })
        };
        report.items.push(BulkItem {
            agent,
            native_id: id.clone(),
            label: format!("{agent} {}", session.short_id()),
            outcome,
        });
    }
    Ok(report)
}

fn in_sync() -> ItemOutcome {
    ItemOutcome::Ok { note: "in sync".into() }
}

fn push_one(
    remote: &Remote,
    state: &mut SyncState,
    heads: &HashMap<String, Head>,
    origins: &mut HashMap<PathBuf, Option<String>>,
    session: &Session,
    force: bool,
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
        && t.fingerprint == fingerprint
        && t.hub_rev == h.rev
    {
        return Ok(in_sync());
    }

    let mut bundle = bundle::collect(session)?;
    let canonical = bundle.canonical.clone();
    let record = |state: &mut SyncState, rev: &str, files: String| {
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

    let parent = match (&tracked, head) {
        (Some(t), Some(h)) if h.rev == t.hub_rev => {
            if canonical == t.canonical && files_now(&bundle) == t.files {
                record(state, &h.rev, t.files.clone())?;
                return Ok(in_sync());
            }
            Some(h.rev.clone())
        }
        (Some(t), Some(h)) => {
            if h.manifest.canonical == canonical {
                record(state, &h.rev, files_now(&bundle))?;
                return Ok(in_sync());
            }
            if !force && !ours(h) {
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
            }
            Some(h.rev.clone())
        }
        // Tracked here, gone from the hub (a new hub, or one reset): push
        // it as new.
        (Some(_), None) | (None, None) => None,
        (None, Some(h)) => {
            if h.manifest.canonical == canonical {
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
fn resolve_head<'a>(heads: &'a [Head], query: &str) -> Result<&'a Head, CoreError> {
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
    /// The machine that pushed this revision.
    pub from: Option<String>,
    #[serde(flatten)]
    pub installed: Installed,
}

/// Bring one session from the hub onto this machine.
pub fn pull(remote: &Remote, query: &str, project_dir: Option<&Path>) -> Result<Pulled, CoreError> {
    let heads = remote.heads()?;
    let head = resolve_head(&heads, query)?;
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
    let manifest = &history.manifest;

    let scratch = Scratch(scratch_dir("pull")?);
    let blob = |sha: &str| -> Result<PathBuf, CoreError> {
        let path = scratch.0.join(sha);
        if !path.is_file() {
            remote.get_blob(sha, &path)?;
        }
        Ok(path)
    };
    let installed = match agent {
        AgentKind::ClaudeCode => {
            let adapter = crate::adapter::claude::ClaudeAdapter::default_store()
                .ok_or_else(|| invalid("cannot locate the Claude Code store"))?;
            crate::adapter::claude::hub::install(&adapter, manifest, &blob, project_dir)?
        }
        _ => unreachable!("restorable() admitted {agent}"),
    };

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
                hub_rev: history.head.clone(),
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

#[derive(Debug, Serialize)]
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
    /// Whether `asm pull` can bring it here.
    pub restorable: bool,
}

/// Every session on this machine and on the hub, grouped by project. The
/// same session on both is one row, with how the two copies relate.
pub fn remote_list(remote: &Remote, local: &[Session]) -> Result<Vec<Row>, CoreError> {
    let heads = remote.heads()?;
    let state = SyncState::load(remote)?;
    let mut origins = HashMap::new();
    let by_key: HashMap<String, &Session> =
        local.iter().map(|s| (key(s.handle.agent, &s.handle.native_id), s)).collect();

    let mut rows = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for head in &heads {
        let m = &head.manifest;
        let k = key(m.agent, &m.id);
        seen.insert(k.clone());
        let row_state = match (by_key.get(&k), state.get(&k)) {
            (None, _) => RowState::Remote,
            (Some(_), None) => RowState::Untracked,
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
            restorable: bundle::restorable(m.agent),
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
            restorable: bundle::restorable(s.handle.agent),
        });
    }
    rows.sort_by(|a, b| a.project.cmp(&b.project).then(b.updated.cmp(&a.updated)));
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::manifest::Manifest;

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
}
