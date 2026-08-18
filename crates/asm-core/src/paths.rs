//! Our own state directories (strict XDG). `ASM_DATA_DIR` overrides the
//! root for tests and portable setups.

use std::path::PathBuf;

pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ASM_DATA_DIR").map(PathBuf::from) {
        return Some(dir);
    }
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| etcetera::home_dir().ok().map(|h| h.join(".local/share")))
        .map(|b| b.join("asm"))
}

/// Backups written before every destructive operation:
/// `<data>/backups/<agent>/<session-id>/<epoch-millis>/`.
pub fn backup_dir(agent: &str, session_id: &str) -> Option<PathBuf> {
    let now = jiff::Timestamp::now().as_millisecond();
    data_dir().map(|d| d.join("backups").join(agent).join(session_id).join(now.to_string()))
}

/// The archive store (git-friendly; sync-ready): `<data>/archive/<agent>/<id>/`.
pub fn archive_dir(agent: &str, session_id: &str) -> Option<PathBuf> {
    data_dir().map(|d| d.join("archive").join(agent).join(session_id))
}
