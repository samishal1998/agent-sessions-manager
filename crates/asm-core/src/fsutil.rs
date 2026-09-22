//! Filesystem helpers shared by adapters: cross-device-safe moves,
//! recursive copies, newline-safe appends to JSONL files, atomic writes,
//! and streaming content hashes.

use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

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

/// Replace `path` with `bytes` so that a reader sees either the old file or
/// the new one, never a torn mixture: write a sibling, flush it to disk,
/// then rename over. The sibling lives in the same directory because a
/// rename across filesystems is not atomic (and fails with EXDEV).
///
/// The file is 0600 on unix: everything written this way (hub state, a
/// machine credential, transcripts) is the user's alone, and a mode set
/// after the rename would leave a window where it is not.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = parent.join(format!(".{name}.tmp-{}", std::process::id()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp).map_err(|e| CoreError::io(&tmp, e))?;
        file.write_all(bytes).map_err(|e| CoreError::io(&tmp, e))?;
        // Without this a crash shortly after the rename can leave the new
        // name pointing at an empty file on some filesystems.
        file.sync_all().map_err(|e| CoreError::io(&tmp, e))?;
        fs::rename(&tmp, path).map_err(|e| CoreError::io(path, e))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// `write_atomic` for content already in a file: copied by the kernel in
/// chunks, never read into memory — sidecar files reach hundreds of MB.
pub fn copy_atomic(from: &Path, to: &Path) -> Result<(), CoreError> {
    let parent = to.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = to.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = parent.join(format!(".{name}.tmp-{}", std::process::id()));
    let result = (|| {
        fs::copy(from, &tmp).map_err(|e| CoreError::io(&tmp, e))?;
        fs::File::open(&tmp).and_then(|f| f.sync_all()).map_err(|e| CoreError::io(&tmp, e))?;
        fs::rename(&tmp, to).map_err(|e| CoreError::io(to, e))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Hex sha256 of a file, read in chunks: session files reach tens of
/// megabytes and sidecar bundles hundreds, so never read one whole.
pub fn sha256_file(path: &Path) -> Result<String, CoreError> {
    let mut file = fs::File::open(path).map_err(|e| CoreError::io(path, e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| CoreError::io(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Hex sha256 of bytes already in memory.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_and_leaves_no_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("state.json")]);
    }

    #[test]
    fn write_atomic_into_a_missing_directory_fails_without_litter() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write_atomic(&dir.path().join("nope/state.json"), b"x").is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    /// The streaming hash must agree with the one-shot hash, including
    /// across the 256 KiB read boundary.
    #[test]
    fn file_hash_matches_in_memory_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob");
        let bytes: Vec<u8> = (0..(600 * 1024)).map(|i| (i % 251) as u8).collect();
        fs::write(&path, &bytes).unwrap();
        assert_eq!(sha256_file(&path).unwrap(), sha256_hex(&bytes));
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
