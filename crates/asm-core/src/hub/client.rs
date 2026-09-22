//! A machine's side of the hub: its credential, and HTTP by way of curl.
//!
//! curl rather than a linked HTTP client, the same policy `asm update`
//! follows: it is already required, it speaks HTTPS through the system's
//! own trust store, and asm-core stays free of an async runtime and a TLS
//! stack. Four details make it safe to use this way:
//!
//! - `-q` first, so the user's `~/.curlrc` cannot add `-L`, a proxy, or an
//!   output file behind asm's back;
//! - the credential only ever on stdin (`-K -`), never in argv, where any
//!   other user on the machine could read it from the process list;
//! - `--noproxy '*'`, so an `http_proxy` in the environment does not quietly
//!   receive every transcript and the credential with them;
//! - never `-L`: following a redirect would replay the Authorization header
//!   to whatever host the redirect names.

use std::io::Write;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use super::manifest::Manifest;
use super::store::{Head, History, Joined, Machine, random_hex};
use crate::{CoreError, fsutil, paths};

#[derive(Serialize, Deserialize)]
struct RemoteFile {
    url: String,
    machine: Machine,
    credential: String,
}

pub struct Remote {
    pub url: String,
    pub machine: Machine,
    credential: String,
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

enum Body<'a> {
    None,
    Json(Vec<u8>),
    File(&'a Path),
}

#[derive(Clone, Copy)]
enum Profile {
    /// Small requests: bounded, and retried when the hub is briefly away.
    Control,
    /// Blob transfers reach hundreds of megabytes, so no wall-clock limit —
    /// only a floor on throughput, which is what a hung transfer violates.
    Transfer,
    /// Not safe to repeat: a join that succeeded but lost its response would
    /// otherwise register the machine twice.
    Once,
}

pub enum PutOutcome {
    Created { rev: String },
    /// The hub's head is not the revision this push was based on.
    Conflict { head: Option<String> },
}

fn invalid(msg: impl Into<String>) -> CoreError {
    CoreError::Invalid { msg: msg.into() }
}

fn config_path() -> Result<PathBuf, CoreError> {
    paths::data_dir()
        .map(|d| d.join("hub-client.json"))
        .ok_or_else(|| invalid("cannot determine asm data dir"))
}

/// A value curl's config syntax can carry inside double quotes without
/// escaping. Everything asm puts there — URLs it validated, hex tokens,
/// validated ids — already is; this is the guard for when that stops being
/// true.
fn config_safe(value: &str) -> Result<&str, CoreError> {
    if value.chars().any(|c| c == '"' || c == '\\' || c.is_control()) {
        return Err(invalid(format!("refusing to pass {value:?} to curl")));
    }
    Ok(value)
}

/// `http://host:port` with no path, userinfo, quote or whitespace.
fn normalize_url(url: &str) -> Result<String, CoreError> {
    let url = url.trim().trim_end_matches('/');
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| invalid("the hub URL must start with http:// or https://"))?;
    if rest.is_empty() || rest.contains('@') || rest.contains(char::is_whitespace) {
        return Err(invalid(format!("{url:?} is not a hub URL")));
    }
    config_safe(url)?;
    Ok(url.to_string())
}

fn host_and_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split('/').next()?;
    let default = if scheme == "https" { 443 } else { 80 };
    if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        let port = after.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(default);
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((authority.to_string(), default)),
    }
}

fn private_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                // 100.64.0.0/10, carrier-grade NAT — where Tailscale lives.
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        std::net::IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_loopback()
                || (s[0] & 0xfe00) == 0xfc00 // unique local
                || (s[0] & 0xffc0) == 0xfe80 // link local
        }
    }
}

/// Plain HTTP carries every transcript and the credential in the clear, so
/// it is only accepted to an address no one on the internet can sit between:
/// loopback, a private LAN range, or a VPN's.
fn plain_http_is_private(url: &str) -> bool {
    let Some((host, port)) = host_and_port(url) else { return false };
    match (host.as_str(), port).to_socket_addrs() {
        Ok(addrs) => {
            let addrs: Vec<_> = addrs.collect();
            !addrs.is_empty() && addrs.iter().all(|a| private_ip(a.ip()))
        }
        Err(_) => false,
    }
}

