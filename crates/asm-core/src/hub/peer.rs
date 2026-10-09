//! Moving one session straight between two machines, with no hub between.
//!
//! A hub is the answer for many machines, a laptop behind a NAT or a box
//! that sleeps: only the hub has to be reachable. Two machines that already
//! reach each other can skip it — over SSH (`ssh host asm hub receive`), or
//! over HTTP when the other one runs `asm hub serve --peer`. What crosses is
//! the bundle a push would upload (`manifest.json` + `blobs/<sha256>`),
//! packed into one tar stream by the system `tar`, the same shell-out policy
//! `client` follows for curl. The receiving end installs it the way a pull
//! does, comparing the two copies on the spot: identical, one extends the
//! other, or diverged, which is refused. No revisions and no sync record: a
//! peer transfer has no history, only the two copies in front of it.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use super::actions::{Pulled, Scratch, manifest_for, origin_of, scratch_dir};
use super::bundle::{self, Base, Installed, Source};
use super::client::{self, Body, Profile, Remote, Response, expect, parse, run_curl};
use super::manifest::{Manifest, valid_sha};
use super::store::Machine;
use crate::model::{AgentKind, Session};
use crate::{CoreError, fsutil, paths};

fn invalid(msg: impl Into<String>) -> CoreError {
    CoreError::Invalid { msg: msg.into() }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub name: String,
    #[serde(flatten)]
    pub transport: Transport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum Transport {
    /// `ssh host asm …`: whoever has a shell there is trusted, and asm must
    /// be on the PATH of a non-interactive shell.
    Ssh { host: String },
    /// `asm hub serve --peer` on the other machine, joined like a hub. The
    /// credential is this machine's there.
    Http { url: String, machine: Machine, credential: String },
}

impl Peer {
    /// Where the peer is, for a listing. Never the credential.
    pub fn address(&self) -> String {
        match &self.transport {
            Transport::Ssh { host } => format!("ssh://{host}"),
            Transport::Http { url, .. } => url.clone(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct PeersFile {
    peers: Vec<Peer>,
}

fn peers_path() -> Result<PathBuf, CoreError> {
    paths::data_dir().map(|d| d.join("peers.json")).ok_or_else(|| invalid("cannot determine asm data dir"))
}

fn load_peers() -> Result<PeersFile, CoreError> {
    let path = peers_path()?;
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| invalid(format!("{} is unreadable: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PeersFile::default()),
        Err(e) => Err(CoreError::io(&path, e)),
    }
}

fn save_peers(file: &PeersFile) -> Result<(), CoreError> {
    let path = peers_path()?;
    // An ssh peer may be the first thing ever written to the data dir.
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| CoreError::io(dir, e))?;
    }
    // write_atomic's file is 0600 from the moment it exists: an HTTP peer's
    // credential is in here.
    fsutil::write_atomic(&path, &serde_json::to_vec_pretty(file).unwrap())
}

pub fn list() -> Result<Vec<Peer>, CoreError> {
    Ok(load_peers()?.peers)
}

pub fn get(name: &str) -> Result<Peer, CoreError> {
    list()?
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| invalid(format!("no peer named {name:?}; `asm peer list` shows them, `asm peer add` adds one")))
}

/// A name that reads well in a sentence and cannot be mistaken for an
/// option: letters, digits, `._-`.
fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.starts_with('-')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Something ssh takes as a destination and nothing else: `[user@]host`,
/// never an option (`-oProxyCommand=…` would run a command here).
fn check_host(host: &str) -> Result<(), CoreError> {
    if host.is_empty()
        || host.starts_with('-')
        || host.contains('/')
        || host.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\'' || c == '"')
    {
        return Err(invalid(format!("{host:?} is not an ssh destination; use ssh://[user@]host")));
    }
    Ok(())
}

/// Remember a peer. `ssh://[user@]host` needs nothing else; `http(s)://…`
/// exchanges the peer's join token for a credential, like `asm join`.
pub fn add(
    name: &str,
    address: &str,
    token: Option<&str>,
    machine_name: &str,
    insecure_http: bool,
) -> Result<Peer, CoreError> {
    if !valid_name(name) {
        return Err(invalid(format!("{name:?} is not a peer name; use letters, digits, `.`, `_` and `-`")));
    }
    let mut file = load_peers()?;
    if file.peers.iter().any(|p| p.name == name) {
        return Err(invalid(format!("a peer named {name:?} exists; `asm peer remove {name}` first")));
    }
    let transport = if let Some(host) = address.strip_prefix("ssh://") {
        let host = host.trim_end_matches('/');
        check_host(host)?;
        Transport::Ssh { host: host.to_string() }
    } else if address.starts_with("http://") || address.starts_with("https://") {
        // A hub holds one credential per machine, and a join replaces it:
        // joining the hub this machine already joined would lock the hub
        // side out. Its credential serves both.
        let remote = match joined_hub(address.trim().trim_end_matches('/')) {
            Some(hub) => hub,
            None => {
                let token = token.ok_or_else(|| {
                    invalid(
                        "an HTTP peer needs its join token: set ASM_JOIN_TOKEN to what `asm hub token` \
                         prints there, or pass --token -",
                    )
                })?;
                client::exchange(address, token, machine_name, insecure_http)?
            }
        };
        Transport::Http { url: remote.url, machine: remote.machine, credential: remote.credential }
    } else {
        return Err(invalid(format!(
            "{address:?} is not a peer address; use ssh://[user@]host, or the http(s):// URL of an \
             `asm hub serve --peer`"
        )));
    };
    let peer = Peer { name: name.to_string(), transport };
    file.peers.push(peer.clone());
    save_peers(&file)?;
    Ok(peer)
}

pub fn remove(name: &str) -> Result<Peer, CoreError> {
    let mut file = load_peers()?;
    let at = file
        .peers
        .iter()
        .position(|p| p.name == name)
        .ok_or_else(|| invalid(format!("no peer named {name:?}")))?;
    let removed = file.peers.remove(at);
    save_peers(&file)?;
    Ok(removed)
}

/// The per-agent install, as a pull from a hub and a transfer from a peer
/// both do it: one copy of the dispatch.
pub fn install_bundle(
    manifest: &Manifest,
    blob: &dyn Fn(&str) -> Result<PathBuf, CoreError>,
    project_dir: Option<&Path>,
    base: Option<&Base>,
) -> Result<Installed, CoreError> {
    match manifest.agent {
        AgentKind::ClaudeCode => {
            let adapter = crate::adapter::claude::ClaudeAdapter::default_store()
                .ok_or_else(|| invalid("cannot locate the Claude Code store"))?;
            crate::adapter::claude::hub::install(&adapter, manifest, blob, project_dir, base)
        }
        AgentKind::Antigravity => {
            let adapter = crate::adapter::antigravity::AntigravityAdapter::default_store()
                .ok_or_else(|| invalid("cannot locate the Antigravity store"))?;
            crate::adapter::antigravity::hub::install(&adapter, manifest, blob, project_dir, base)
        }
        AgentKind::Codex => {
            let adapter = crate::adapter::codex::CodexAdapter::default_store()
                .ok_or_else(|| invalid("cannot locate the Codex store"))?;
            crate::adapter::codex::hub::install(&adapter, manifest, blob, project_dir, base)
        }
        AgentKind::JCode => {
            let adapter = crate::adapter::jcode::JCodeAdapter::default_store()
                .ok_or_else(|| invalid("cannot locate the jcode store"))?;
            crate::adapter::jcode::hub::install(&adapter, manifest, blob, project_dir, base)
        }
        AgentKind::OpenCode => {
            let adapter = crate::adapter::opencode::OpenCodeAdapter::default_store()
                .ok_or_else(|| invalid("cannot locate the OpenCode store"))?;
            crate::adapter::opencode::hub::install(&adapter, manifest, blob, project_dir, base)
        }
    }
}

fn tar(args: &[&str], what: &str) -> Result<(), CoreError> {
    let output = Command::new("tar")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| invalid(format!("could not run tar (it is required for peer transfers): {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(invalid(format!("{what}: {}", stderr.trim().lines().last().unwrap_or("tar failed"))));
    }
    Ok(())
}

/// A session packed for a peer: `bundle/` and the tar of it, under one
/// scratch directory that goes when this is dropped.
#[derive(Debug)]
pub struct Packed {
    _scratch: Scratch,
    pub tar: PathBuf,
    pub manifest: Manifest,
}

pub fn pack(session: &Session) -> Result<Packed, CoreError> {
    let mut bundle = bundle::collect(session)?;
    let scratch = Scratch(scratch_dir("peer")?);
    let dir = scratch.0.join("bundle");
    let blobs = dir.join("blobs");
    std::fs::create_dir_all(&blobs).map_err(|e| CoreError::io(&blobs, e))?;
    for (n, file) in bundle.files.iter_mut().enumerate() {
        // Copied, then the copy hashed: a live session's sidecar can grow
        // between collecting and packing, and the manifest must name what
        // is in `blobs/`.
        let tmp = blobs.join(format!(".{n}"));
        match &file.source {
            Source::Path(path) => {
                std::fs::copy(path, &tmp).map_err(|e| CoreError::io(path, e))?;
                file.entry.size = std::fs::metadata(&tmp).map_err(|e| CoreError::io(&tmp, e))?.len();
                file.entry.sha256 = Some(fsutil::sha256_file(&tmp)?);
            }
            Source::Bytes(bytes) => std::fs::write(&tmp, bytes).map_err(|e| CoreError::io(&tmp, e))?,
            Source::Symlink => continue,
        }
        let Some(sha) = &file.entry.sha256 else { continue };
        let dest = blobs.join(sha);
        if dest.is_file() {
            let _ = std::fs::remove_file(&tmp);
        } else {
            std::fs::rename(&tmp, &dest).map_err(|e| CoreError::io(&dest, e))?;
        }
    }
    let origin = origin_of(&session.project_root, &mut HashMap::new());
    let manifest = manifest_for(session, &bundle, None, origin);
    let path = dir.join("manifest.json");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).map_err(|e| CoreError::io(&path, e))?;
    let tar_path = scratch.0.join("bundle.tar");
    tar(&["-cf", &tar_path.display().to_string(), "-C", &dir.display().to_string(), "."], "could not pack the session")?;
    Ok(Packed { _scratch: scratch, tar: tar_path, manifest })
}

/// A bundle from a peer, unpacked and checked, ready to install.
#[derive(Debug)]
pub struct Unpacked {
    _scratch: Scratch,
    blobs: PathBuf,
    pub manifest: Manifest,
}

impl Unpacked {
    fn blob(&self, sha: &str) -> Result<PathBuf, CoreError> {
        let path = self.blobs.join(sha);
        if !path.is_file() {
            return Err(invalid(format!("the bundle is missing file {sha}")));
        }
        Ok(path)
    }

    /// Install onto this machine, comparing with any copy already here.
    pub fn install(&self, project_dir: Option<&Path>) -> Result<Installed, CoreError> {
        install_bundle(&self.manifest, &|sha| self.blob(sha), project_dir, None)
    }
}

/// Unpack a tar a peer sent. Nothing tar did is trusted: only
/// `manifest.json` and regular files at `blobs/<sha256>` whose content
/// hashes to their name are accepted, the manifest must validate, and every
/// file it names must be there. Anything else refuses the whole bundle.
pub fn unpack(tar_path: &Path) -> Result<Unpacked, CoreError> {
    vet_members(tar_path)?;
    let scratch = Scratch(scratch_dir("peer")?);
    let dir = scratch.0.join("bundle");
    std::fs::create_dir_all(&dir).map_err(|e| CoreError::io(&dir, e))?;
    tar(&["-xf", &tar_path.display().to_string(), "-C", &dir.display().to_string()], "the peer's bundle could not be unpacked")?;
    check_layout(&dir)?;
    let path = dir.join("manifest.json");
    let bytes = std::fs::read(&path).map_err(|_| invalid("the bundle has no manifest.json"))?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| invalid(format!("the peer's manifest is unreadable: {e}")))?;
    manifest.validate().map_err(|msg| invalid(format!("the peer's manifest was refused: {msg}")))?;
    let unpacked = Unpacked { _scratch: scratch, blobs: dir.join("blobs"), manifest };
    for sha in unpacked.manifest.blob_shas() {
        unpacked.blob(sha)?;
    }
    Ok(unpacked)
}

/// Run tar and return what it printed.
fn tar_lines(args: &[&str], what: &str) -> Result<Vec<String>, CoreError> {
    let output = Command::new("tar")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| invalid(format!("could not run tar (it is required for peer transfers): {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(invalid(format!("{what}: {}", stderr.trim().lines().last().unwrap_or("tar failed"))));
    }
    Ok(String::from_utf8_lossy(&output.stdout).lines().map(String::from).collect())
}

/// Before anything is extracted: every member must be the manifest, the
/// blobs directory or a regular file named by a sha256 inside it. A `../` or
/// absolute name, a symlink or a hardlink (which a later member could write
/// through) refuses the bundle, so safety does not depend on whether this
/// machine's tar strips such names on its own. A name with a newline in it
/// splits into lines that fail the same check or misalign the two listings.
fn vet_members(tar_path: &Path) -> Result<(), CoreError> {
    let file = tar_path.display().to_string();
    let what = "the peer's bundle could not be unpacked";
    let names = tar_lines(&["-tf", &file], what)?;
    let kinds = tar_lines(&["-tvf", &file], what)?;
    let refuse = |name: &str| {
        let shown = name.strip_prefix("./").unwrap_or(name);
        invalid(format!("the bundle contains {shown:?}, which is not part of a session bundle"))
    };
    if names.len() != kinds.len() {
        return Err(invalid("the bundle has a member name asm cannot read safely"));
    }
    for (name, kind) in names.iter().zip(&kinds) {
        if !matches!(kind.chars().next(), Some('-' | 'd')) {
            return Err(refuse(name));
        }
        let plain = name.strip_prefix("./").unwrap_or(name).trim_end_matches('/');
        let allowed = match plain {
            "" | "." | "manifest.json" | "blobs" => true,
            other => other.strip_prefix("blobs/").is_some_and(valid_sha),
        };
        if !allowed {
            return Err(refuse(name));
        }
    }
    Ok(())
}

/// What came out of tar, checked again on disk: see `unpack`.
fn check_layout(dir: &Path) -> Result<(), CoreError> {
    let refuse = |path: &Path| {
        invalid(format!(
            "the bundle contains {:?}, which is not part of a session bundle",
            path.strip_prefix(dir).unwrap_or(path).display().to_string()
        ))
    };
    let entries = |d: &Path| -> Result<Vec<PathBuf>, CoreError> {
        Ok(std::fs::read_dir(d).map_err(|e| CoreError::io(d, e))?.flatten().map(|e| e.path()).collect())
    };
    for path in entries(dir)? {
        let meta = std::fs::symlink_metadata(&path).map_err(|e| CoreError::io(&path, e))?;
        match path.file_name().and_then(|n| n.to_str()) {
            Some("manifest.json") if meta.is_file() => {}
            Some("blobs") if meta.is_dir() => {
                for blob in entries(&path)? {
                    let meta = std::fs::symlink_metadata(&blob).map_err(|e| CoreError::io(&blob, e))?;
                    let name = blob.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                    if !meta.is_file() || !valid_sha(name) {
                        return Err(refuse(&blob));
                    }
                    if fsutil::sha256_file(&blob)? != name {
                        return Err(invalid(format!("file {name} in the bundle does not match its hash")));
                    }
                }
            }
            _ => return Err(refuse(&path)),
        }
    }
    Ok(())
}

/// The receiving end: a tar on `reader`, at most `limit` bytes, installed.
/// `asm hub receive` reads stdin this way; the `--peer` route, its body.
pub fn receive(reader: &mut dyn Read, limit: u64, project_dir: Option<&Path>) -> Result<Installed, CoreError> {
    let scratch = Scratch(scratch_dir("peer")?);
    let tar_path = scratch.0.join("bundle.tar");
    let mut file = std::fs::File::create(&tar_path).map_err(|e| CoreError::io(&tar_path, e))?;
    let written = std::io::copy(&mut reader.take(limit.saturating_add(1)), &mut file).map_err(|e| CoreError::io(&tar_path, e))?;
    if written > limit {
        return Err(invalid(format!("the bundle is larger than the {} this machine accepts", crate::fmt::human_bytes(limit))));
    }
    unpack(&tar_path)?.install(project_dir)
}

/// A word in a shell line, as ssh's remote shell will read it.
fn shell_quote(word: &str) -> String {
    if !word.is_empty() && word.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./:=@".contains(&b)) {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// Run `asm …` on the peer over ssh. The tar goes on stdin when sending;
/// stdout goes to a file when fetching, and is returned otherwise.
fn ssh(host: &str, args: &[&str], stdin: Option<&Path>, stdout: Option<&Path>) -> Result<Vec<u8>, CoreError> {
    check_host(host)?;
    let line = std::iter::once("asm").chain(args.iter().copied()).map(shell_quote).collect::<Vec<_>>().join(" ");
    let mut cmd = Command::new("ssh");
    // Options before the destination, so the destination is never read as
    // one; BatchMode, so a missing key or an unknown host fails instead of
    // waiting on a prompt nobody sees.
    cmd.args(["-o", "BatchMode=yes", host, &line]);
    cmd.stdin(match stdin {
        Some(path) => Stdio::from(std::fs::File::open(path).map_err(|e| CoreError::io(path, e))?),
        None => Stdio::null(),
    });
    cmd.stdout(match stdout {
        Some(path) => Stdio::from(std::fs::File::create(path).map_err(|e| CoreError::io(path, e))?),
        None => Stdio::piped(),
    });
    cmd.stderr(Stdio::piped());
    let output =
        cmd.output().map_err(|e| invalid(format!("could not run ssh (it is required for an ssh peer): {e}")))?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    // 255 is ssh's own failure; anything else came from the remote shell or
    // from asm there.
    Err(if output.status.code() == Some(255) {
        invalid(format!("could not reach {host} over ssh: {stderr}"))
    } else if stderr.contains("not found") {
        invalid(format!(
            "asm is not on the PATH of a non-interactive shell on {host} ({stderr}); put it where \
             that shell looks, such as /usr/local/bin"
        ))
    } else {
        invalid(format!("asm on {host} failed: {stderr}"))
    })
}

/// The hub this machine joined, when it is at `url`.
fn joined_hub(url: &str) -> Option<Remote> {
    client::load().ok().filter(|r| r.url == url)
}

/// The peer as a `Remote`. When the peer is also this machine's hub, the
/// hub's credential is the live one: an `asm join` since would have replaced
/// what `peers.json` holds.
fn remote(url: &str, machine: &Machine, credential: &str) -> Remote {
    joined_hub(url).unwrap_or_else(|| Remote {
        url: url.to_string(),
        machine: machine.clone(),
        credential: credential.to_string(),
        quick: false,
    })
}

/// `expect`, with the one answer that means something else for a peer: a
/// hub without `--peer` has no peer routes at all.
fn expect_peer(response: Response, ok: &[u16], what: &str, url: &str) -> Result<Response, CoreError> {
    if response.status == 404 {
        return Err(invalid(format!(
            "{what}: `asm hub serve` at {url} is not running with --peer, so it takes no peer transfers"
        )));
    }
    expect(response, ok, what)
}

/// A path in a query string.
fn percent(path: &Path) -> String {
    path.as_os_str()
        .as_encoded_bytes()
        .iter()
        .map(|&b| {
            if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }
        })
        .collect()
}

/// What the peer has, as `asm list --json` there would say.
pub fn sessions(peer: &Peer) -> Result<Vec<Session>, CoreError> {
    match &peer.transport {
        Transport::Ssh { host } => {
            let out = ssh(host, &["list", "--json"], None, None)?;
            serde_json::from_slice(&out)
                .map_err(|e| invalid(format!("the listing from {host} was not understood: {e}")))
        }
        Transport::Http { url, machine, credential } => {
            let r = remote(url, machine, credential);
            let path = format!("{url}/hub/v1/peer/sessions");
            let r = run_curl(&path, Some(&r.credential), "GET", Body::None, Profile::Control, None)?;
            parse(&expect_peer(r, &[200], "listing the peer's sessions", url)?, "listing the peer's sessions")
        }
    }
}

fn pulled(peer: &Peer, manifest: &Manifest, installed: Installed) -> Pulled {
    Pulled {
        agent: manifest.agent,
        id: manifest.id.clone(),
        title: manifest.title.clone(),
        slug: manifest.slug.clone(),
        from: Some(peer.name.clone()),
        installed,
    }
}

/// Send one session to the peer, where it is installed like a pull. The
/// outcome describes the peer's copy.
pub fn push_to(peer: &Peer, session: &Session, project_dir: Option<&Path>) -> Result<Pulled, CoreError> {
    let packed = pack(session)?;
    let dir = project_dir.map(|d| d.display().to_string());
    let installed = match &peer.transport {
        Transport::Ssh { host } => {
            let mut args = vec!["hub", "receive"];
            if let Some(dir) = &dir {
                args.extend(["--project-dir", dir]);
            }
            let out = ssh(host, &args, Some(&packed.tar), None)?;
            serde_json::from_slice(&out).map_err(|e| invalid(format!("asm on {host} answered something unexpected: {e}")))?
        }
        Transport::Http { url, machine, credential } => {
            let r = remote(url, machine, credential);
            let mut path = format!("{url}/hub/v1/peer/receive");
            if let Some(dir) = project_dir {
                path.push_str(&format!("?project_dir={}", percent(dir)));
            }
            let r = run_curl(&path, Some(&r.credential), "PUT", Body::File(&packed.tar), Profile::Transfer, None)?;
            parse(&expect_peer(r, &[200], "sending to the peer", url)?, "sending to the peer")?
        }
    };
    Ok(pulled(peer, &packed.manifest, installed))
}

/// Bring one of the peer's sessions here. `query` is resolved against the
/// peer's listing the way `asm list` refs are.
pub fn pull_from(peer: &Peer, query: &str, project_dir: Option<&Path>) -> Result<Pulled, CoreError> {
    let session = crate::ops::resolve_in(sessions(peer)?, query)
        .map_err(|e| invalid(format!("{e} on {}", peer.name)))?;
    let key = format!("{}:{}", session.handle.agent, session.handle.native_id);
    let scratch = Scratch(scratch_dir("peer")?);
    let tar_path = scratch.0.join("bundle.tar");
    match &peer.transport {
        Transport::Ssh { host } => {
            ssh(host, &["hub", "send", &key], None, Some(&tar_path))?;
        }
        Transport::Http { url, machine, credential } => {
            let r = remote(url, machine, credential);
            let path = format!("{url}/hub/v1/peer/send/{}/{}", session.handle.agent, session.handle.native_id);
            let r = run_curl(&path, Some(&r.credential), "GET", Body::None, Profile::Transfer, Some(&tar_path))?;
            expect_peer(r, &[200], "fetching from the peer", url)?;
        }
    }
    let unpacked = unpack(&tar_path)?;
    if unpacked.manifest.key() != key {
        return Err(invalid(format!("{} sent {} instead of {key}", peer.name, unpacked.manifest.key())));
    }
    let installed = unpacked.install(project_dir)?;
    Ok(pulled(peer, &unpacked.manifest, installed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_line_is_quoted_for_the_peers_shell() {
        assert_eq!(shell_quote("claude-code:7f3a"), "claude-code:7f3a");
        assert_eq!(shell_quote("/home/a/my project"), "'/home/a/my project'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    /// An ssh destination is `[user@]host`; an option in its place would be
    /// obeyed by ssh, so it is refused before ssh sees it.
    #[test]
    fn hosts_are_destinations_not_options() {
        for ok in ["box", "me@box.local", "[::1]", "box:2222"] {
            assert!(check_host(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-oProxyCommand=evil", "box evil", "box/path", "a'b"] {
            assert!(check_host(bad).is_err(), "{bad:?}");
        }
        assert!(valid_name("desk-2") && valid_name("a.b_c"));
        assert!(!valid_name("-x") && !valid_name("a b") && !valid_name(""));
    }

    #[test]
    fn a_project_dir_survives_a_query_string() {
        assert_eq!(percent(Path::new("/home/a/my project")), "/home/a/my%20project");
        assert_eq!(percent(Path::new("/x/a&b=c")), "/x/a%26b%3Dc");
    }

    /// What came out of tar is a layout to check, not a bundle to trust.
    #[test]
    fn only_a_manifest_and_hashed_blobs_come_out_of_the_tar() {
        use std::fs;
        let sha = fsutil::sha256_hex(b"line\n");
        let fresh = || {
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir(dir.path().join("blobs")).unwrap();
            fs::write(dir.path().join("manifest.json"), b"{}").unwrap();
            fs::write(dir.path().join("blobs").join(&sha), b"line\n").unwrap();
            dir
        };
        assert!(check_layout(fresh().path()).is_ok());

        let d = fresh();
        fs::write(d.path().join("evil"), b"x").unwrap();
        assert!(check_layout(d.path()).unwrap_err().to_string().contains("\"evil\""));
        let d = fresh();
        fs::write(d.path().join("blobs").join(&sha), b"other\n").unwrap();
        assert!(check_layout(d.path()).unwrap_err().to_string().contains("does not match its hash"));
        let d = fresh();
        fs::write(d.path().join("blobs/notasha"), b"x").unwrap();
        assert!(check_layout(d.path()).is_err());
        let d = fresh();
        std::os::unix::fs::symlink("/etc/passwd", d.path().join("blobs").join("0".repeat(64))).unwrap();
        assert!(check_layout(d.path()).is_err(), "a symlink is never a blob");
        let d = fresh();
        fs::remove_file(d.path().join("manifest.json")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", d.path().join("manifest.json")).unwrap();
        assert!(check_layout(d.path()).is_err(), "nor a manifest");
    }
}
