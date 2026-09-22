//! What a machine uploads for one session, and the rules that make it safe
//! to accept from someone else.
//!
//! A manifest crosses a trust boundary twice: the hub receives it from a
//! machine, and every other machine later installs from it. So everything a
//! file path could be built from is validated here, once, and both sides
//! call the same check. A manifest name never reaches the filesystem without
//! passing `validate`.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fsutil;
use crate::model::AgentKind;

pub const SCHEMA: u32 = 1;

/// Upper bounds that stop a hostile or broken manifest from turning into a
/// resource problem. Far above anything real: the largest session on the
/// development machine has a few hundred sidecar files.
const MAX_FILES: usize = 50_000;
const MAX_PATH: usize = 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub agent: AgentKind,
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    /// Absolute on the machine that pushed it. Display only elsewhere.
    pub project_root: String,
    /// The same path with `${HOME}` tokenized, which is what a pull resolves
    /// on another machine.
    pub project_root_portable: String,
    /// `host/owner/repo`, the project key that survives different paths.
    #[serde(default)]
    pub git_origin: Option<String>,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub agent_version: Option<String>,
    #[serde(default)]
    pub created: Option<Timestamp>,
    #[serde(default)]
    pub updated: Option<Timestamp>,
    /// Set by the hub from the credential that pushed it, never trusted from
    /// the client.
    #[serde(default)]
    pub machine: Option<MachineRef>,
    /// Set by the hub, on the hub's clock.
    #[serde(default)]
    pub pushed_at: Option<Timestamp>,
    /// The conversation's identity for conflict decisions, computed by the
    /// agent's own rule (see `bundle`). Not a file hash.
    pub canonical: String,
    /// The hub revision this push was based on; the hub refuses the push if
    /// it is not the current head.
    #[serde(default)]
    pub parent_rev: Option<String>,
    pub files: Vec<FileEntry>,
    /// Agent data that is not a file — never used as a path.
    #[serde(default)]
    pub extra: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative, `/`-separated, and inside the agent's whitelist.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default)]
    pub size: u64,
    /// A symlink is recorded by its target text and never followed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink: Option<String>,
}

/// The top-level names each agent's bundle may use: exact files, and
/// directories whose contents may follow.
fn layout(agent: AgentKind) -> (&'static [&'static str], &'static [&'static str]) {
    match agent {
        AgentKind::ClaudeCode => {
            (&["transcript.jsonl"], &["session-dir", "file-history", "session-env", "tasks"])
        }
        AgentKind::Codex => (&["rollout.jsonl", "thread.json"], &[]),
        AgentKind::JCode => (&["snapshot.json", "snapshot.bak", "journal.jsonl"], &[]),
        AgentKind::OpenCode => (&["rows.json"], &["session_diff"]),
        AgentKind::Antigravity => (&["conversation.db", "cache.json"], &["brain"]),
    }
}

