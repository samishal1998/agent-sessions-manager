//! Deterministic target-id derivation.
//!
//! Same source session → same target ids, every time. OpenCode ids are
//! prefix-checked strings; a sequence counter leads the hash so that
//! lexicographic id order equals conversation order (OpenCode orders parts
//! and messages by id).

use sha2::{Digest, Sha256};

fn hex(input: &str, len: usize) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut out = String::with_capacity(len);
    for byte in digest {
        for c in [byte >> 4, byte & 0xf] {
            out.push(char::from_digit(c as u32, 16).unwrap());
            if out.len() == len {
                return out;
            }
        }
    }
    out
}

const NS: &str = "asm-import:v1";

pub fn opencode_session_id(source_agent: &str, source_id: &str) -> String {
    format!("ses_{}", hex(&format!("{NS}:{source_agent}:{source_id}"), 26))
}

pub fn opencode_message_id(session_seed: &str, index: usize, source_id: &str) -> String {
    format!("msg_{index:08x}{}", hex(&format!("{NS}:{session_seed}:msg:{source_id}"), 18))
}

pub fn opencode_part_id(session_seed: &str, index: usize) -> String {
    format!("prt_{index:08x}{}", hex(&format!("{NS}:{session_seed}:prt:{index}"), 18))
}

/// Deterministic UUID (v4-shaped) for a Claude target session or record.
pub fn claude_uuid(source_agent: &str, source_id: &str, discriminator: &str) -> String {
    let digest = Sha256::digest(format!("{NS}:{source_agent}:{source_id}:{discriminator}"));
    let mut bytes: [u8; 16] = digest[..16].try_into().unwrap();
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
    let h: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}{}{}{}-{}{}-{}{}-{}{}-{}{}{}{}{}{}",
        h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], h[8], h[9], h[10], h[11], h[12], h[13],
        h[14], h[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_deterministic_and_shaped() {
        let a = opencode_session_id("claude-code", "abc");
        assert_eq!(a, opencode_session_id("claude-code", "abc"));
        assert!(a.starts_with("ses_") && a.len() == 4 + 26);

        let m0 = opencode_message_id("seed", 0, "u1");
        let m1 = opencode_message_id("seed", 1, "u2");
        assert!(m0 < m1, "sequence prefix keeps lexicographic order");

        let u = claude_uuid("opencode", "ses_x", "session");
        assert_eq!(u, claude_uuid("opencode", "ses_x", "session"));
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4", "version nibble");
    }
}
