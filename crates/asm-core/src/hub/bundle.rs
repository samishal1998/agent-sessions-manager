//! A session as a set of files ready to upload, and the per-agent rules for
//! what those files are and what counts as the conversation having changed.
//!
//! Collecting is a read, so it runs on live sessions — that is the point of
//! backing up to a hub. Whatever is half-written right now is left out: a
//! transcript is cut at its last complete line, and a sidecar file that
//! vanishes mid-walk is skipped rather than failing the whole push.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::manifest::FileEntry;
use crate::adapter::Adapter;
use crate::model::{AgentKind, Session};
use crate::{CoreError, fsutil};

/// A file as it will be uploaded: where its bytes come from, already hashed.
pub struct Staged {
    pub entry: FileEntry,
    pub source: Source,
}

pub enum Source {
    Path(PathBuf),
    /// Content that is not a file on disk as-is — a transcript with local
    /// bookkeeping removed, a database row as JSON.
    Bytes(Vec<u8>),
    Symlink,
}

pub struct Bundle {
    pub files: Vec<Staged>,
    /// The conversation's identity, by the agent's own rule.
    pub canonical: String,
    pub extra: Value,
}

impl Staged {
    pub fn bytes(path: &str, bytes: Vec<u8>) -> Staged {
        Staged {
            entry: FileEntry {
                path: path.to_string(),
                sha256: Some(fsutil::sha256_hex(&bytes)),
                size: bytes.len() as u64,
                symlink: None,
            },
            source: Source::Bytes(bytes),
        }
    }

    /// `None` when the file disappeared between listing and hashing.
    pub fn file(path: &str, source: &Path) -> Result<Option<Staged>, CoreError> {
        let size = match fs::metadata(source) {
            Ok(m) => m.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(CoreError::io(source, e)),
        };
        let sha = match fsutil::sha256_file(source) {
            Ok(sha) => sha,
            Err(CoreError::Io { source: e, .. }) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        Ok(Some(Staged {
            entry: FileEntry { path: path.to_string(), sha256: Some(sha), size, symlink: None },
            source: Source::Path(source.to_path_buf()),
        }))
    }

    pub fn symlink(path: &str, target: String) -> Staged {
        Staged {
            entry: FileEntry { path: path.to_string(), sha256: None, size: 0, symlink: Some(target) },
            source: Source::Symlink,
        }
    }
}

/// Every file and symlink under `dir`, named `<prefix>/<relative path>`.
/// Anything that vanishes during the walk is skipped: sidecars of a live
/// session change while they are read.
pub fn walk(dir: &Path, prefix: &str) -> Result<Vec<Staged>, CoreError> {
    let mut out = Vec::new();
    walk_into(dir, prefix, &mut out)?;
    Ok(out)
}

fn walk_into(dir: &Path, prefix: &str, out: &mut Vec<Staged>) -> Result<(), CoreError> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(CoreError::io(dir, e)),
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let Some(name) = entry.file_name().to_str().map(String::from) else { continue };
        let path = entry.path();
        let rel = format!("{prefix}/{name}");
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if meta.is_symlink() {
            if let Ok(target) = fs::read_link(&path) {
                out.push(Staged::symlink(&rel, target.display().to_string()));
            }
        } else if meta.is_dir() {
            walk_into(&path, &rel, out)?;
        } else if meta.is_file()
            && let Some(staged) = Staged::file(&rel, &path)?
        {
            out.push(staged);
        }
    }
    Ok(())
}

/// Bytes up to and including the last newline. What follows it is a record
/// still being written, and a copy that captured half of it could never
/// agree with the finished file.
pub fn complete_lines(raw: &[u8]) -> &[u8] {
    match raw.iter().rposition(|&b| b == b'\n') {
        Some(i) => &raw[..=i],
        None => &[],
    }
}

/// Gather a session for upload.
pub fn collect(session: &Session) -> Result<Bundle, CoreError> {
    let adapter = crate::ops::adapter_for(session.handle.agent)
        .ok_or(CoreError::StoreNotFound { path: PathBuf::new() })?;
    match &adapter {
        Adapter::ClaudeCode(a) => crate::adapter::claude::hub::collect(a, session),
        Adapter::Codex(a) => crate::adapter::codex::hub::collect(a, session),
        Adapter::JCode(a) => crate::adapter::jcode::hub::collect(a, session),
        Adapter::OpenCode(a) => crate::adapter::opencode::hub::collect(a, session),
        Adapter::Antigravity(a) => crate::adapter::antigravity::hub::collect(a, session),
    }
}

/// The cheap "did it change" check a push compares before reading a
/// session: the search index's, except for OpenCode, whose bundle carries
/// every descendant session too.
pub fn fingerprint(session: &Session) -> String {
    if matches!(session.handle.location, crate::model::SessionLocation::SqliteRow { .. })
        && let Some(Adapter::OpenCode(a)) = crate::ops::adapter_for(session.handle.agent)
        && let Some(fp) = crate::adapter::opencode::hub::fingerprint(&a, session)
    {
        return fp;
    }
    let fingerprint = crate::index::fingerprint(session);
    // jcode keeps a running session's turns in a journal beside the
    // snapshot until it checkpoints; a push must see those turns arrive.
    if session.handle.agent == AgentKind::JCode
        && let crate::model::SessionLocation::JsonlFile { path } = &session.handle.location
    {
        let journal = std::fs::metadata(crate::adapter::jcode::hub::journal_of(path)).map(|m| m.len()).unwrap_or(0);
        return format!("{fingerprint}:journal:{journal}");
    }
    fingerprint
}