/// A native id, as every adapter produces them. Deliberately narrow: this
/// becomes a directory name on the hub.
pub fn valid_id(id: &str) -> bool {
    (1..=200).contains(&id.len())
        && id != "."
        && id != ".."
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

pub fn valid_sha(sha: &str) -> bool {
    sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Relative, normal components only. No `..`, no absolute paths, no
/// backslashes (which are separators on another platform), no NUL.
fn valid_rel_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

impl FileEntry {
    pub fn is_symlink(&self) -> bool {
        self.symlink.is_some()
    }
}

/// One hash of which files a bundle holds and what is in them. A link's
/// target is left out: it names a place on one machine's disk.
pub fn files_hash<'a>(files: impl IntoIterator<Item = &'a FileEntry>) -> String {
    let mut lines: Vec<String> = files
        .into_iter()
        .map(|f| format!("{}\0{}", f.path, f.sha256.as_deref().unwrap_or("symlink")))
        .collect();
    lines.sort();
    fsutil::sha256_hex(lines.join("\n").as_bytes())
}

impl Manifest {
    /// Everything a path or a directory name will be built from, checked.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err(format!("unsupported manifest schema {}", self.schema));
        }
        if !valid_id(&self.id) {
            return Err(format!("invalid session id {:?}", self.id));
        }
        if self.files.len() > MAX_FILES {
            return Err(format!("{} files is more than the {MAX_FILES} allowed", self.files.len()));
        }
        let (exact, dirs) = layout(self.agent);
        let mut seen = std::collections::HashSet::new();
        for file in &self.files {
            if !valid_rel_path(&file.path) {
                return Err(format!("invalid file path {:?}", file.path));
            }
            let (top, rest) = match file.path.split_once('/') {
                Some((top, rest)) => (top, Some(rest)),
                None => (file.path.as_str(), None),
            };
            let allowed = match rest {
                None => exact.contains(&top),
                Some(_) => dirs.contains(&top),
            };
            if !allowed {
                return Err(format!("{:?} is not part of a {} session", file.path, self.agent));
            }
            match (&file.sha256, &file.symlink) {
                (Some(sha), None) if valid_sha(sha) => {}
                (None, Some(target)) if !target.is_empty() && !target.contains('\0') => {}
                _ => return Err(format!("{:?} needs exactly one of sha256 or symlink", file.path)),
            }
            if !seen.insert(file.path.as_str()) {
                return Err(format!("{:?} appears twice", file.path));
            }
        }
        // A symlink entry must be a leaf. Otherwise `dir -> /anywhere`
        // followed by `dir/file` would make an install write `file`
        // wherever the link points — outside the session, into any file the
        // pulling user can write.
        let links: std::collections::HashSet<&str> =
            self.files.iter().filter(|f| f.is_symlink()).map(|f| f.path.as_str()).collect();
        for file in &self.files {
            let mut prefix = file.path.as_str();
            while let Some((parent, _)) = prefix.rsplit_once('/') {
                if links.contains(parent) {
                    return Err(format!("{:?} would be written through the symlink {parent:?}", file.path));
                }
                prefix = parent;
            }
        }
        if let Some(parent) = &self.parent_rev
            && !valid_sha(parent)
        {
            return Err(format!("invalid parent revision {parent:?}"));
        }
        Ok(())
    }

    /// The blobs a hub must hold before it accepts this manifest.
    pub fn blob_shas(&self) -> impl Iterator<Item = &str> {
        self.files.iter().filter_map(|f| f.sha256.as_deref())
    }

    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    pub fn key(&self) -> String {
        format!("{}:{}", self.agent, self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn manifest(files: Vec<FileEntry>) -> Manifest {
        Manifest {
            schema: SCHEMA,
            agent: AgentKind::ClaudeCode,
            id: "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43".into(),
            title: None,
            slug: None,
            project_root: "/home/a/code/x".into(),
            project_root_portable: "${HOME}/code/x".into(),
            git_origin: None,
            git_branch: None,
            agent_version: None,
            created: None,
            updated: None,
            machine: None,
            pushed_at: None,
            canonical: SHA.into(),
            parent_rev: None,
            files,
            extra: Value::Null,
        }
    }

    fn file(path: &str) -> FileEntry {
        FileEntry { path: path.into(), sha256: Some(SHA.into()), size: 0, symlink: None }
    }

    #[test]
    fn a_real_claude_layout_is_accepted() {
        let m = manifest(vec![
            file("transcript.jsonl"),
            file("session-dir/subagents/agent-a.jsonl"),
            file("file-history/abc@v2"),
            FileEntry {
                path: "session-dir/tool-results/x".into(),
                sha256: None,
                size: 0,
                symlink: Some("/elsewhere/out.txt".into()),
            },
        ]);
        assert_eq!(m.validate(), Ok(()));
    }

    /// Each of these would write outside the session's own directories if a
    /// machine installed it — the reason validation exists.
    #[test]
    fn paths_that_escape_are_refused() {
        for bad in [
            "../../.ssh/authorized_keys",
            "session-dir/../../../.bashrc",
            "/etc/passwd",
            "session-dir//x",
            "session-dir/./x",
            "session-dir\\..\\x",
            "transcript.jsonl/extra",
            "session-dir",
            "memory/MEMORY.md",
            "jobs/1/state.json",
        ] {
            assert!(manifest(vec![file(bad)]).validate().is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn ids_and_hashes_are_narrow() {
        let mut m = manifest(vec![file("transcript.jsonl")]);
        for id in ["", ".", "..", "a/b", "a b", "a\0b"] {
            m.id = id.into();
            assert!(m.validate().is_err(), "{id:?} was accepted");
        }
        m.id = "session_boar_1788737060725_d6efa80a37259c0a".into();
        m.files = vec![FileEntry {
            path: "transcript.jsonl".into(),
            sha256: Some("ABC".into()),
            size: 0,
            symlink: None,
        }];
        assert!(m.validate().is_err());
    }

    #[test]
    fn one_of_hash_or_symlink_and_no_duplicates() {
        let neither =
            FileEntry { path: "transcript.jsonl".into(), sha256: None, size: 0, symlink: None };
        assert!(manifest(vec![neither]).validate().is_err());
        assert!(manifest(vec![file("transcript.jsonl"), file("transcript.jsonl")]).validate().is_err());
    }

    /// Reported by review: a link entry followed by an entry beneath it
    /// would write through the link on install. Refused at every depth.
    #[test]
    fn nothing_may_live_beneath_a_symlink_entry() {
        let link = |path: &str| FileEntry {
            path: path.into(),
            sha256: None,
            size: 0,
            symlink: Some("/home/victim".into()),
        };
        for (l, f) in [
            ("session-dir/evil", "session-dir/evil/.bashrc"),
            ("session-dir/a", "session-dir/a/b/c/d"),
            ("tasks/x", "tasks/x/y"),
        ] {
            let m = manifest(vec![file("transcript.jsonl"), link(l), file(f)]);
            assert!(m.validate().is_err(), "{f} beneath {l} was accepted");
        }
        // A sibling that merely shares a prefix is fine.
        let m = manifest(vec![file("transcript.jsonl"), link("session-dir/a"), file("session-dir/ab")]);
        assert_eq!(m.validate(), Ok(()));
    }

    /// A layout is per agent: a Claude name in a codex manifest is refused.
    #[test]
    fn names_belong_to_their_agent() {
        let mut m = manifest(vec![file("transcript.jsonl")]);
        m.agent = AgentKind::Codex;
        assert!(m.validate().is_err());
        m.files = vec![file("rollout.jsonl"), file("thread.json")];
        assert_eq!(m.validate(), Ok(()));
    }
}
