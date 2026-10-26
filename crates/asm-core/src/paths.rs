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

/// `<data>/tmp`, private whatever the umask: curl's request and reply
/// files (a join token, a credential), transcript copies on their way to a
/// hub. Created on first use; tightened on every use, in case an older asm
/// made it 0755.
pub fn tmp_dir() -> Result<PathBuf, crate::CoreError> {
    let dir = data_dir()
        .ok_or_else(|| crate::CoreError::Invalid { msg: "cannot determine asm data dir".into() })?
        .join("tmp");
    std::fs::create_dir_all(&dir).map_err(|e| crate::CoreError::io(&dir, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| crate::CoreError::io(&dir, e))?;
    }
    Ok(dir)
}

/// Backups written before every destructive operation:
/// `<data>/backups/<agent>/<session-id>/<epoch-millis>/`.
pub fn backup_dir(agent: &str, session_id: &str) -> Option<PathBuf> {
    data_dir().map(|d| d.join("backups").join(agent).join(session_id).join(backup_stamp().to_string()))
}

/// Now in epoch millis, but never the same twice in this process: two
/// backups of one session in the same millisecond would share a directory.
pub fn backup_stamp() -> i64 {
    static LAST: std::sync::Mutex<i64> = std::sync::Mutex::new(0);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    *last = (*last + 1).max(jiff::Timestamp::now().as_millisecond());
    *last
}

/// The archive store (git-friendly; sync-ready): `<data>/archive/<agent>/<id>/`.
pub fn archive_dir(agent: &str, session_id: &str) -> Option<PathBuf> {
    data_dir().map(|d| d.join("archive").join(agent).join(session_id))
}

/// The user's home directory. Callers that display paths need this to
/// abbreviate them to `~`; hard-coding `/home/<user>` is wrong on macOS.
pub fn home() -> Option<std::path::PathBuf> {
    etcetera::home_dir().ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn backup_stamps_never_repeat_even_across_threads() {
        let stamps: Vec<i64> = std::thread::scope(|s| {
            let workers: Vec<_> = (0..4).map(|_| s.spawn(|| (0..1000).map(|_| super::backup_stamp()).collect::<Vec<_>>())).collect();
            workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
        });
        let unique: std::collections::HashSet<_> = stamps.iter().collect();
        assert_eq!(unique.len(), stamps.len());
    }
}