/// Create `dir` one level at a time from `base`'s parent, refusing to pass
/// through anything that is not a real directory. A symlink planted by an
/// earlier pull, or by a hostile manifest, must never become a way to write
/// outside the session's own directories.
pub(crate) fn real_dirs(base: &Path, dir: &Path) -> Result<(), CoreError> {
    let trusted = base.parent().unwrap_or(base);
    fs::create_dir_all(trusted).map_err(|e| CoreError::io(trusted, e))?;
    let mut at = trusted.to_path_buf();
    for part in dir.strip_prefix(trusted).unwrap_or(Path::new("")).components() {
        at.push(part);
        match fs::symlink_metadata(&at) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(CoreError::Invalid {
                    msg: format!("{} is not a directory; refusing to write through it", at.display()),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&at).map_err(|e| CoreError::io(&at, e))?;
            }
            Err(e) => return Err(CoreError::io(&at, e)),
        }
    }
    Ok(())
}

/// What this machine last agreed with the hub about a session: the
/// conversation's identity then, and that revision's files by path.
#[derive(Debug, Default)]
pub struct Base {
    pub canonical: String,
    pub files: std::collections::HashMap<String, String>,
}

/// Refine a comparison of the two copies' contents with what this machine
/// last synced. A copy unchanged here since then takes the hub's, whatever
/// the hub did (rows deleted, a title changed); a copy changed here is never
/// silently replaced. With no record, the content comparison stands.
pub(crate) fn with_base(content: InstallOutcome, local: &str, hub: &str, base: Option<&Base>) -> InstallOutcome {
    if local == hub {
        return InstallOutcome::InSync;
    }
    let Some(base) = base else { return content };
    match (local == base.canonical, hub == base.canonical) {
        (true, _) => InstallOutcome::Replaced,
        (false, true) => InstallOutcome::Ahead,
        (false, false) if content == InstallOutcome::Replaced => InstallOutcome::Diverged,
        (false, false) => content,
    }
}

/// How a pull changed this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "result")]
pub enum InstallOutcome {
    /// The session was not here; it is now.
    New,
    /// It was here and behind; the hub's newer content was appended.
    FastForward { appended: u64 },
    /// It was here and behind, and is kept as rows rather than a log: the
    /// older copy was backed up and replaced by the hub's.
    Replaced,
    InSync,
    /// This machine has more than the hub; push it.
    Ahead,
    /// Both sides continued it. Nothing was changed.
    Diverged,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Installed {
    pub outcome: InstallOutcome,
    pub project_root: PathBuf,
    pub path: PathBuf,
}

/// Where a pulled session with no copy here goes: `--project-dir`, else the
/// pushing machine's path resolved against this home. Canonical, and it
/// must exist — an agent binds a session to a real directory.
pub(crate) fn target_dir(manifest: &super::manifest::Manifest, project_dir: Option<&Path>) -> Result<PathBuf, CoreError> {
    let wanted = match project_dir {
        Some(dir) => dir.to_path_buf(),
        None => crate::ir::PortablePath(manifest.project_root_portable.clone()).resolve(),
    };
    if !wanted.is_dir() {
        return Err(CoreError::Invalid {
            msg: format!(
                "{} does not exist on this machine; pass --project-dir to put the session \
                 somewhere else",
                wanted.display()
            ),
        });
    }
    wanted.canonicalize().map_err(|e| CoreError::io(&wanted, e))
}

/// Agents whose pull asm can perform today. The rest are backed up only;
/// each is added once its own CLI has been seen to resume a restored copy.
pub fn restorable(agent: AgentKind) -> bool {
    // Every agent, now. Kept as a function: an agent added later starts out
    // backed up only, until its own CLI has resumed a restored copy.
    matches!(
        agent,
        AgentKind::ClaudeCode | AgentKind::OpenCode | AgentKind::JCode | AgentKind::Codex | AgentKind::Antigravity
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_last_synced_decides_between_the_copies() {
        use InstallOutcome::*;
        let base = Base { canonical: "synced".into(), ..Default::default() };
        let b = Some(&base);
        // Unchanged here: take the hub's, even if its rows only shrank.
        assert_eq!(with_base(Diverged, "synced", "hub", b), Replaced);
        // Unchanged there: this copy is ahead, whatever the rows say.
        assert_eq!(with_base(Replaced, "here", "synced", b), Ahead);
        // Both moved: never replaced silently.
        assert_eq!(with_base(Replaced, "here", "hub", b), Diverged);
        assert_eq!(with_base(Ahead, "here", "hub", b), Ahead);
        // No record: the contents decide.
        assert_eq!(with_base(Replaced, "here", "hub", None), Replaced);
        assert_eq!(with_base(Diverged, "same", "same", None), InSync);
    }

    #[test]
    fn a_torn_final_line_is_left_out() {
        assert_eq!(complete_lines(b"{\"a\":1}\n{\"b\""), b"{\"a\":1}\n");
        assert_eq!(complete_lines(b"{\"a\":1}\n"), b"{\"a\":1}\n");
        assert_eq!(complete_lines(b"{\"half"), b"");
    }

    #[test]
    fn walking_names_files_under_their_prefix_and_keeps_symlinks_unfollowed() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/a.jsonl"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/somewhere/else", dir.path().join("link")).unwrap();
        let staged = walk(dir.path(), "session-dir").unwrap();
        let names: Vec<&str> = staged.iter().map(|s| s.entry.path.as_str()).collect();
        assert!(names.contains(&"session-dir/sub/a.jsonl"), "{names:?}");
        #[cfg(unix)]
        {
            let link = staged.iter().find(|s| s.entry.path == "session-dir/link").unwrap();
            assert_eq!(link.entry.symlink.as_deref(), Some("/somewhere/else"));
        }
        assert!(walk(&dir.path().join("missing"), "x").unwrap().is_empty());
    }
}