/// One request. The response body goes to `save_to` when given — a
/// download, written by curl straight to disk — and is otherwise read back
/// into the returned `Response`.
fn run_curl(
    url: &str,
    credential: Option<&str>,
    method: &str,
    body: Body,
    profile: Profile,
    save_to: Option<&Path>,
) -> Result<Response, CoreError> {
    let out = match save_to {
        Some(path) => path.to_path_buf(),
        None => paths::tmp_dir()?.join(format!("curl-{}.out", random_hex(8)?)),
    };
    let mut cmd = Command::new("curl");
    cmd.args(["-q", "-K", "-", "-sS", "--noproxy", "*", "--connect-timeout", "5"])
        .arg("-o")
        .arg(&out)
        .args(["-w", "%{http_code}"]);
    match profile {
        Profile::Control => {
            cmd.args(["--max-time", "60", "--retry", "3", "--retry-connrefused"]);
        }
        Profile::Transfer => {
            cmd.args(["--speed-limit", "1024", "--speed-time", "60", "--retry", "2"]);
        }
        Profile::Once => {
            cmd.args(["--max-time", "60"]);
        }
    }

    let mut config = format!("url = \"{}\"\n", config_safe(url)?);
    if let Some(credential) = credential {
        config.push_str(&format!("header = \"Authorization: Bearer {}\"\n", config_safe(credential)?));
    }
    let mut json_file_path = None;
    match body {
        Body::None => {
            cmd.args(["-X", method]);
        }
        Body::Json(bytes) => {
            let json_file = paths::tmp_dir()?.join(format!("curl-{}.json", random_hex(8)?));
            std::fs::write(&json_file, bytes).map_err(|e| CoreError::io(&json_file, e))?;
            cmd.args(["-X", method]).arg("--data-binary").arg(format!("@{}", json_file.display()));
            json_file_path = Some(json_file);
            config.push_str("header = \"Content-Type: application/json\"\n");
        }
        Body::File(path) => {
            // -T streams the file with a Content-Length and implies PUT.
            cmd.arg("-T").arg(path);
        }
    }

    let result = (|| {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| invalid(format!("could not run curl (it is required for hub sync): {e}")))?;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(config.as_bytes())
            .map_err(|e| invalid(format!("could not talk to curl: {e}")))?;
        let output = child.wait_with_output().map_err(|e| invalid(format!("curl failed: {e}")))?;
        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            // One line per retry; the last says it.
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = stderr.trim().lines().last().unwrap_or_default().to_string();
            // 55/56 during an upload is almost always the hub refusing it
            // before reading the body — a revoked credential or a size cap —
            // not a network blip worth retrying forever.
            let hint = if matches!(code, 55 | 56) && matches!(profile, Profile::Transfer) {
                " (the hub closed the connection mid-upload: check this machine is still \
                 joined, and the hub's size limit)"
            } else {
                ""
            };
            return Err(invalid(format!("could not reach the hub at {url}: {stderr}{hint}")));
        }
        let status: u16 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .map_err(|_| invalid("curl reported no HTTP status"))?;
        // A download that failed has an error body in the file; read it so
        // the caller can say why.
        let body = if save_to.is_none() || !(200..300).contains(&status) {
            std::fs::read(&out).unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(Response { status, body })
    })();
    if save_to.is_none() {
        let _ = std::fs::remove_file(&out);
    }
    if let Some(file) = json_file_path {
        let _ = std::fs::remove_file(file);
    }
    result
}

