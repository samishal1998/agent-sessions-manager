//! High-level operations shared by every frontend (CLI, TUI, web).
//! Frontends never touch adapters or stores directly.

use std::path::{Path, PathBuf};

use crate::CoreError;
use crate::adapter::{
    Adapter, AgentRead, AgentWrite, ArchiveOutcome, DeleteReport, RelocateOutcome, SessionFilter,
};
use crate::model::{AgentKind, Project, ProjectWorktree, Session};

/// Every session, from every agent that can be read. An agent whose store
/// cannot be read is left out rather than failing the whole listing (one
/// broken or unsupported store must not hide the others); use
/// `list_sessions_partial` to hear which.
pub fn list_sessions(filter: &SessionFilter) -> Result<Vec<Session>, CoreError> {
    Ok(list_sessions_partial(filter).0)
}

/// `list_sessions`, plus one line per agent that could not be read.
pub fn list_sessions_partial(filter: &SessionFilter) -> (Vec<Session>, Vec<String>) {
    let mut sessions = Vec::new();
    let mut problems = Vec::new();
    for adapter in Adapter::available() {
        match adapter.sessions(filter) {
            Ok(found) => sessions.extend(found),
            Err(e) => problems.push(format!("{}: {e}", adapter.kind())),
        }
    }
    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated));
    (sessions, problems)
}

/// What a listing should say about the stores beyond their sessions: one
/// line each, for the frontend to show once (an OpenCode 2.x database that
/// still has sessions to migrate lists only the migrated ones).
pub fn store_notes() -> Vec<String> {
    Adapter::available()
        .into_iter()
        .filter_map(|a| match a {
            Adapter::OpenCode(opencode) => opencode.store_note(),
            _ => None,
        })
        .collect()
}

/// Sessions as each agent's store gives them up, for a frontend that shows
/// them arriving. Returns what could not be read rather than failing: one
/// broken store should not leave the list empty.
/// `emit` returns false to stop: the walk ends there, and so does the
/// reading behind it.
pub fn stream_sessions(
    filter: &SessionFilter,
    mut emit: impl FnMut(AgentKind, Vec<Session>) -> bool,
) -> Vec<String> {
    let mut problems = Vec::new();
    for adapter in Adapter::available() {
        let kind = adapter.kind();
        let mut listening = true;
        let mut batch = |mut sessions: Vec<Session>| {
            if !sessions.is_empty() {
                sessions.sort_by_key(|s| std::cmp::Reverse(s.updated));
                listening = emit(kind, sessions);
            }
            listening
        };
        if let Err(e) = adapter.sessions_streamed(filter, &mut batch) {
            problems.push(format!("{kind}: {e}"));
        }
        if !listening {
            break;
        }
    }
    problems
}

/// Where a command was run from, as `--here` and `--all-worktrees` mean it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Here {
    /// The checkout containing the directory (a linked worktree is its own
    /// checkout), or the directory itself outside git.
    pub worktree: PathBuf,
    /// Every checkout of the same repository, `worktree` among them.
    pub worktrees: Vec<PathBuf>,
}

impl Here {
    /// The roots to narrow to: this checkout, or every one of the repository.
    pub fn roots(&self, all_worktrees: bool) -> Vec<PathBuf> {
        if all_worktrees { self.worktrees.clone() } else { vec![self.worktree.clone()] }
    }

    /// The roots to leave out: with one worktree, the repository's other
    /// worktrees nested inside it, since a session belongs to the deepest
    /// checkout containing it. Every worktree leaves out none.
    pub fn without(&self, all_worktrees: bool) -> Vec<PathBuf> {
        if all_worktrees {
            return Vec::new();
        }
        self.worktrees.iter().filter(|w| self.nests(w)).cloned().collect()
    }

    /// Whether a session's project root is in these roots, as `roots` and
    /// `without` together mean it, without building either.
    pub fn contains(&self, root: &Path, all_worktrees: bool) -> bool {
        if all_worktrees {
            return crate::adapter::under_any(root, &self.worktrees);
        }
        crate::adapter::under_any(root, std::slice::from_ref(&self.worktree))
            && !self.worktrees.iter().any(|w| self.nests(w) && root.starts_with(w))
    }

    fn nests(&self, other: &Path) -> bool {
        other != self.worktree && other.starts_with(&self.worktree)
    }
}

/// Resolve `cwd` to its checkout and the repository's other checkouts.
///
/// Paths are canonical: agents record the physical working directory
/// (`getcwd`), so a symlinked cwd, or macOS's `/var` for `/private/var`,
/// has to be resolved before it can equal a recorded project root. Session
/// roots are compared as recorded, which avoids a syscall per session.
pub fn here(cwd: &Path) -> Here {
    let canon = |p: PathBuf| p.canonicalize().unwrap_or(p);
    let cwd = canon(cwd.to_path_buf());
    let Some(worktree) = crate::git::toplevel(&cwd).map(canon) else {
        return Here { worktree: cwd.clone(), worktrees: vec![cwd] };
    };
    let mut worktrees: Vec<PathBuf> = crate::git::repo_of(&worktree)
        .and_then(|repo| crate::git::worktrees(&repo.main_worktree).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|w| canon(w.path))
        .collect();
    if !worktrees.contains(&worktree) {
        worktrees.insert(0, worktree.clone());
    }
    Here { worktree, worktrees }
}

pub fn list_projects() -> Result<Vec<Project>, CoreError> {
    let sessions = list_sessions(&SessionFilter::default())?;
    Ok(group_projects(&sessions, crate::git::repo_of, |root| {
        crate::git::worktrees(root).unwrap_or_default()
    }))
}

