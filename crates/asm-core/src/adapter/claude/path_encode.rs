//! Exact port of Claude Code's project-directory encoder (binary 2.1.233):
//!
//! ```js
//! function mAo(e){ return e.replace(/[^a-zA-Z0-9]/g, "-") }
//! function Iot(e){ let t=0; for(let r=0;r<e.length;r++) t=(t<<5)-t+e.charCodeAt(r)|0; return t }
//! function wDy(e){ return Math.abs(Iot(e)).toString(36) }
//! function WE(e){ let t=mAo(e); if(t.length<=200) return t; return `${t.slice(0,200)}-${wDy(e)}` }
//! ```
//!
//! Port gotchas (all deliberate here):
//! - The hash runs over UTF-16 code units of the RAW path, while
//!   sanitization/truncation applies to the sanitized string.
//! - `(t<<5)-t` is `t*31` in wrapping i32 arithmetic.
//! - JS `Math.abs(-2147483648)` is 2147483648 — widen to i64 before abs.
//! - The regex operates per UTF-16 unit, so an astral character becomes
//!   TWO dashes.
//!
//! This encoder exists for WRITE paths (relocate). Reads never invert it —
//! they trust the `cwd` field inside transcripts.

const MAX_LEN: usize = 200;

pub fn encode_project_dir(cwd: &str) -> String {
    let sanitized: String = cwd
        .encode_utf16()
        .map(|unit| match unit {
            0x30..=0x39 | 0x41..=0x5A | 0x61..=0x7A => unit as u8 as char,
            _ => '-',
        })
        .collect();
    // `sanitized` is pure ASCII, so chars == UTF-16 units == bytes.
    if sanitized.len() <= MAX_LEN {
        return sanitized;
    }
    format!("{}-{}", &sanitized[..MAX_LEN], base36(js_hash(cwd).unsigned_abs() as u64))
}

fn js_hash(s: &str) -> i32 {
    let mut hash: i32 = 0;
    for unit in s.encode_utf16() {
        hash = hash.wrapping_mul(31).wrapping_add(unit as i32);
    }
    hash
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_paths_sanitize_only() {
        assert_eq!(
            encode_project_dir("/home/samimishal/projects/rust/agent-sessions-manager"),
            "-home-samimishal-projects-rust-agent-sessions-manager"
        );
        assert_eq!(encode_project_dir("/a/b.c_d e"), "-a-b-c-d-e");
    }

    #[test]
    fn js_hash_matches_reference_values() {
        // Oracle values computed with the JS algorithm
        // ((t<<5)-t+charCodeAt|0, i.e. i32-wrapping t*31+unit):
        assert_eq!(js_hash("a"), 97);
        assert_eq!(js_hash("abc"), 96354);
        assert_eq!(js_hash("/home/user/project"), -1588831306);
    }

    #[test]
    fn long_paths_truncate_and_append_base36_hash() {
        let long = format!("/home/user/{}", "x".repeat(300));
        let encoded = encode_project_dir(&long);
        let (prefix, hash) = encoded.split_at(MAX_LEN);
        assert_eq!(prefix, {
            let sanitized: String =
                long.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
            sanitized[..MAX_LEN].to_string()
        });
        assert!(hash.starts_with('-'));
        assert!(hash[1..].chars().all(|c| c.is_ascii_alphanumeric()));
        // JS abs quirk: hash of the RAW path, base36, lowercase.
        assert_eq!(&hash[1..], base36(js_hash(&long).unsigned_abs() as u64));
    }

    #[test]
    fn astral_chars_become_two_dashes() {
        // '😀' is one code point but two UTF-16 units.
        assert_eq!(encode_project_dir("a😀b"), "a--b");
    }
}
