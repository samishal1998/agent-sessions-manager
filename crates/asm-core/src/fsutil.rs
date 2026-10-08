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

/// A temp-file suffix no other writer uses: the pid alone is shared by every
/// thread of the web server, and two of them writing one file would rename
/// each other's half-written temp into place.
fn unique() -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!("{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// How many times (10 ms apart) a lock that looks held is looked at again.
#[cfg(unix)]
const HELD_RETRIES: u32 = 20;

/// An exclusive flock on `path` (created if missing, never truncated or
/// removed), or `None` while another process holds it. Released when the
/// file is dropped, or however the process ends. Codex and Antigravity mark
/// a session in use this way; asm takes the same lock to write one. A lock
/// that looks held is looked at again for about 200 ms before it is believed
/// (see the loop).
pub fn lock_exclusive(path: &Path) -> Result<Option<fs::File>, CoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| CoreError::io(parent, e))?;
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| CoreError::io(path, e))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // A flock belongs to the open file description, not to the process,
        // so a child forked by another thread of this process holds a copy of
        // a descriptor we just dropped until it execs. That lasts a few
        // milliseconds; a real holder (another program) lasts. Look again for
        // a short while before saying it is held.
        let mut waited = 0;
        loop {
            // Safety: flock on a descriptor this function owns.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            let e = std::io::Error::last_os_error();
            // Only "someone holds it" means held; a filesystem that cannot
            // lock at all (NFS without lockd) is an error to show as such.
            if e.kind() != std::io::ErrorKind::WouldBlock {
                return Err(CoreError::io(path, e));
            }
            if waited == HELD_RETRIES {
                return Ok(None);
            }
            waited += 1;
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    Ok(Some(file))
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
    let tmp = parent.join(format!(".{name}.tmp-{}", unique()));
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
    let tmp = parent.join(format!(".{name}.tmp-{}", unique()));
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

    /// A flock belongs to the open file description, and a child forked by
    /// another thread holds a copy of the descriptor until it execs (a few
    /// milliseconds). Taking the lock again just after dropping it must not
    /// report it held because of that.
    #[cfg(unix)]
    #[test]
    fn a_lock_just_dropped_is_not_held_by_a_child_forked_meanwhile() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let spawner = {
            let (stop, cwd) = (stop.clone(), dir.path().to_path_buf());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = std::process::Command::new("true").current_dir(&cwd).status();
                }
            })
        };
        let mut held = 0;
        for _ in 0..400 {
            match lock_exclusive(&path).unwrap() {
                Some(lock) => drop(lock),
                None => held += 1,
            }
        }
        stop.store(true, Ordering::Relaxed);
        spawner.join().unwrap();
        assert_eq!(held, 0, "the lock was reported held {held} times by a descriptor nobody owns");
    }

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