/// Group sessions into projects by the repository that contains them.
///
/// Split out with its resolvers injected so the grouping rules can be
/// tested without real repositories on disk. `repo_of` identifies the
/// repository containing a directory; `worktrees_of` lists a repository's
/// checkouts given its main worktree.
pub fn group_projects(
    sessions: &[Session],
    repo_of: impl Fn(&Path) -> Option<crate::git::Repo>,
    worktrees_of: impl Fn(&Path) -> Vec<crate::git::Worktree>,
) -> Vec<Project> {
    use std::collections::HashMap;

    // Resolving a repository shells out to git, so do it once per distinct
    // directory rather than once per session.
    let mut repo_cache: HashMap<PathBuf, Option<crate::git::Repo>> = HashMap::new();
    let mut groups: HashMap<PathBuf, Vec<&Session>> = HashMap::new();
    let mut repos: HashMap<PathBuf, Option<crate::git::Repo>> = HashMap::new();

    for session in sessions {
        // A project counts the sessions an agent's own picker lists. Children
        // are in the listing a frontend groups, and counting them here
        // would make every project's number depend on which listing it got.
        if session.parent.is_some() {
            continue;
        }
        // A session whose agent never recorded a working directory is in no
        // project. Antigravity is the case in point: nothing in its store
        // ties most conversations to a directory. Grouping them by the
        // empty path would invent a project with a blank name and file
        // every such session under it.
        if session.project_root.as_os_str().is_empty() {
            continue;
        }
        let dir = session.project_root.clone();
        let repo = repo_cache.entry(dir.clone()).or_insert_with(|| repo_of(&dir)).clone();
        // Identity: the repository's common dir, or the bare directory.
        let key = repo.as_ref().map_or(dir.clone(), |r| r.common_dir.clone());
        groups.entry(key.clone()).or_default().push(session);
        repos.entry(key).or_insert(repo);
    }

    let mut projects: Vec<Project> = groups
        .into_iter()
        .map(|(key, group)| {
            let repo = repos.get(&key).cloned().flatten();
            let root = repo
                .as_ref()
                .map_or_else(|| key.clone(), |r| r.main_worktree.clone());

            let mut agents: Vec<AgentKind> = group.iter().map(|s| s.handle.agent).collect();
            agents.sort_by_key(|a| a.as_str());
            agents.dedup();

            // Every checkout git knows about, so an empty worktree is still
            // visible, plus any session directory git did not list.
            let mut worktrees: Vec<ProjectWorktree> = if repo.is_some() {
                worktrees_of(&root)
                    .into_iter()
                    .map(|w| ProjectWorktree {
                        path: w.path,
                        branch: w.branch,
                        is_main: w.is_main,
                        session_count: 0,
                    })
                    .collect()
            } else {
                Vec::new()
            };
            if worktrees.is_empty() {
                worktrees.push(ProjectWorktree {
                    path: root.clone(),
                    branch: None,
                    is_main: true,
                    session_count: 0,
                });
            }

            for session in &group {
                // A session in a subdirectory belongs to the checkout that
                // contains it — the longest matching path, so nested
                // worktrees attribute to the innermost one.
                let best = worktrees
                    .iter_mut()
                    .filter(|w| session.project_root.starts_with(&w.path))
                    .max_by_key(|w| w.path.as_os_str().len());
                match best {
                    Some(worktree) => worktree.session_count += 1,
                    None => worktrees.push(ProjectWorktree {
                        path: session.project_root.clone(),
                        branch: session.git_branch.clone(),
                        is_main: false,
                        session_count: 1,
                    }),
                }
            }

            Project {
                root,
                repo: repo.map(|r| r.common_dir),
                agents,
                session_count: group.len(),
                size_bytes: group.iter().filter_map(|s| s.size_bytes).sum(),
                last_updated: group.iter().filter_map(|s| s.updated).max(),
                worktrees,
            }
        })
        .collect();

    projects.sort_by_key(|p| std::cmp::Reverse(p.last_updated));
    projects
}

/// A session in the tree [`group_children`] builds, and what hangs under it.
/// `index` is a position in the slice that was grouped, so a caller that
/// already holds its sessions gets them back without copying one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub index: usize,
    pub children: Vec<Node>,
}

impl Node {
    /// Sessions below this one, at any depth.
    pub fn descendants(&self) -> usize {
        self.children.iter().map(|c| 1 + c.descendants()).sum()
    }
}

/// Arrange sessions as the tree their `parent` links make: the roots in the
/// order given, each with its children newest first (stable, so equal times
/// keep the listing's order).
///
/// - A parent is looked up by (agent, native id), never by id alone: two
///   agents may use the same id.
/// - A session whose parent is not in `sessions` is a root. That is every
///   orphan — a Codex thread whose parent rollout is gone, a child archived
///   without its parent — and it is also what makes a filtered list work: a
///   match whose parent did not match stands on its own.
/// - Depth is whatever the data has (OpenCode nests subagents in subagents).
/// - A cycle in the data does not hang this or lose a session. It is broken
///   at its member that comes first in `sessions`, which becomes a root, so
///   every session appears exactly once.
// ponytail: recursion is as deep as the tree, a handful of levels in any
// store seen; an iterative walk if a store ever nests thousands.
pub fn group_children<S: std::borrow::Borrow<Session>>(sessions: &[S]) -> Vec<Node> {
    use std::collections::HashMap;

    let n = sessions.len();
    let at = |i: usize| sessions[i].borrow();
    let mut position: HashMap<(AgentKind, &str), usize> = HashMap::new();
    for i in 0..n {
        position.entry((at(i).handle.agent, at(i).handle.native_id.as_str())).or_insert(i);
    }
    let parent_of: Vec<Option<usize>> = (0..n)
        .map(|i| {
            let parent = at(i).parent.as_deref()?;
            position.get(&(at(i).handle.agent, parent)).copied().filter(|&p| p != i)
        })
        .collect();
    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (child, parent) in parent_of.iter().enumerate() {
        if let Some(parent) = parent {
            kids[*parent].push(child);
        }
    }
    for k in &mut kids {
        k.sort_by_key(|&i| std::cmp::Reverse(at(i).updated));
    }

    fn grow(i: usize, kids: &[Vec<usize>], seen: &mut [bool]) -> Node {
        seen[i] = true;
        let mut children = Vec::new();
        for &child in &kids[i] {
            if !seen[child] {
                children.push(grow(child, kids, seen));
            }
        }
        Node { index: i, children }
    }

    let mut seen = vec![false; n];
    let mut roots: Vec<Node> =
        (0..n).filter(|&i| parent_of[i].is_none()).map(|i| grow(i, &kids, &mut seen)).collect();
    // What is left hangs off a cycle: no root reaches it.
    for start in 0..n {
        if seen[start] {
            continue;
        }
        let mut path = vec![start];
        let mut cursor = start;
        let cycle = loop {
            let Some(next) = parent_of[cursor] else { break &path[..] };
            if let Some(at) = path.iter().position(|&p| p == next) {
                break &path[at..];
            }
            path.push(next);
            cursor = next;
        };
        let cut = cycle.iter().copied().min().unwrap_or(start);
        roots.push(grow(cut, &kids, &mut seen));
    }
    roots.sort_by_key(|node| node.index);
    roots
}

