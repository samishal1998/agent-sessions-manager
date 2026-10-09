//! Which machine this is, across re-registrations.
//!
//! A hub gives every `join` a fresh machine id, so joining twice from one
//! box left two records and, because the sync state was keyed on that id,
//! forgot what had been synced. The identity here is stable for the box: a
//! salted hash of the operating system's machine id, so the hub never sees
//! the raw value (it is not a secret, but it is not ours to publish). It is
//! what the hub dedupes on and what the local sync state is kept under.

use std::fs;

use sha2::{Digest, Sha256};

use crate::paths;

/// `ASM_MACHINE_UID` overrides detection: tests, and a cloned VM image whose
/// copies would otherwise share an OS machine id.
const ENV: &str = "ASM_MACHINE_UID";

/// A 32-hex-character identity, or None when no stable source can be read
/// or made (no OS machine id and no writable data directory).
pub fn machine_uid() -> Option<String> {
    let raw = std::env::var(ENV).ok().filter(|v| !v.trim().is_empty()).or_else(os_machine_id).or_else(persisted)?;
    Some(digest(&raw))
}

/// The hub accepts only what `digest` produces, so a stray value cannot
/// reach a record.
pub fn valid_uid(uid: &str) -> bool {
    uid.len() == 32 && uid.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn digest(raw: &str) -> String {
    let hash = Sha256::digest(format!("asm machine v1\0{}", raw.trim()).as_bytes());
    hash[..16].iter().map(|b| format!("{b:02x}")).collect()
}

fn os_machine_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        ["/etc/machine-id", "/var/lib/dbus/machine-id"]
            .iter()
            .filter_map(|p| fs::read_to_string(p).ok())
            .map(|s| s.trim().to_string())
            .find(|s| !s.is_empty())
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("ioreg").args(["-rd1", "-c", "IOPlatformExpertDevice"]).output().ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|l| l.contains("IOPlatformUUID"))?
            .split('"')
            .nth(3)
            .map(String::from)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Where the OS offers nothing (a minimal container): an id made once and
/// kept beside the other asm state. Deleting asm's data starts over, which
/// is the same as a new machine.
fn persisted() -> Option<String> {
    let path = paths::data_dir()?.join("machine-uid");
    if let Ok(text) = fs::read_to_string(&path)
        && !text.trim().is_empty()
    {
        return Some(text.trim().to_string());
    }
    let fresh = super::store::random_hex(16).ok()?;
    crate::fsutil::write_atomic(&path, fresh.as_bytes()).ok()?;
    Some(fresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digest_is_what_the_hub_accepts_and_does_not_leak_the_source() {
        let d = digest("abc123\n");
        assert!(valid_uid(&d), "{d}");
        assert_eq!(d, digest("abc123"));
        assert_ne!(d, digest("abc124"));
        assert!(!d.contains("abc123"));
        assert!(!valid_uid("abc") && !valid_uid(&d.to_uppercase()) && !valid_uid(&format!("{d}0")));
    }
}
