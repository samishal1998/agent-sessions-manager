//! The one process-liveness probe. Every "is this session live" and "is
//! this store busy" check routes through here.
//!
//! There used to be three, each testing `/proc/<pid>`, and off Linux they
//! disagreed about which way to fail: Claude's refused every write on macOS,
//! while OpenCode's and jcode's reported every running instance as dead —
//! so asm would have written into a busy OpenCode store and archived a jcode
//! session mid-turn. `kill(pid, 0)` asks the kernel on every unix.

/// True when a process with this pid exists on this machine.
#[cfg(unix)]
pub fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // Pid 0 means "every process in my group" to kill(2), which would
    // report this process itself as alive.
    if pid <= 0 {
        return false;
    }
    // Signal 0 performs the existence and permission checks without
    // delivering anything.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    // EPERM: the process exists but belongs to someone else. It is still
    // alive, and a live foreign process holding a lock must be respected.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// No release targets a non-unix platform. Assume alive, which only ever
/// makes asm refuse a write.
#[cfg(not(unix))]
pub fn alive(_pid: u32) -> bool {
    true
}

/// This machine's name. Linux keeps it in files; macOS has neither of
/// those, so the last resort is the `hostname` command every unix ships.
pub fn hostname() -> Option<String> {
    if let Ok(name) = std::env::var("HOSTNAME")
        && !name.trim().is_empty()
    {
        return Some(name.trim().to_string());
    }
    for path in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
        if let Ok(name) = std::fs::read_to_string(path)
            && !name.trim().is_empty()
        {
            return Some(name.trim().to_string());
        }
    }
    let output = std::process::Command::new("hostname").output().ok()?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive() {
        assert!(alive(std::process::id()));
    }

    #[test]
    fn a_reaped_child_is_not() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!alive(pid));
    }

    #[test]
    fn this_machine_has_a_name() {
        assert!(hostname().is_some_and(|n| !n.is_empty()));
    }

    #[test]
    fn pid_zero_is_not_mistaken_for_ourselves() {
        assert!(!alive(0));
    }

    /// Pid 1 always exists and is never ours to signal: the EPERM branch.
    #[test]
    fn a_process_we_may_not_signal_is_still_alive() {
        assert!(alive(1));
    }
}