/// The tree in display order, each session with its depth (a root is 0).
/// `open` says whether the children of a session are shown.
pub fn flatten(nodes: &[Node], open: &dyn Fn(usize) -> bool) -> Vec<(usize, usize)> {
    fn walk(nodes: &[Node], depth: usize, open: &dyn Fn(usize) -> bool, out: &mut Vec<(usize, usize)>) {
        for node in nodes {
            out.push((node.index, depth));
            if open(node.index) {
                walk(&node.children, depth + 1, open, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(nodes, 0, open, &mut out);
    out
}

/// Everything under `session` in `all`, in tree order, each with how far
/// below `session` it sits (a child is 1).
pub fn descendants_in<'a>(all: &'a [Session], session: &Session) -> Vec<(usize, &'a Session)> {
    let flat = flatten(&group_children(all), &|_| true);
    let is_it = |i: usize| {
        all[i].handle.agent == session.handle.agent && all[i].handle.native_id == session.handle.native_id
    };
    let Some(at) = flat.iter().position(|&(i, _)| is_it(i)) else { return Vec::new() };
    let depth = flat[at].1;
    flat[at + 1..].iter().take_while(|&&(_, d)| d > depth).map(|&(i, d)| (d - depth, &all[i])).collect()
}

/// The sessions under `session` in its agent's store, as [`descendants_in`].
pub fn descendants(session: &Session) -> Result<Vec<(usize, Session)>, CoreError> {
    let filter = SessionFilter {
        agent: Some(session.handle.agent),
        include_children: true,
        ..SessionFilter::default()
    };
    let all = list_sessions(&filter)?;
    Ok(descendants_in(&all, session).into_iter().map(|(depth, s)| (depth, s.clone())).collect())
}

/// Whether deleting a session of `agent` deletes the sessions under it too.
/// OpenCode's own removal is recursive, and asm follows it rather than
/// strand a subagent behind a parent that is gone.
pub fn deletes_descendants(agent: AgentKind) -> bool {
    agent == AgentKind::OpenCode
}

/// Health report for `asm doctor`.
#[derive(Debug, serde::Serialize)]
pub struct DoctorReport {
    pub agents: Vec<AgentHealth>,
    /// Stores that could not be read at all, one line each (the lines a
    /// listing prints to stderr). `agents` still has them, with no sessions.
    pub warnings: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct AgentHealth {
    pub agent: crate::model::AgentKind,
    pub store_root: std::path::PathBuf,
    pub store_found: bool,
    pub session_count: usize,
    /// Claude only: ids whose transcript exists in >1 project dir, which
    /// poisons cross-project `--resume`.
    pub duplicate_session_ids: Vec<String>,
    pub warnings: Vec<String>,
    /// What this adapter can actually do, so a UI can offer only those
    /// verbs rather than letting the user discover the answer by error.
    pub capabilities: crate::adapter::Capabilities,
    /// OpenCode only: `v1`, `v2`, `absent`, `unsupported` or `unknown`. Null
    /// for every other agent.
    pub schema: Option<&'static str>,
    /// OpenCode 2.x only: whether sessions are still waiting to be migrated
    /// from the 1.x tables.
    pub unmigrated: Option<bool>,
}

pub fn doctor() -> Result<DoctorReport, CoreError> {
    // One unreadable store is a finding, not a reason to report on none.
    let problems = std::cell::RefCell::new(Vec::new());
    let mut report = doctor_with(|adapter| {
        Ok(adapter.sessions(&SessionFilter::default()).map(|s| s.len()).unwrap_or_else(|e| {
            problems.borrow_mut().push(format!("{}: could not read sessions: {e}", adapter.kind()));
            0
        }))
    })?;
    report.warnings = problems.into_inner();
    Ok(report)
}

/// An OpenCode store's schema generation and whether it is still migrating;
/// nothing for any other agent.
pub fn store_schema(adapter: &Adapter) -> (Option<&'static str>, Option<bool>) {
    use crate::adapter::opencode::{Schema, unmigrated};
    let Adapter::OpenCode(opencode) = adapter else { return (None, None) };
    let schema = opencode.schema();
    let label = match schema {
        Schema::V1 => "v1",
        Schema::V2 => "v2",
        Schema::Absent => "absent",
        Schema::Unsupported => "unsupported",
        Schema::Unknown => "unknown",
    };
    let migrating = (schema == Schema::V2).then(|| unmigrated(opencode.db()).is_ok_and(|n| n > 0));
    (Some(label), migrating)
}

/// `doctor`, with the session counts supplied. Counting is a full listing
/// of the store, and a frontend that has just listed it already has the
/// number.
pub fn doctor_with(
    session_count: impl Fn(&Adapter) -> Result<usize, CoreError>,
) -> Result<DoctorReport, CoreError> {
    let mut agents = Vec::new();
    for adapter in Adapter::available() {
        let detect = adapter.detect();
        let session_count = session_count(&adapter)?;
        let mut duplicate_session_ids = Vec::new();
        let mut warnings = Vec::new();
        match &adapter {
            Adapter::ClaudeCode(claude) => {
                for (id, paths) in claude.duplicate_session_ids()? {
                    warnings.push(format!(
                        "session {id} exists in {} project dirs — cross-project --resume \
                         will fail for it",
                        paths.len()
                    ));
                    duplicate_session_ids.push(id);
                }
            }
            // jcode keeps one snapshot per session id in one directory, so
            // it has neither Claude's duplicate-id hazard nor OpenCode's
            // single-writer lock.
            Adapter::JCode(_) => {}
            // Codex keeps one rollout per thread id and asm never writes to
            // it, so there is no divergence for doctor to find.
            Adapter::Codex(_) => {}
            // One database per conversation, never written by asm, so no
            // divergence and no shared lock to contend for.
            Adapter::Antigravity(_) => {}
            // 2.x does not use the 1.x lock directory: what is left in it is
            // not about this store, and busy sessions are found per session.
            Adapter::OpenCode(opencode) if opencode.is_v2() => {
                warnings.extend(opencode.store_note());
            }
            Adapter::OpenCode(opencode) => {
                for lock in opencode.locks() {
                    if lock.held {
                        warnings.push(format!(
                            "store is in use ({}); mutations will be refused until it exits",
                            lock.reason
                        ));
                    } else {
                        warnings.push(format!(
                            "stale lock ({}) — it does not block asm, and you can remove it: {}",
                            lock.reason,
                            lock.path.display()
                        ));
                    }
                }
            }
        }
        let (schema, unmigrated) = store_schema(&adapter);
        agents.push(AgentHealth {
            agent: detect.agent,
            store_root: detect.store_root,
            store_found: detect.store_found,
            session_count,
            duplicate_session_ids,
            warnings,
            capabilities: adapter.capabilities(),
            schema,
            unmigrated,
        });
    }
    Ok(DoctorReport { agents, warnings: Vec::new() })
}

/// The command that resumes `session` in its native agent.
pub fn resume_command(session: &Session) -> Result<std::process::Command, ResolveError> {
    for adapter in Adapter::available() {
        if adapter.kind() == session.handle.agent
            && let Some(cmd) = adapter.resume_command(session)
        {
            return Ok(cmd);
        }
    }
    Err(ResolveError::NotFound { query: session.handle.native_id.clone() })
}

/// The adapter that owns a session's store, for callers outside `ops`.
pub fn adapter_for(agent: AgentKind) -> Option<Adapter> {
    Adapter::available().into_iter().find(|a| a.kind() == agent)
}

fn owning_adapter(session: &Session) -> Result<Adapter, CoreError> {
    Adapter::available()
        .into_iter()
        .find(|a| a.kind() == session.handle.agent)
        .ok_or(CoreError::StoreNotFound { path: PathBuf::new() })
}

pub fn rename(session: &Session, title: &str) -> Result<(), CoreError> {
    owning_adapter(session)?.rename(session, title)
}

pub fn archive(session: &Session) -> Result<ArchiveOutcome, CoreError> {
    owning_adapter(session)?.archive(session)
}

/// Unarchive by user-supplied reference. OpenCode sessions unarchive in
/// place (clearing `time_archived`); Claude sessions are restored from our
/// archive store by id. An ambiguous reference stays an `Ambiguous` error,
/// so a caller can list the candidates.
pub fn unarchive(query: &str, filter: &SessionFilter) -> Result<Option<PathBuf>, ResolveError> {
    match resolve_ref(query, filter) {
        Ok(session) => {
            owning_adapter(&session)?.unarchive(&session)?;
            Ok(None)
        }
        // Not resolvable as a live session — try the Claude archive store.
        Err(ResolveError::NotFound { .. }) => {
            let id_query = query.split_once(':').map(|(_, rest)| rest).unwrap_or(query);
            Ok(Some(crate::archive::unarchive_by_id(id_query)?))
        }
        Err(e) => Err(e),
    }
}

pub fn relocate(session: &Session, new_dir: &Path) -> Result<RelocateOutcome, CoreError> {
    owning_adapter(session)?.relocate(session, new_dir)
}

pub fn delete(session: &Session) -> Result<DeleteReport, CoreError> {
    owning_adapter(session)?.delete(session)
}

pub fn export_ir(session: &Session) -> Result<crate::ir::IrSession, CoreError> {
    owning_adapter(session)?.export_ir(session)
}

pub fn import(
    session: &Session,
    target: AgentKind,
    opts: &crate::import::ImportOpts,
) -> Result<crate::import::ImportOutcome, CoreError> {
    crate::import::import(session, target, opts)
}

/// What `asm capabilities` and `/api/meta` say about this build and the
/// agents on this machine.
#[derive(Debug, serde::Serialize)]
pub struct CapabilitiesReport {
    pub asm_version: &'static str,
    pub json_contract_version: u32,
    pub ir_version: u32,
    pub index_schema_version: u32,
    pub agents: Vec<AgentCapabilities>,
}

#[derive(Debug, serde::Serialize)]
pub struct AgentCapabilities {
    pub agent: AgentKind,
    /// Whether its store is on this machine. An agent that is not has the
    /// capabilities asm would have for it, and no `store_root`.
    pub installed: bool,
    pub store_root: Option<PathBuf>,
    pub capabilities: crate::adapter::Capabilities,
    /// Whether a session of this agent can be pushed to a hub and restored
    /// on another machine.
    pub hub_sync: bool,
    /// OpenCode only: `v1`, `v2`, `absent`, `unsupported` or `unknown`.
    pub schema: Option<&'static str>,
    /// OpenCode 2.x only: sessions still waiting to be migrated.
    pub unmigrated: Option<bool>,
}

/// Every agent asm knows, installed or not. Counts sessions never: that is a
/// full listing, and this is for a caller deciding what to ask next.
pub fn capabilities_report() -> CapabilitiesReport {
    let present = Adapter::available();
    let agents = AgentKind::ALL
        .iter()
        .map(|&agent| {
            let adapter = present.iter().find(|a| a.kind() == agent);
            let (schema, unmigrated) = adapter.map_or((None, None), store_schema);
            AgentCapabilities {
                agent,
                installed: adapter.is_some(),
                store_root: adapter.map(|a| a.detect().store_root),
                capabilities: adapter.map_or_else(|| Adapter::capabilities_for(agent), |a| a.capabilities()),
                hub_sync: crate::hub::bundle::restorable(agent),
                schema,
                unmigrated,
            }
        })
        .collect();
    CapabilitiesReport {
        asm_version: env!("CARGO_PKG_VERSION"),
        json_contract_version: crate::contract::JSON_CONTRACT_VERSION,
        ir_version: crate::ir::IR_VERSION,
        index_schema_version: crate::index::SCHEMA_VERSION,
        agents,
    }
}

/// What a listing keeps beyond `SessionFilter`: a state and free text. Kept
/// apart from the filter because the filter is built literally in many
/// places and narrows what adapters read, where this narrows what was read.
#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    /// `live`, `idle` or `archived`.
    pub state: Option<String>,
    /// Case-insensitive substring of the title, slug, id or project path.
    pub text: Option<String>,
}

impl ListQuery {
    pub fn keep(&self, session: &Session) -> bool {
        if let Some(state) = &self.state
            && session.status.state() != state
        {
            return false;
        }
        if let Some(text) = self.text.as_deref().map(str::to_lowercase).filter(|t| !t.is_empty()) {
            let hay = [
                session.title.as_deref().unwrap_or(""),
                session.slug.as_deref().unwrap_or(""),
                &session.handle.native_id,
                &session.project_root.to_string_lossy(),
            ];
            return hay.iter().any(|h| h.to_lowercase().contains(&text));
        }
        true
    }
}

/// One message of a session as `asm messages` prints it: its place in the
/// transcript and what the IR holds of it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MessageRow {
    /// Position in the whole transcript, counted before any filtering.
    pub index: usize,
    pub role: crate::ir::IrRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<jiff::Timestamp>,
    pub parts: Vec<crate::ir::IrPart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
}

/// Which messages. Applied in this order: `range`, then `role` and
/// `text_only`, then `tail`.
#[derive(Debug, Clone, Default)]
pub struct MessageQuery {
    /// `user`, `assistant`, `system`, or `tool`: a message holding a tool
    /// call or a tool result (the IR gives those to the assistant and the
    /// user).
    pub role: Option<String>,
    /// Indexes `start..end`, end exclusive and open when `None`.
    pub range: Option<(usize, Option<usize>)>,
    /// Keep only `text` parts, and only messages that have one.
    pub text_only: bool,
    /// The last N of what is left.
    pub tail: Option<usize>,
}

impl MessageQuery {
    pub const ROLES: [&'static str; 4] = ["user", "assistant", "system", "tool"];

    /// `A..B`, `A..` or `..B`.
    pub fn parse_range(text: &str) -> Result<(usize, Option<usize>), String> {
        let bad = || format!("'{text}' is not a range: write A..B, A.. or ..B");
        let (a, b) = text.split_once("..").ok_or_else(bad)?;
        let a = if a.is_empty() { 0 } else { a.parse().map_err(|_| bad())? };
        let b = if b.is_empty() { None } else { Some(b.parse().map_err(|_| bad())?) };
        Ok((a, b))
    }
}

/// [`messages`] over an IR already in hand.
pub fn messages_from_ir(ir: &crate::ir::IrSession, query: &MessageQuery) -> Vec<MessageRow> {
    use crate::ir::{IrPart, IrRole};
    let (start, end) = query.range.unwrap_or((0, None));
    let mut rows: Vec<MessageRow> = ir
        .messages
        .iter()
        .enumerate()
        .skip(start)
        .take(end.map_or(usize::MAX, |e| e.saturating_sub(start)))
        .filter(|(_, m)| match query.role.as_deref() {
            None => true,
            Some("user") => m.role == IrRole::User,
            Some("assistant") => m.role == IrRole::Assistant,
            Some("system") => m.role == IrRole::System,
            Some(_) => m.parts.iter().any(|p| matches!(p, IrPart::ToolCall { .. } | IrPart::ToolResult { .. })),
        })
        .map(|(index, m)| MessageRow {
            index,
            role: m.role,
            ts: m.timestamp,
            parts: if query.text_only {
                m.parts.iter().filter(|p| matches!(p, IrPart::Text { .. })).cloned().collect()
            } else {
                m.parts.clone()
            },
            source_id: m.source_id.clone(),
        })
        .filter(|row| !query.text_only || !row.parts.is_empty())
        .collect();
    if let Some(n) = query.tail {
        rows.drain(..rows.len().saturating_sub(n));
    }
    rows
}

/// A session's messages, read through the IR every agent exports to.
pub fn messages(session: &Session, query: &MessageQuery) -> Result<Vec<MessageRow>, CoreError> {
    Ok(messages_from_ir(&export_ir(session)?, query))
}

/// How a session changed between two looks at the stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Changed,
    Removed,
}

#[derive(Debug, Clone)]
pub struct Change {
    pub kind: ChangeKind,
    /// As it is now; for `Removed`, as it was last seen.
    pub session: Session,
}

/// Remembers the listing and says what differs from it the next time. The
/// first `poll` only learns the baseline.
// ponytail: a change is "the session's JSON differs", found by listing every
// store each poll; fine at a poll every few seconds, and a per-store
// fingerprint walk is the upgrade if listings ever get slow enough to hurt.
#[derive(Default)]
pub struct SessionWatch {
    seen: Option<std::collections::HashMap<(AgentKind, String), (serde_json::Value, Session)>>,
}

impl SessionWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Changes since the last poll (none, the first time), and what could
    /// not be read. A poll that could not read a store reports no removals:
    /// its sessions are unseen, not gone.
    pub fn poll(&mut self, filter: &SessionFilter, query: &ListQuery) -> (Vec<Change>, Vec<String>) {
        let (listed, problems) = list_sessions_partial(filter);
        let mut now = std::collections::HashMap::new();
        for session in listed.into_iter().filter(|s| query.keep(s)) {
            let json = serde_json::to_value(&session).unwrap_or_default();
            now.insert((session.handle.agent, session.handle.native_id.clone()), (json, session));
        }
        let Some(before) = self.seen.as_mut() else {
            self.seen = Some(now);
            return (Vec::new(), problems);
        };
        let mut changes = Vec::new();
        if problems.is_empty() {
            for (key, (_, session)) in before.iter() {
                if !now.contains_key(key) {
                    changes.push(Change { kind: ChangeKind::Removed, session: session.clone() });
                }
            }
        }
        for (key, (json, session)) in &now {
            match before.get(key) {
                None => changes.push(Change { kind: ChangeKind::Added, session: session.clone() }),
                Some((old, _)) if old != json => {
                    changes.push(Change { kind: ChangeKind::Changed, session: session.clone() })
                }
                Some(_) => {}
            }
        }
        // A stable order, whatever the map's.
        changes.sort_by(|a, b| {
            (a.session.handle.agent.as_str(), &a.session.handle.native_id)
                .cmp(&(b.session.handle.agent.as_str(), &b.session.handle.native_id))
        });
        if problems.is_empty() {
            *before = now;
        } else {
            before.extend(now);
        }
        (changes, problems)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("no session matches '{query}'")]
    NotFound { query: String },
    #[error("'{query}' is ambiguous; matches: {}", candidates.join(", "))]
    Ambiguous { query: String, candidates: Vec<String> },
    #[error(transparent)]
    Core(#[from] CoreError),
}

/// Resolve a user-supplied session reference: a native id, a unique id
/// prefix, or `agent:id-prefix`.
///
/// A child session (a subagent, a spawned thread) is found by its id like
/// any other: `include_children` and `include_debug` are questions about what
/// a listing shows, not about what can be addressed, so both are always on
/// here (a jcode debug session can still be shown or deleted by its id).
pub fn resolve_ref(query: &str, filter: &SessionFilter) -> Result<Session, ResolveError> {
    let mut filter = filter.clone();
    filter.include_children = true;
    filter.include_debug = true;
    let id_query = match query.split_once(':') {
        Some((agent, rest)) if AgentKind::parse(agent).is_some() => {
            filter.agent = AgentKind::parse(agent);
            rest
        }
        _ => query,
    };

    resolve_among(list_sessions(&filter)?, query, id_query)
}

/// `resolve_ref` over a listing already in hand — another machine's, say.
/// The query is matched whole against ids and names; an `agent:` prefix
/// only narrows the listing, so it is stripped here too.
pub fn resolve_in(sessions: Vec<Session>, query: &str) -> Result<Session, ResolveError> {
    let agent = query.split_once(':').and_then(|(agent, rest)| Some((AgentKind::parse(agent)?, rest)));
    match agent {
        Some((agent, rest)) => {
            resolve_among(sessions.into_iter().filter(|s| s.handle.agent == agent).collect(), query, rest)
        }
        None => resolve_among(sessions, query, query),
    }
}

fn resolve_among(sessions: Vec<Session>, query: &str, id_query: &str) -> Result<Session, ResolveError> {
    // An id prefix, or the memorable name the agent itself uses — jcode
    // calls a session "hog", never `session_hog_1787…`, and its own
    // `--resume` takes that name.
    let mut matches: Vec<Session> = sessions
        .into_iter()
        .filter(|s| {
            s.handle.native_id.starts_with(id_query)
                || s.slug.as_deref().is_some_and(|slug| slug == id_query)
        })
        .collect();

    // Several matches are narrowed by how much the user said: the whole id,
    // then the sessions an agent's own picker lists. The second keeps a
    // prefix that named one session before children could be addressed
    // naming it still, and the first is what makes an exact id always
    // resolve.
    if matches.len() > 1 {
        let narrowed = [
            matches.iter().filter(|s| s.handle.native_id == id_query).count(),
            matches.iter().filter(|s| s.parent.is_none()).count(),
        ];
        if narrowed[0] == 1 {
            matches.retain(|s| s.handle.native_id == id_query);
        } else if narrowed[1] == 1 {
            matches.retain(|s| s.parent.is_none());
        }
    }

    match matches.len() {
        0 => Err(ResolveError::NotFound { query: query.to_string() }),
        1 => Ok(matches.remove(0)),
        _ => Err(ResolveError::Ambiguous {
            query: query.to_string(),
            candidates: matches
                .iter()
                .map(|s| format!("{}:{}", s.handle.agent, s.handle.native_id))
                .collect(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SessionLocation, SessionRef, SessionStatus};

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// A linked worktree is its own checkout, a subdirectory resolves to
    /// the checkout above it, and a symlinked path resolves to the real one.
    #[test]
    fn here_is_the_checkout_and_its_sibling_worktrees() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let main = base.join("proj");
        std::fs::create_dir_all(main.join("src")).unwrap();
        git(&main, &["init", "-q"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "x"]);
        git(&main, &["worktree", "add", "-q", "../proj-wt"]);
        let wt = base.join("proj-wt");

        let at_main = here(&main.join("src"));
        assert_eq!(at_main.worktree, main);
        assert_eq!(at_main.worktrees, vec![main.clone(), wt.clone()]);
        let at_wt = here(&wt);
        assert_eq!(at_wt.worktree, wt, "the linked worktree, not the main one");
        assert_eq!(at_wt.roots(false), vec![wt.clone()]);
        assert_eq!(at_wt.roots(true), vec![main.clone(), wt.clone()]);

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&wt, base.join("link")).unwrap();
            assert_eq!(here(&base.join("link")).worktree, wt, "a symlinked cwd is resolved");
        }

        // Outside git: the directory alone, whether or not -W was asked.
        let plain = base.join("plain");
        std::fs::create_dir(&plain).unwrap();
        let outside = here(&plain);
        assert_eq!((outside.worktree.clone(), outside.roots(true)), (plain.clone(), vec![plain]));
    }

    #[test]
    fn within_is_a_root_or_under_it_by_component() {
        let filter = SessionFilter { within: vec!["/a/proj".into(), "/a/proj-wt".into()], ..Default::default() };
        let at = |root: &str| {
            let mut s = session(AgentKind::ClaudeCode, "x", None, 1);
            s.project_root = root.into();
            filter.matches(&s)
        };
        assert!(at("/a/proj"));
        assert!(at("/a/proj/sub/dir"), "a subdirectory");
        assert!(at("/a/proj-wt"), "a sibling worktree that is listed");
        assert!(!at("/a/proj2"), "a prefix is not a parent");
        assert!(!at("/a"));
        assert!(!at(""), "no recorded root is in no project");
        let mut rootless = session(AgentKind::ClaudeCode, "x", None, 1);
        rootless.project_root = PathBuf::new();
        assert!(SessionFilter::default().matches(&rootless), "an empty within lets everything through");
    }

    /// Claude Code puts its worktrees inside the repository, at
    /// `.claude/worktrees/<name>`: "this worktree" in the main checkout is
    /// not also that one, as a session belongs to the deepest checkout.
    #[test]
    fn here_leaves_out_worktrees_nested_in_this_one() {
        let nested = PathBuf::from("/a/proj/.claude/worktrees/x");
        let at_main = Here { worktree: "/a/proj".into(), worktrees: vec!["/a/proj".into(), nested.clone()] };
        assert_eq!(at_main.without(false), vec![nested.clone()]);
        assert!(at_main.without(true).is_empty(), "-W keeps every worktree");
        let at_nested = Here { worktree: nested.clone(), ..at_main.clone() };
        assert!(at_nested.without(false).is_empty(), "nothing is nested in the inner one");

        for (here, all, root, expected) in [
            (&at_main, false, "/a/proj/src", true),
            (&at_main, false, "/a/proj/.claude/worktrees/x/src", false),
            (&at_main, true, "/a/proj/.claude/worktrees/x/src", true),
            (&at_nested, false, "/a/proj/.claude/worktrees/x/src", true),
            (&at_nested, false, "/a/proj/src", false),
        ] {
            let filter =
                SessionFilter { within: here.roots(all), without: here.without(all), ..Default::default() };
            let mut s = session(AgentKind::ClaudeCode, "x", None, 1);
            s.project_root = root.into();
            assert_eq!(filter.matches(&s), expected, "{root} from {:?}, all: {all}", here.worktree);
            assert_eq!(here.contains(Path::new(root), all), expected, "contains agrees with the filter");
        }
    }

    fn session(agent: AgentKind, id: &str, parent: Option<&str>, updated: i64) -> Session {
        Session {
            handle: SessionRef {
                agent,
                native_id: id.into(),
                location: SessionLocation::JsonlFile { path: format!("/t/{id}").into() },
            },
            title: Some(id.into()),
            slug: None,
            project_root: "/w/app".into(),
            git_branch: None,
            created: None,
            updated: jiff::Timestamp::from_second(updated).ok(),
            model: None,
            usage: Default::default(),
            status: SessionStatus::Idle,
            parent: parent.map(str::to_string),
            agent_version: None,
            size_bytes: None,
        }
    }

    fn oc(id: &str, parent: Option<&str>, updated: i64) -> Session {
        session(AgentKind::OpenCode, id, parent, updated)
    }

    /// The tree as nested ids, for comparing at a glance: `a(b(c) d)`.
    fn shape(sessions: &[Session], nodes: &[Node]) -> String {
        nodes
            .iter()
            .map(|n| {
                let id = &sessions[n.index].handle.native_id;
                if n.children.is_empty() { id.clone() } else { format!("{id}({})", shape(sessions, &n.children)) }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn everything(nodes: &[Node]) -> Vec<(usize, usize)> {
        flatten(nodes, &|_| true)
    }

    #[test]
    fn children_nest_to_any_depth_newest_first() {
        // Listed newest first, as every listing is; the children's own order
        // is by time, not by where they happen to sit in the list.
        let all = vec![
            oc("root", None, 100),
            oc("old", Some("root"), 10),
            oc("new", Some("root"), 90),
            oc("grand", Some("new"), 80),
            oc("great", Some("grand"), 70),
            oc("other", None, 5),
        ];
        let tree = group_children(&all);
        assert_eq!(shape(&all, &tree), "root(new(grand(great)) old) other");
        assert_eq!(tree[0].descendants(), 4);
        assert_eq!(tree[1].descendants(), 0);
        let depths: Vec<usize> = everything(&tree).iter().map(|&(_, d)| d).collect();
        assert_eq!(depths, vec![0, 1, 2, 3, 1, 0]);
    }

    #[test]
    fn equal_times_keep_the_listing_order() {
        let all = vec![oc("root", None, 9), oc("b", Some("root"), 5), oc("a", Some("root"), 5), oc("c", Some("root"), 5)];
        assert_eq!(shape(&all, &group_children(&all)), "root(b a c)");
    }

    /// A child whose parent is not in the list is a root: a Codex thread
    /// whose parent rollout is gone, a child archived alone, a match in a
    /// filtered list.
    #[test]
    fn a_child_without_its_parent_is_a_root() {
        let all = vec![oc("kid", Some("ghost"), 9), oc("root", None, 8), oc("sub", Some("root"), 7)];
        let tree = group_children(&all);
        assert_eq!(shape(&all, &tree), "kid root(sub)");
        // The same list with the parent filtered out.
        let some: Vec<&Session> = vec![&all[2]];
        assert_eq!(group_children(&some).len(), 1);
    }

    /// Ids are only unique within an agent: a child is never filed under
    /// another agent's session that happens to share the id.
    #[test]
    fn a_parent_is_matched_within_its_own_agent() {
        let all = vec![
            session(AgentKind::ClaudeCode, "same", None, 9),
            session(AgentKind::OpenCode, "kid", Some("same"), 8),
            session(AgentKind::OpenCode, "same", None, 7),
        ];
        let tree = group_children(&all);
        assert_eq!(shape(&all, &tree), "same same(kid)");
        assert_eq!(tree[0].descendants(), 0, "claude's `same` has no child");
        assert_eq!(tree[1].children[0].index, 1);
        let alone = vec![session(AgentKind::ClaudeCode, "same", None, 9), session(AgentKind::OpenCode, "kid", Some("same"), 8)];
        assert_eq!(group_children(&alone).len(), 2, "no opencode `same`, so the child is a root");
    }

    /// Data can be wrong. A cycle, or a session its own parent, must not
    /// hang the grouping or lose a session; it is cut at the member that
    /// comes first in the list.
    #[test]
    fn a_cycle_is_cut_deterministically_and_loses_nobody() {
        let all = vec![
            oc("c", Some("a"), 9), // hangs off the cycle
            oc("a", Some("b"), 8),
            oc("b", Some("a"), 7),
            oc("me", Some("me"), 6),
        ];
        let tree = group_children(&all);
        // `a` comes first of the cycle, so it is the root; `b` is under it,
        // and `c` stays with its parent.
        assert_eq!(shape(&all, &tree), "a(c b) me");
        let mut seen: Vec<usize> = everything(&tree).iter().map(|&(i, _)| i).collect();
        seen.sort();
        assert_eq!(seen, vec![0, 1, 2, 3]);
        // And it is the same every time.
        assert_eq!(group_children(&all), tree);
    }

    #[test]
    fn however_the_parents_point_every_session_appears_once() {
        // A deterministic scramble: 300 sessions whose parents are other
        // sessions, ghosts, themselves, or nothing.
        let mut x: u64 = 0x9e3779b97f4a7c15;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let n = 300;
        let all: Vec<Session> = (0..n)
            .map(|i| {
                let parent = match next() % 5 {
                    0 => None,
                    1 => Some(format!("s{}", next() % n as u64)),
                    2 => Some(format!("s{i}")),
                    3 => Some("ghost".to_string()),
                    _ => Some(format!("s{}", next() % n as u64)),
                };
                oc(&format!("s{i}"), parent.as_deref(), (next() % 50) as i64)
            })
            .collect();
        let tree = group_children(&all);
        let mut seen: Vec<usize> = everything(&tree).iter().map(|&(i, _)| i).collect();
        seen.sort();
        assert_eq!(seen, (0..n).collect::<Vec<_>>(), "each exactly once");
        // Roots stay in the order given.
        assert!(tree.windows(2).all(|w| w[0].index < w[1].index));
    }

    #[test]
    fn flatten_skips_what_is_collapsed() {
        let all = vec![oc("a", None, 9), oc("b", Some("a"), 8), oc("c", Some("b"), 7), oc("d", None, 6), oc("e", Some("d"), 5)];
        let tree = group_children(&all);
        let ids = |open: &dyn Fn(usize) -> bool| -> Vec<String> {
            flatten(&tree, open).iter().map(|&(i, d)| format!("{}{}", "-".repeat(d), all[i].handle.native_id)).collect()
        };
        assert_eq!(ids(&|_| true), ["a", "-b", "--c", "d", "-e"]);
        assert_eq!(ids(&|_| false), ["a", "d"]);
        // `a` open, `b` collapsed: c is hidden, e is hidden.
        assert_eq!(ids(&|i| i == 0), ["a", "-b", "d"]);
    }

    #[test]
    fn descendants_are_everything_below_in_tree_order() {
        let all = vec![oc("a", None, 9), oc("b", Some("a"), 8), oc("c", Some("b"), 7), oc("d", Some("a"), 6), oc("z", None, 5)];
        let ids = |of: &Session| -> Vec<String> {
            descendants_in(&all, of).iter().map(|(d, s)| format!("{d}{}", s.handle.native_id)).collect()
        };
        assert_eq!(ids(&all[0]), ["1b", "2c", "1d"]);
        assert_eq!(ids(&all[1]), ["1c"]);
        assert!(ids(&all[2]).is_empty());
        assert!(ids(&all[4]).is_empty());
        assert!(ids(&oc("not-listed", None, 0)).is_empty());
    }

    /// A project counts what an agent's own picker lists, whichever listing
    /// the sessions came from.
    #[test]
    fn projects_count_the_same_with_children_in_the_listing() {
        let roots = vec![oc("a", None, 9), oc("z", None, 5)];
        let mut with_children = roots.clone();
        with_children.push(oc("b", Some("a"), 8));
        with_children.push(oc("c", Some("b"), 7));
        with_children.push(oc("orphan", Some("ghost"), 6));
        let count = |s: &[Session]| group_projects(s, |_| None, |_| Vec::new())[0].session_count;
        assert_eq!(count(&roots), 2);
        assert_eq!(count(&with_children), 2);
    }

    #[test]
    fn a_child_is_found_by_its_id_and_roots_win_a_shared_prefix() {
        let all = vec![
            oc("ses_tree_root", None, 9),
            oc("ses_tree_root_kid1", Some("ses_tree_root"), 8),
            oc("ses_tree_root_kid2", Some("ses_tree_root_kid1"), 7),
            oc("ses_zzz", None, 6),
        ];
        let found = |q: &str| resolve_in(all.clone(), q).map(|s| s.handle.native_id);
        // Exactly, and by a prefix only the child has.
        assert_eq!(found("ses_tree_root_kid2").unwrap(), "ses_tree_root_kid2");
        assert_eq!(found("ses_tree_root_kid1").unwrap(), "ses_tree_root_kid1");
        assert_eq!(found("opencode:ses_tree_root_kid2").unwrap(), "ses_tree_root_kid2");
        assert_eq!(found("ses_z").unwrap(), "ses_zzz");
        // The whole id of the parent names it, though the children's ids begin with it.
        assert_eq!(found("ses_tree_root").unwrap(), "ses_tree_root");
        // A prefix of the parent and its children names the parent, as it did
        // before children could be named at all.
        assert_eq!(found("ses_tree_r").unwrap(), "ses_tree_root");
        // Two children and nothing to choose between them is still ambiguous.
        assert!(matches!(found("ses_tree_root_k"), Err(ResolveError::Ambiguous { .. })));
        assert!(matches!(found("ses_nothing"), Err(ResolveError::NotFound { .. })));
        // Two roots stay ambiguous.
        let two = vec![oc("ses_a1", None, 9), oc("ses_a2", None, 8), oc("ses_a2_kid", Some("ses_a2"), 7)];
        assert!(matches!(resolve_in(two, "ses_a"), Err(ResolveError::Ambiguous { .. })));
    }

    #[test]
    fn only_opencode_takes_a_sessions_children_with_it() {
        assert!(deletes_descendants(AgentKind::OpenCode));
        for agent in [AgentKind::ClaudeCode, AgentKind::JCode, AgentKind::Codex, AgentKind::Antigravity] {
            assert!(!deletes_descendants(agent), "{agent}");
        }
    }
}
