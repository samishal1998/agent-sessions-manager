//! `asm update` — replace this binary with the latest release.
//!
//! The same steps `install.sh` performs, so that updating does not mean
//! re-running a shell pipeline: resolve a version, download the asset for
//! this platform, check it against the release's `SHA256SUMS`, and only
//! then put it in place.
//!
//! Two deliberate choices:
//!
//! - **HTTP goes through `curl` (or `wget`), not a linked TLS stack.**
//!   `install.sh` already requires one of them and is verified against the
//!   real release, so this reuses that requirement rather than pulling
//!   rustls or openssl into a binary that otherwise needs neither.
//! - **The download is staged next to the binary it replaces**, not in
//!   `/tmp`, because the final step is a rename and `/tmp` is very often a
//!   different filesystem — where rename fails with `EXDEV`.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

const REPO: &str = "samishal1998/agent-sessions-manager";

/// The triple this binary was built for; the asset names match it.
const fn target_triple() -> &'static str {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "aarch64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64-apple-darwin"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64-apple-darwin"
    }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )))]
    {
        ""
    }
}

fn repo() -> String {
    std::env::var("ASM_REPO").unwrap_or_else(|_| REPO.to_string())
}

/// A mirror, or a `file://` directory, instead of GitHub. Mirrors the
/// installer's variable of the same name so both can be pointed at a
/// staging area.
fn base_url() -> Option<String> {
    std::env::var("ASM_BASE_URL").ok().filter(|s| !s.is_empty())
}

/// Fetch a URL to a file. `curl` first, `wget` as the fallback — the same
/// pair `install.sh` accepts.
fn fetch(url: &str, dest: &Path) -> Result<()> {
    if which("curl") {
        let status = Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(dest)
            .arg(url)
            .status()
            .context("running curl")?;
        if status.success() {
            return Ok(());
        }
        bail!("could not download {url}");
    }
    if which("wget") {
        let status = Command::new("wget")
            .arg("-qO")
            .arg(dest)
            .arg(url)
            .status()
            .context("running wget")?;
        if status.success() {
            return Ok(());
        }
        bail!("could not download {url}");
    }
    bail!("neither curl nor wget is installed; asm update needs one of them")
}

fn fetch_text(url: &str) -> Result<String> {
    let tmp = std::env::temp_dir().join(format!("asm-update-meta-{}", std::process::id()));
    fetch(url, &tmp)?;
    let text = std::fs::read_to_string(&tmp).context("reading downloaded metadata")?;
    let _ = std::fs::remove_file(&tmp);
    Ok(text)
}

fn which(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The newest published tag. Parsed out of the releases API without a JSON
/// dependency on the shape beyond `tag_name`.
fn latest_version() -> Result<String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo());
    let body = fetch_text(&url).context("asking GitHub for the latest release")?;
    let value: serde_json::Value =
        serde_json::from_str(&body).context("the releases API did not return JSON")?;
    value
        .get("tag_name")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("no release found for {}", repo()))
}

fn asset_url(version: &str, name: &str) -> String {
    match base_url() {
        Some(base) => format!("{}/{name}", base.trim_end_matches('/')),
        None => {
            format!("https://github.com/{}/releases/download/{version}/{name}", repo())
        }
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Compare `1.2.3`-shaped versions numerically, so 0.10.0 is newer than
/// 0.9.0. Anything unparseable falls back to "different means newer",
/// which is what a user asking for a specific tag wants anyway.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> {
        v.trim_start_matches('v')
            .split('-')
            .next()?
            .split('.')
            .map(|p| p.parse::<u64>().ok())
            .collect()
    };
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => candidate.trim_start_matches('v') != current.trim_start_matches('v'),
    }
}

pub fn run(check: bool, force: bool, want: Option<String>, json: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let target = target_triple();
    if target.is_empty() {
        bail!(
            "asm update has no release build for this platform; \
             build from source instead"
        );
    }

    let version = match want {
        Some(v) => v,
        None => latest_version()?,
    };
    let newer = is_newer(&version, current);

    if json {
        println!(
            "{}",
            serde_json::json!({
                "current": current,
                "latest": version,
                "update_available": newer,
                "target": target,
            })
        );
        if check {
            return Ok(());
        }
    } else if check {
        if newer {
            println!("asm {current} is installed; {version} is available");
            println!("Run `asm update` to install it.");
        } else {
            println!("asm {current} is the latest version.");
        }
        return Ok(());
    }

    if !newer && !force {
        if !json {
            println!("asm {current} is already the latest version.");
            println!("Use --force to reinstall it anyway.");
        }
        return Ok(());
    }

    // --- where the new binary has to land ---------------------------------
    let exe = std::env::current_exe().context("finding the running binary")?;
    // A symlink means the real file lives elsewhere; replacing the link
    // would leave the actual binary behind and confuse the next update.
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", exe.display()))?
        .to_path_buf();

    // A cargo build directory is writable, so the probe below would happily
    // let `cargo run -- update` replace a freshly built binary with a
    // release one. Recoverable, but confusing enough to be worth refusing.
    if is_cargo_build(&exe) && !force {
        bail!(
            "{} looks like a cargo build directory, not an installed asm.\n\
             Updating it would replace your build with a release binary. \
             Re-run with --force if that is what you meant.",
            exe.display()
        );
    }

    // Fail before downloading anything if the result could not be written.
    writable(&dir).with_context(|| {
        format!(
            "cannot write to {}. Install somewhere you own with \
             ASM_INSTALL_DIR=<dir> and the install script, or re-run with sudo",
            dir.display()
        )
    })?;

    // Staged in the destination directory: the last step is a rename, and
    // a rename cannot cross filesystems.
    let staging = dir.join(format!(".asm-update-{}", std::process::id()));
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("creating {}", staging.display()))?;
    let result = install(&version, target, &staging, &exe, current, json);
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// `…/target/debug/asm` or `…/target/release/asm` — where `cargo build`
/// puts things, and where nobody installs.
fn is_cargo_build(exe: &Path) -> bool {
    let mut parts = exe.components().rev().skip(1); // skip the file name
    let profile = parts.next().and_then(|c| c.as_os_str().to_str().map(str::to_string));
    let target = parts.next().and_then(|c| c.as_os_str().to_str().map(str::to_string));
    matches!(profile.as_deref(), Some("debug") | Some("release"))
        && target.as_deref() == Some("target")
}