fn error_text(response: &Response) -> String {
    serde_json::from_slice::<serde_json::Value>(&response.body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_else(|| String::from_utf8_lossy(&response.body).chars().take(300).collect())
}

fn expect(response: Response, ok: &[u16], what: &str) -> Result<Response, CoreError> {
    if ok.contains(&response.status) {
        return Ok(response);
    }
    let detail = error_text(&response);
    Err(match response.status {
        401 => invalid(format!(
            "{what}: the hub does not accept this machine's credential ({detail}); \
             it may have been revoked — `asm join` again"
        )),
        status => invalid(format!("{what}: hub answered {status}: {detail}")),
    })
}

fn parse<T: serde::de::DeserializeOwned>(response: &Response, what: &str) -> Result<T, CoreError> {
    serde_json::from_slice(&response.body)
        .map_err(|e| invalid(format!("{what}: the hub's reply was not understood: {e}")))
}

/// Exchange a join token for this machine's own credential, and remember it.
pub fn join(url: &str, token: &str, name: &str, insecure_http: bool) -> Result<Remote, CoreError> {
    let url = normalize_url(url)?;
    if url.starts_with("http://") && !insecure_http && !plain_http_is_private(&url) {
        return Err(invalid(format!(
            "{url} is plain HTTP to an address outside loopback, a private LAN or a VPN, so \
             every transcript would cross the network readable by anyone on the path. Put the \
             hub behind HTTPS (a reverse proxy, `tailscale serve`), or pass --insecure-http if \
             you accept that"
        )));
    }
    let body = serde_json::to_vec(&serde_json::json!({ "token": token, "name": name })).unwrap();
    let response =
        run_curl(&format!("{url}/hub/v1/join"), None, "POST", Body::Json(body), Profile::Once, None)?;
    if response.status == 401 {
        return Err(invalid(
            "the hub did not accept that join token; `asm hub token` on the hub prints the \
             current one",
        ));
    }
    let response = expect(response, &[201], "joining the hub")?;
    let joined: Joined = parse(&response, "joining the hub")?;
    let remote = Remote { url, machine: joined.machine, credential: joined.credential };
    remote.save()?;
    Ok(remote)
}

pub fn load() -> Result<Remote, CoreError> {
    let path = config_path()?;
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(invalid("this machine has not joined a hub; run `ASM_JOIN_TOKEN=<token> asm join <url>`"));
        }
        Err(e) => return Err(CoreError::io(&path, e)),
    };
    let file: RemoteFile = serde_json::from_slice(&bytes)
        .map_err(|e| invalid(format!("{} is unreadable: {e}", path.display())))?;
    Ok(Remote { url: file.url, machine: file.machine, credential: file.credential })
}

impl Remote {
    fn save(&self) -> Result<(), CoreError> {
        let path = config_path()?;
        let body = serde_json::to_vec_pretty(&RemoteFile {
            url: self.url.clone(),
            machine: self.machine.clone(),
            credential: self.credential.clone(),
        })
        .unwrap();
        // write_atomic's file is 0600 from the moment it exists.
        fsutil::write_atomic(&path, &body)
    }

    fn call(&self, method: &str, path: &str, body: Body, profile: Profile) -> Result<Response, CoreError> {
        run_curl(&format!("{}{path}", self.url), Some(&self.credential), method, body, profile, None)
    }

    pub fn machines(&self) -> Result<Vec<Machine>, CoreError> {
        let r = expect(self.call("GET", "/hub/v1/machines", Body::None, Profile::Control)?, &[200], "listing machines")?;
        parse(&r, "listing machines")
    }

    pub fn heads(&self) -> Result<Vec<Head>, CoreError> {
        let r = expect(self.call("GET", "/hub/v1/sessions", Body::None, Profile::Control)?, &[200], "listing sessions")?;
        parse(&r, "listing sessions")
    }

    pub fn history(&self, agent: &str, id: &str) -> Result<Option<History>, CoreError> {
        let r = self.call("GET", &format!("/hub/v1/sessions/{agent}/{id}"), Body::None, Profile::Control)?;
        if r.status == 404 {
            return Ok(None);
        }
        let r = expect(r, &[200], "reading a session")?;
        Ok(Some(parse(&r, "reading a session")?))
    }

    pub fn revision(&self, agent: &str, id: &str, rev: &str) -> Result<Option<Manifest>, CoreError> {
        let r = self.call("GET", &format!("/hub/v1/sessions/{agent}/{id}/revisions/{rev}"), Body::None, Profile::Control)?;
        if r.status == 404 {
            return Ok(None);
        }
        let r = expect(r, &[200], "reading a revision")?;
        Ok(Some(parse(&r, "reading a revision")?))
    }

    pub fn missing(&self, shas: &[String]) -> Result<Vec<String>, CoreError> {
        if shas.is_empty() {
            return Ok(Vec::new());
        }
        let body = serde_json::to_vec(&serde_json::json!({ "shas": shas })).unwrap();
        let r = expect(
            self.call("POST", "/hub/v1/missing", Body::Json(body), Profile::Control)?,
            &[200],
            "checking uploads",
        )?;
        let v: serde_json::Value = parse(&r, "checking uploads")?;
        Ok(v.get("missing")
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_default())
    }

