//! pi and omp over the hub: backed up, not yet restored.
//!
//! The session file is append-only JSONL, except omp's title slot, the
//! first line, which omp rewrites in place on a rename. So the
//! conversation's identity, and the comparison of two copies, are the
//! complete lines from the header on: a rename is not a divergence.

use std::path::PathBuf;

use serde_json::json;

use super::PiAdapter;
use crate::hub::bundle::{self, Bundle, Staged};
use crate::hub::manifest::Manifest;
use crate::model::{Session, SessionLocation};
use crate::{CoreError, fsutil};

pub(crate) const FILE: &str = "session.jsonl";

/// `raw` without omp's title slot, cut at its last complete line.
fn body(raw: &[u8]) -> &[u8] {
    let complete = bundle::complete_lines(raw);
    match complete.iter().position(|&b| b == b'\n') {
        Some(end) if complete.starts_with(b"{\"type\":\"title\"") => &complete[end + 1..],
        _ => complete,
    }
}

fn file_of(session: &Session) -> Result<&PathBuf, CoreError> {
    match &session.handle.location {
        SessionLocation::JsonlFile { path } => Ok(path),
        _ => Err(CoreError::Invalid { msg: format!("{} session has no file", session.handle.agent) }),
    }
}

pub(crate) fn collect(adapter: &PiAdapter, session: &Session) -> Result<Bundle, CoreError> {
    let path = file_of(session)?;
    let raw = std::fs::read(path).map_err(|e| CoreError::io(path, e))?;
    let canonical = fsutil::sha256_hex(body(&raw));
    let rel = path.strip_prefix(adapter.sessions_dir()).unwrap_or(path);
    Ok(Bundle {
        files: vec![Staged::bytes(FILE, bundle::complete_lines(&raw).to_vec())],
        canonical,
        extra: json!({ "session_rel": rel.display().to_string() }),
    })
}

/// How the file here relates to the hub's copy, without writing anything.
pub(crate) fn relate(
    session: &Session,
    manifest: &Manifest,
    blob: &dyn Fn(&str) -> Result<PathBuf, CoreError>,
) -> Result<Option<bundle::Relation>, CoreError> {
    let sha = manifest
        .files
        .iter()
        .find(|f| f.path == FILE)
        .and_then(|f| f.sha256.as_deref())
        .ok_or_else(|| CoreError::Invalid { msg: "bundle has no session file".into() })?;
    let src = blob(sha)?;
    let incoming = std::fs::read(&src).map_err(|e| CoreError::io(&src, e))?;
    let path = file_of(session)?;
    let local = std::fs::read(path).map_err(|e| CoreError::io(path, e))?;
    Ok(bundle::Relation::of(bundle::prefix_outcome(body(&local), body(&incoming))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_slot_is_not_part_of_the_body() {
        let header = "{\"type\":\"session\",\"id\":\"x\"}\n";
        let slot = |t: &str| format!("{{\"type\":\"title\",\"title\":\"{t}\"}}\n{header}");
        assert_eq!(body(slot("a").as_bytes()), header.as_bytes());
        assert_eq!(body(slot("a").as_bytes()), body(slot("renamed").as_bytes()));
        // pi has no slot; a torn last line is not part of it either.
        assert_eq!(body(format!("{header}{{\"type\":\"mess").as_bytes()), header.as_bytes());
    }
}