/// Can we create a file here? Checked by doing it, because permission bits
/// do not account for read-only mounts or immutable flags.
fn writable(dir: &Path) -> Result<()> {
    let probe = dir.join(format!(".asm-write-probe-{}", std::process::id()));
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)?;
    Ok(())
}

fn install(
    version: &str,
    target: &str,
    staging: &Path,
    exe: &Path,
    current: &str,
    json: bool,
) -> Result<()> {
    let asset = format!("asm-{target}.tar.gz");
    let tarball = staging.join(&asset);
    if !json {
        println!("Updating asm {current} → {version} ({target})");
    }
    fetch(&asset_url(version, &asset), &tarball)?;

    // --- verify before anything is put in place ---------------------------
    let sums = staging.join("SHA256SUMS");
    match fetch(&asset_url(version, "SHA256SUMS"), &sums) {
        Ok(()) => {
            let text = std::fs::read_to_string(&sums).context("reading SHA256SUMS")?;
            let expected = text
                .lines()
                .find_map(|line| {
                    let (hash, name) = line.split_once("  ")?;
                    (name.trim() == asset).then(|| hash.trim().to_string())
                })
                .ok_or_else(|| anyhow!("{asset} is not listed in this release's SHA256SUMS"))?;
            let actual = sha256_file(&tarball)?;
            if actual != expected {
                bail!(
                    "checksum mismatch for {asset}; nothing was installed\n  \
                     expected {expected}\n  actual   {actual}"
                );
            }
            if !json {
                println!("Checksum ok.");
            }
        }
        // A release without checksums is not a reason to install
        // something unverified over a working binary.
        Err(e) => bail!("could not fetch SHA256SUMS ({e}); refusing to install unverified"),
    }

    // --- unpack -----------------------------------------------------------
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(staging)
        .status()
        .context("running tar")?;
    if !status.success() {
        bail!("could not unpack {}", tarball.display());
    }
    let fresh = staging.join(format!("asm-{target}")).join("asm");
    if !fresh.is_file() {
        bail!("the archive did not contain the expected binary");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755))
            .context("making the new binary executable")?;
    }

    // Renaming over the running binary is fine on Unix: this process keeps
    // the old inode open until it exits.
    std::fs::rename(&fresh, exe)
        .with_context(|| format!("replacing {}", exe.display()))?;

    if json {
        println!("{}", serde_json::json!({ "updated_to": version, "path": exe }));
    } else {
        println!("Updated to {version}: {}", exe.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    use super::is_cargo_build;
    use std::path::Path;

    #[test]
    fn a_cargo_build_directory_is_recognised() {
        assert!(is_cargo_build(Path::new("/home/dev/proj/target/debug/asm")));
        assert!(is_cargo_build(Path::new("/home/dev/proj/target/release/asm")));
    }

    #[test]
    fn an_installed_binary_is_not_mistaken_for_a_build() {
        assert!(!is_cargo_build(Path::new("/home/dev/.local/bin/asm")));
        assert!(!is_cargo_build(Path::new("/usr/local/bin/asm")));
        // A directory that merely ends in "debug" is not a cargo layout.
        assert!(!is_cargo_build(Path::new("/opt/debug/asm")));
    }

    #[test]
    fn versions_compare_numerically_not_as_text() {
        // The reason this is not a string compare: "0.10.0" < "0.9.0".
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.9.0", "0.10.0"));
    }

    #[test]
    fn a_leading_v_is_not_part_of_the_number() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
    }

    #[test]
    fn the_same_version_is_not_an_update() {
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "v0.1.0"));
    }

    #[test]
    fn an_older_release_is_not_offered_as_an_update() {
        assert!(!is_newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn a_prerelease_suffix_compares_on_the_numbers_before_it() {
        assert!(is_newer("0.2.0-rc1", "0.1.0"));
        assert!(!is_newer("0.1.0-rc1", "0.1.0"));
    }

    #[test]
    fn an_unparseable_tag_counts_as_different_rather_than_older() {
        // Asking for a named tag explicitly should still install it.
        assert!(is_newer("nightly", "0.1.0"));
        assert!(!is_newer("nightly", "nightly"));
    }
}