    pub fn put_blob(&self, sha: &str, file: &Path) -> Result<(), CoreError> {
        let r = self.call("PUT", &format!("/hub/v1/blobs/{sha}"), Body::File(file), Profile::Transfer)?;
        expect(r, &[200, 201], "uploading a file").map(|_| ())
    }

    pub fn get_blob(&self, sha: &str, dest: &Path) -> Result<(), CoreError> {
        let parent = dest.parent().unwrap_or(Path::new("."));
        let partial = parent.join(format!(".{sha}.part"));
        let result = (|| {
            let r = run_curl(
                &format!("{}/hub/v1/blobs/{sha}", self.url),
                Some(&self.credential),
                "GET",
                Body::None,
                Profile::Transfer,
                Some(&partial),
            )?;
            expect(r, &[200], "downloading a file")?;
            // Checked here too: the hub verified it on the way in, and this
            // is what makes a corrupted or substituted blob harmless on the
            // way out. Hashed from disk, in chunks.
            if fsutil::sha256_file(&partial)? != sha {
                return Err(invalid(format!("downloaded file {sha} does not match its hash")));
            }
            std::fs::rename(&partial, dest).map_err(|e| CoreError::io(dest, e))
        })();
        let _ = std::fs::remove_file(&partial);
        result
    }

    pub fn put_revision(&self, manifest: &Manifest) -> Result<PutOutcome, CoreError> {
        let path = format!("/hub/v1/sessions/{}/{}", manifest.agent, manifest.id);
        let body = serde_json::to_vec(manifest).unwrap();
        let r = self.call("PUT", &path, Body::Json(body), Profile::Control)?;
        match r.status {
            201 => {
                let v: serde_json::Value = parse(&r, "pushing")?;
                let rev = v.get("rev").and_then(|r| r.as_str()).unwrap_or_default().to_string();
                Ok(PutOutcome::Created { rev })
            }
            409 => {
                let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap_or_default();
                let head = v.get("head").and_then(|h| h.as_str()).map(String::from);
                // A retried request whose first attempt landed: the head is
                // this very push. That is success, not a conflict.
                if let Some(head) = &head
                    && let Some(history) = self.history(manifest.agent.as_str(), &manifest.id)?
                    && history.head == *head
                    && history.manifest.canonical == manifest.canonical
                    && history.manifest.machine.as_ref().map(|m| &m.id) == Some(&self.machine.id)
                    && history.manifest.parent_rev == manifest.parent_rev
                {
                    return Ok(PutOutcome::Created { rev: head.clone() });
                }
                Ok(PutOutcome::Conflict { head })
            }
            _ => expect(r, &[201], "pushing").map(|_| unreachable!()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_urls_are_normalized_and_nothing_else_is_accepted() {
        assert_eq!(normalize_url("http://hub.local:7434/").unwrap(), "http://hub.local:7434");
        assert!(normalize_url("ftp://hub").is_err());
        assert!(normalize_url("http://user:pw@hub").is_err(), "userinfo would leak into argv logs");
        assert!(normalize_url("http://hub\" -o /etc/x").is_err());
        assert!(normalize_url("http://").is_err());
    }

    #[test]
    fn host_and_port_parse_every_form() {
        assert_eq!(host_and_port("http://a:7434"), Some(("a".into(), 7434)));
        assert_eq!(host_and_port("https://a"), Some(("a".into(), 443)));
        assert_eq!(host_and_port("http://[::1]:8"), Some(("::1".into(), 8)));
    }

    /// Plain HTTP only where nobody on the internet sits in between.
    #[test]
    fn plain_http_is_accepted_only_to_private_addresses() {
        for ok in ["http://127.0.0.1:7434", "http://10.1.2.3", "http://192.168.1.9",
                   "http://172.20.0.1", "http://100.101.102.103", "http://[::1]:7434"] {
            assert!(plain_http_is_private(ok), "{ok}");
        }
        for bad in ["http://8.8.8.8", "http://100.200.0.1", "http://172.40.0.1"] {
            assert!(!plain_http_is_private(bad), "{bad}");
        }
    }
}
