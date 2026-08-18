//! Filesystem helpers shared by adapters: cross-device-safe moves,
//! recursive copies, and newline-safe appends to JSONL files.

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::CoreError;

/// Move a file or directory, falling back to copy+remove across
/// filesystems (EXDEV). Never overwrites: callers check the destination.
pub fn move_path(from: &Path, to: &Path) -> Result<(), CoreError> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc_exdev()) => {
            copy_recursive(from, to)?;
            remove_recursive(from)
        }
        Err(e) => Err(CoreError::io(from, e)),
    }
}

const fn libc_exdev() -> i32 {
    18 // EXDEV on Linux; the only platform we currently ship on
}

pub fn copy_recursive(from: &Path, to: &Path) -> Result<(), CoreError> {
    let meta = fs::symlink_metadata(from).map_err(|e| CoreError::io(from, e))?;
    if meta.is_dir() {
        fs::create_dir_all(to).map_err(|e| CoreError::io(to, e))?;
        for entry in fs::read_dir(from).map_err(|e| CoreError::io(from, e))? {
            let entry = entry.map_err(|e| CoreError::io(from, e))?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else if meta.is_symlink() {
        let target = fs::read_link(from).map_err(|e| CoreError::io(from, e))?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, to).map_err(|e| CoreError::io(to, e))?;
        #[cfg(not(unix))]
        return Err(CoreError::Invalid { msg: "symlink copy unsupported on this platform".into() });
        Ok(())
    } else {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|e| CoreError::io(parent, e))?;
        }
        fs::copy(from, to).map_err(|e| CoreError::io(to, e))?;
        Ok(())
    }
}

pub fn remove_recursive(path: &Path) -> Result<(), CoreError> {
    let meta = fs::symlink_metadata(path).map_err(|e| CoreError::io(path, e))?;
    if meta.is_dir() {
        fs::remove_dir_all(path).map_err(|e| CoreError::io(path, e))
    } else {
        fs::remove_file(path).map_err(|e| CoreError::io(path, e))
    }
}

/// Append one line to a JSONL file. If the file does not end in a newline
/// (torn tail from a crashed writer), a newline is inserted first so the
/// appended record starts on its own line.
pub fn append_jsonl_line(path: &Path, line: &str) -> Result<(), CoreError> {
    let needs_newline = match fs::read(path) {
        Ok(bytes) => !bytes.is_empty() && bytes.last() != Some(&b'\n'),
        Err(e) => return Err(CoreError::io(path, e)),
    };
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| CoreError::io(path, e))?;
    let payload = if needs_newline {
        format!("\n{line}\n")
    } else {
        format!("{line}\n")
    };
    file.write_all(payload.as_bytes()).map_err(|e| CoreError::io(path, e))
}
