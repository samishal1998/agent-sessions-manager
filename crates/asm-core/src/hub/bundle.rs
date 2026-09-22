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
    crate::index::fingerprint(session)
}

/// How a pull changed this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "result")]
pub enum InstallOutcome {
    /// The session was not here; it is now.
    New,
    /// It was here and behind; the hub's newer content was appended.
    FastForward { appended: u64 },
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

/// Agents whose pull asm can perform today. The rest are backed up only;
/// each is added once its own CLI has been seen to resume a restored copy.
pub fn restorable(agent: AgentKind) -> bool {
    matches!(agent, AgentKind::ClaudeCode)
}

#[cfg(test)]
mod tests {
    use super::*;

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
