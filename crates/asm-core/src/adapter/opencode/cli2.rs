//! Running the OpenCode 2.x CLI, the one sanctioned way to change its store.
//!
//! 2.x keeps sessions in tables that are projections of an event log, and a
//! background service may hold them open, so asm never writes those rows
//! itself: rename, delete and import go through `opencode`. What this module
//! pins down is how to run it without surprises (verified against 2.0.25):
//!
//! - With no flag the CLI talks to the service named in
//!   `$XDG_STATE_HOME/opencode/service.json`, and when that service is DOWN
//!   it starts a managed one whose boot resumes sessions an earlier service
//!   left claimed: a model call nobody asked for. So [`Cli::connect`] adds
//!   `--standalone` (a private server for one request, which leaves nothing
//!   behind) unless the service is PROVEN up: `service.json` names a live
//!   pid AND that service answers `GET /api/info` over loopback with the same
//!   pid (and version), which is what OpenCode's own probe requires. A pid
//!   alone proves nothing: a stale file whose pid another process now owns
//!   would make asm run the flagless command with the service down.
//!   Anything short of proof (no file, a dead pid, a closed port, another
//!   answer, a non-loopback or non-http url) is "not running".
//! - Only `service.json` is known. OpenCode names the file by release
//!   channel: `latest`, `dev`, `beta` and `next` use `service.json`, any other
//!   channel `service-<channel>.json`. A binary of such a channel registers
//!   where asm does not look, so asm sees no service and stands alone, which
//!   is the safe answer (it never starts a service, and never reuses one).
//!   There remains a window between the check and the call in which the
//!   service can die; it is the cost of letting the live service apply the
//!   change.
//! - `OPENCODE_DB` and `OPENCODE_TEST_HOME` are removed from the child's
//!   environment: they would make it write a database other than the one
//!   asm reads.
//! - `session import` of an id that exists prints `Session already exists`
//!   and exits 0, so callers check the store before and after.
//! - Nothing here sends a prompt to a model.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::CoreError;

/// A request is local and small; a minute is far beyond any real one.
const TIMEOUT: Duration = Duration::from_secs(60);

/// How long to wait for a service to accept a connection, and then to speak.
const CONNECT: Duration = Duration::from_millis(300);
const IO: Duration = Duration::from_millis(700);

/// The pid of the service `<state_dir>/service.json` names, when that
/// service is really up: the pid is alive and the url answers `/api/info`
/// with that pid (see the module doc).
pub(super) fn service_pid(state_dir: &Path) -> Option<u32> {
    let info: Value = serde_json::from_slice(&std::fs::read(state_dir.join("service.json")).ok()?).ok()?;
    let pid = u32::try_from(info.get("pid")?.as_u64()?).ok()?;
    (crate::process::alive(pid) && answers_as(&info, pid)).then_some(pid)
}

/// Does the service registered in `info` answer on its loopback url as `pid`?
fn answers_as(info: &Value, pid: u32) -> bool {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
    let Some(authority) = info.get("url").and_then(Value::as_str).and_then(|u| u.strip_prefix("http://")) else {
        return false;
    };
    let authority = authority.split('/').next().unwrap_or_default();
    let Some((host, port)) = authority.rsplit_once(':').and_then(|(h, p)| Some((h.trim_matches(['[', ']']), p.parse::<u16>().ok()?))) else {
        return false;
    };
    let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) if ip.is_unspecified() => {
            vec![SocketAddr::new(if ip.is_ipv4() { Ipv4Addr::LOCALHOST.into() } else { Ipv6Addr::LOCALHOST.into() }, port)]
        }
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => (host, port).to_socket_addrs().map(|a| a.collect()).unwrap_or_default(),
    };
    // Never a request (and never the service's password) to another machine.
    let password = info.get("password").and_then(Value::as_str);
    addrs.into_iter().filter(|a| a.ip().is_loopback()).any(|addr| {
        get_info(addr, authority, password).is_some_and(|served| {
            served.get("pid").and_then(Value::as_u64) == Some(u64::from(pid))
                && served.get("version").and_then(Value::as_str).is_some()
                && info.get("version").and_then(Value::as_str).is_none_or(|v| served.get("version").and_then(Value::as_str) == Some(v))
        })
    })
}

/// `GET /api/info` with std alone (asm-core has no HTTP client): the JSON
/// object of a `200` answer.
fn get_info(addr: std::net::SocketAddr, host: &str, password: Option<&str>) -> Option<Value> {
    use std::io::Write;
    let mut stream = std::net::TcpStream::connect_timeout(&addr, CONNECT).ok()?;
    stream.set_read_timeout(Some(IO)).ok()?;
    stream.set_write_timeout(Some(IO)).ok()?;
    let mut request = format!("GET /api/info HTTP/1.0\r\nHost: {host}\r\nAccept: application/json\r\nConnection: close\r\n");
    if let Some(password) = password {
        request.push_str(&format!("Authorization: Basic {}\r\n", base64(format!("opencode:{password}").as_bytes())));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    // Until the server closes (HTTP/1.0), the timeout, or enough.
    while buf.len() < 64 * 1024 {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text.split_once("\r\n\r\n")?;
    if head.split_whitespace().nth(1) != Some("200") {
        return None;
    }
    // A chunked body has size lines around the JSON; the object is between
    // the first brace and the last.
    let (start, end) = (body.find('{')?, body.rfind('}')?);
    serde_json::from_str(body.get(start..=end)?).ok()
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for group in bytes.chunks(3) {
        let n = group.iter().enumerate().fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=group.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
        out.extend(std::iter::repeat_n('=', 3 - group.len()));
    }
    out
}

/// How a command reaches OpenCode's server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Connect {
    /// The running background service, which `service.json` names.
    Service,
    /// A private server for this one request (`--standalone`).
    Standalone,
}

#[derive(Debug, Clone)]
pub(super) struct Cli {
    bin: OsString,
    /// `None`: the environment asm runs in. `Some`: exactly these variables
    /// and nothing else (tests, so a real home is never inherited).
    env: Option<Vec<(OsString, OsString)>>,
    /// `<XDG_STATE_HOME>/opencode`, where `service.json` lives. `None`
    /// cannot see a service, so it never talks to one.
    state_dir: Option<PathBuf>,
    /// Where commands run and import documents are written. `None`: asm's
    /// own tmp directory.
    scratch: Option<PathBuf>,
}

impl Cli {
    /// `opencode` from the PATH, in asm's own environment.
    pub(super) fn ambient(state_dir: Option<PathBuf>) -> Cli {
        Cli { bin: "opencode".into(), env: None, state_dir, scratch: None }
    }

    /// The `opencode` at `bin`, in asm's own environment.
    pub(super) fn ambient_at(bin: PathBuf, state_dir: Option<PathBuf>) -> Cli {
        Cli { bin: bin.into_os_string(), env: None, state_dir, scratch: None }
    }

    /// A binary run with only the variables given. `PATH` is passed through
    /// by the caller if the binary needs it.
    #[cfg(test)]
    pub(super) fn isolated(
        bin: impl Into<OsString>,
        env: Vec<(OsString, OsString)>,
        state_dir: impl Into<PathBuf>,
        scratch: impl Into<PathBuf>,
    ) -> Cli {
        Cli { bin: bin.into(), env: Some(env), state_dir: Some(state_dir.into()), scratch: Some(scratch.into()) }
    }

    pub(super) fn state_dir(&self) -> Option<&Path> {
        self.state_dir.as_deref()
    }

    pub(super) fn scratch(&self) -> Result<PathBuf, CoreError> {
        match &self.scratch {
            Some(dir) => Ok(dir.clone()),
            None => crate::paths::tmp_dir(),
        }
    }

    pub(super) fn connect(&self) -> Connect {
        match self.state_dir.as_deref().and_then(service_pid) {
            Some(_) => Connect::Service,
            None => Connect::Standalone,
        }
    }

    /// `opencode <args>`; `--standalone` is appended unless a live service
    /// is to be used. Returns stdout; a failure carries stderr.
    fn run(&self, args: &[&str], connect: bool) -> Result<String, CoreError> {
        let mut args: Vec<OsString> = args.iter().map(OsString::from).collect();
        if connect && self.connect() == Connect::Standalone {
            args.push("--standalone".into());
        }
        let what = args.iter().take(2).map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ");
        let out = self.spawn(&args, &what)?;
        if !out.status.success() {
            return Err(CoreError::Invalid {
                msg: format!("opencode {what} failed ({}): {}", out.status, out.detail()),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// The process to run for `args`: the environment fixed, nothing started.
    fn command(&self, args: &[OsString]) -> Result<Command, CoreError> {
        let cwd = self.scratch()?;
        let mut cmd = Command::new(&self.bin);
        if let Some(env) = &self.env {
            cmd.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
        } else {
            // They would point the child at a database other than the one
            // asm reads.
            cmd.env_remove("OPENCODE_DB").env_remove("OPENCODE_TEST_HOME");
        }
        cmd.args(args).current_dir(&cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        Ok(cmd)
    }

    fn spawn(&self, args: &[OsString], what: &str) -> Result<Output, CoreError> {
        let mut cmd = self.command(args)?;
        // `Text file busy`: the binary is being written (an update replacing
        // it) or a process forked meanwhile still holds the descriptor it was
        // written through, for a moment. Look again before giving up.
        let mut tries = 0;
        let mut child = loop {
            match cmd.spawn() {
                Ok(child) => break child,
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 50 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => {
                    return Err(CoreError::Invalid {
                        msg: format!("failed to run opencode ({}): {e}", self.bin.to_string_lossy()),
                    });
                }
            }
        };
        let drain = |pipe: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_end(&mut buf);
                }
                buf
            })
        };
        let stdout = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
        let stderr = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CoreError::Invalid {
                        msg: format!("opencode {what} did not finish within {}s and was stopped", TIMEOUT.as_secs()),
                    });
                }
                Err(e) => return Err(CoreError::Invalid { msg: format!("opencode {what}: {e}") }),
            }
        };
        Ok(Output { status, stdout: stdout.join().unwrap_or_default(), stderr: stderr.join().unwrap_or_default() })
    }

    /// `2.0.25` from `opencode --version` (`opencode v2.0.25`).
    pub(super) fn version(&self) -> Option<String> {
        let out = self.spawn(&["--version".into()], "--version").ok().filter(|o| o.status.success())?;
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .map(|t| t.trim_start_matches('v'))
            .find(|t| t.split('.').count() >= 2 && t.split('.').all(|p| p.chars().take(1).all(|c| c.is_ascii_digit())))
            .map(str::to_string)
    }

    /// Is the installed OpenCode a 2.x?
    pub(super) fn is_v2(&self) -> bool {
        self.version().is_some_and(|v| v.starts_with("2."))
    }

    /// `session.update` with a new title. An empty title would make
    /// OpenCode generate one with a model, so it is refused here too.
    pub(super) fn update_title(&self, id: &str, title: &str) -> Result<(), CoreError> {
        if title.trim().is_empty() {
            return Err(CoreError::Invalid { msg: "a session title cannot be empty".into() });
        }
        let body = serde_json::json!({ "title": title }).to_string();
        self.run(&["api", "session.update", "--param", &format!("sessionID={id}"), "-d", &body], true).map(drop)
    }

    /// Delete a session and, recursively, its children.
    pub(super) fn delete(&self, id: &str) -> Result<(), CoreError> {
        self.run(&["session", "delete", id], true).map(drop)
    }

    /// `opencode session export <id>` (tests compare it with the read-only exporter).
    #[cfg(test)]
    pub(super) fn export(&self, id: &str) -> Result<String, CoreError> {
        self.run(&["session", "export", id], true)
    }

    /// Import one transfer document. The parent of a child must be in the
    /// store already, `directory` must exist, and the id must not: the CLI
    /// answers an existing id with success, so the caller checks first.
    pub(super) fn import(&self, file: &Path, directory: &Path) -> Result<(), CoreError> {
        let (file, directory) = (file.to_string_lossy(), directory.to_string_lossy());
        self.run(&["session", "import", &file, "--directory", &directory], true).map(drop)
    }
}

struct Output {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Output {
    /// What to show of a failure: stderr, else stdout, bounded.
    fn detail(&self) -> String {
        let text = String::from_utf8_lossy(if self.stderr.is_empty() { &self.stdout } else { &self.stderr });
        let text = text.trim();
        match text.char_indices().nth(2000) {
            Some((i, _)) => format!("{}...", &text[..i]),
            None => text.to_string(),
        }
    }
}

/// Fixtures for the tests that run the real binary: a private data/state/
/// config/cache/home under a temp dir, `--standalone` only, no model.
#[cfg(test)]
pub(super) mod testenv {
    use super::*;

    pub(crate) struct Isolated {
        pub dir: tempfile::TempDir,
        pub cli: Cli,
        pub db: PathBuf,
    }

    impl Isolated {
        /// `None` (with a message) when there is no OpenCode 2.x to test with.
        pub(crate) fn new() -> Option<Isolated> {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            for sub in ["home", "data", "state", "config", "cache", "scratch"] {
                std::fs::create_dir_all(root.join(sub)).unwrap();
            }
            let var = |k: &str, v: PathBuf| (OsString::from(k), v.into_os_string());
            let mut env = vec![
                var("HOME", root.join("home")),
                var("XDG_DATA_HOME", root.join("data")),
                var("XDG_STATE_HOME", root.join("state")),
                var("XDG_CONFIG_HOME", root.join("config")),
                var("XDG_CACHE_HOME", root.join("cache")),
            ];
            // The binary needs sh/git on its PATH; it is the only thing inherited.
            env.push((OsString::from("PATH"), std::env::var_os("PATH").unwrap_or_default()));
            let bin = find_binary()?;
            let cli = Cli::isolated(bin, env, root.join("state/opencode"), root.join("scratch"));
            if !cli.is_v2() {
                eprintln!("skipped: no OpenCode 2.x binary (found {:?})", cli.version());
                return None;
            }
            let db = root.join("data/opencode/opencode.db");
            Some(Isolated { dir, cli, db })
        }

        pub(crate) fn path(&self, sub: &str) -> PathBuf {
            let p = self.dir.path().join(sub);
            std::fs::create_dir_all(&p).unwrap();
            p
        }

        pub(crate) fn adapter(&self) -> super::super::OpenCodeAdapter {
            super::super::OpenCodeAdapter::with_db(&self.db)
                .with_cli(self.cli.clone())
                .with_backup_root(self.dir.path().join("backups"))
        }
    }

    /// A loopback server that answers every request with `reply` (a raw HTTP
    /// response) and records the requests it saw.
    pub(crate) fn serve(reply: String) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut request = vec![0u8; 4096];
                let n = stream.read(&mut request).unwrap_or(0);
                log.lock().unwrap().push(String::from_utf8_lossy(&request[..n]).into_owned());
                let _ = std::io::Write::write_all(&mut stream, reply.as_bytes());
            }
        });
        (port, seen)
    }

    pub(crate) fn json_reply(body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}")
    }

    /// `service.json` in `dir` for a service at `port` claiming `pid`.
    pub(crate) fn register(dir: &Path, pid: u32, port: u16, extra: Value) {
        let mut info = serde_json::json!({ "id": "svc", "version": "2.0.25", "url": format!("http://127.0.0.1:{port}"), "pid": pid });
        info.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        std::fs::write(dir.join("service.json"), info.to_string()).unwrap();
    }

    /// The pid a service started by this test process answers as.
    pub(crate) fn service_up(state: &Path) -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
        std::fs::create_dir_all(state).unwrap();
        let me = std::process::id();
        let (port, seen) = serve(json_reply(&format!(r#"{{"version":"2.0.25","pid":{me}}}"#)));
        register(state, me, port, serde_json::json!({}));
        seen
    }

    fn find_binary() -> Option<PathBuf> {
        let on_path = std::env::var_os("PATH")
            .into_iter()
            .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .map(|d| d.join("opencode"))
            .find(|p| p.is_file());
        let found = on_path.or_else(|| {
            let fallback = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".opencode/bin/opencode"));
            fallback.filter(|p| p.is_file())
        });
        if found.is_none() {
            eprintln!("skipped: no opencode binary");
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::testenv::{Isolated, json_reply, register, serve};
    use super::*;

    #[test]
    fn connects_to_a_service_only_when_it_answers_as_the_registered_pid() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::isolated("opencode", vec![], dir.path(), dir.path());
        let me = std::process::id();
        assert_eq!(cli.connect(), Connect::Standalone, "no service.json");

        // The service is what it says: a live pid that answers with that pid.
        let (port, _) = serve(json_reply(&format!(r#"{{"version":"2.0.25","pid":{me}}}"#)));
        register(dir.path(), me, port, serde_json::json!({}));
        assert_eq!(cli.connect(), Connect::Service);
        // Chunked, as a node server may send it.
        let (port, _) = serve(format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n20\r\n{{\"version\":\"2.0.25\",\"pid\":{me}}}\r\n0\r\n\r\n"
        ));
        register(dir.path(), me, port, serde_json::json!({}));
        assert_eq!(cli.connect(), Connect::Service, "a chunked body");

        // The same pid but another version than registered is another service.
        let (port, _) = serve(json_reply(&format!(r#"{{"version":"2.0.26","pid":{me}}}"#)));
        register(dir.path(), me, port, serde_json::json!({}));
        assert_eq!(cli.connect(), Connect::Standalone, "another version");

        // A live pid whose port answers as another process: the registered
        // pid was reused (or the port was), so there is no service.
        let (port, _) = serve(json_reply(r#"{"version":"2.0.25","pid":1}"#));
        register(dir.path(), me, port, serde_json::json!({}));
        assert_eq!(cli.connect(), Connect::Standalone, "another pid answers");
        // Not OpenCode's answer at all, or not a success.
        let (port, _) = serve("HTTP/1.1 200 OK\r\n\r\nhello".into());
        register(dir.path(), me, port, serde_json::json!({}));
        assert_eq!(cli.connect(), Connect::Standalone, "not json");
        let (port, _) = serve("HTTP/1.1 401 Unauthorized\r\n\r\n".to_string() + &format!(r#"{{"version":"2.0.25","pid":{me}}}"#));
        register(dir.path(), me, port, serde_json::json!({}));
        assert_eq!(cli.connect(), Connect::Standalone, "refused");
    }

    /// The stale `service.json` of the review: its pid is alive (a stranger's
    /// process now) and nothing listens at its url. The flagless command
    /// would start a managed service whose boot resumes orphaned turns.
    #[test]
    fn a_stale_service_file_whose_pid_a_stranger_owns_is_not_a_service() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::isolated("opencode", vec![], dir.path(), dir.path());
        let mut stranger = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        assert!(crate::process::alive(stranger.id()));
        // A port nothing listens on.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        register(dir.path(), stranger.id(), free, serde_json::json!({}));
        assert_eq!(cli.connect(), Connect::Standalone);
        // A url that is not loopback http is never asked.
        for url in ["http://203.0.113.9:1234", "https://127.0.0.1:1", "unix:///run/oc.sock", "", "http://127.0.0.1"] {
            std::fs::write(dir.path().join("service.json"), serde_json::json!({ "version": "2.0.25", "url": url, "pid": stranger.id() }).to_string()).unwrap();
            assert_eq!(cli.connect(), Connect::Standalone, "{url}");
        }
        stranger.kill().unwrap();
        let _ = stranger.wait();
        register(dir.path(), stranger.id(), free, serde_json::json!({}));
        assert_eq!(cli.connect(), Connect::Standalone, "its service is gone");
        std::fs::write(dir.path().join("service.json"), "not json").unwrap();
        assert_eq!(cli.connect(), Connect::Standalone);
        std::fs::write(dir.path().join("service.json"), r#"{"pid":4000000000}"#).unwrap();
        assert_eq!(cli.connect(), Connect::Standalone);
        assert_eq!(Cli::ambient(None).connect(), Connect::Standalone, "cannot see a service: never talk to one");
    }

    /// A service with a password is asked with it, and only over loopback.
    #[test]
    fn a_password_protected_service_is_asked_with_its_password() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::isolated("opencode", vec![], dir.path(), dir.path());
        let me = std::process::id();
        let (port, seen) = serve(json_reply(&format!(r#"{{"version":"2.0.25","pid":{me}}}"#)));
        register(dir.path(), me, port, serde_json::json!({ "password": "s3cret" }));
        assert_eq!(cli.connect(), Connect::Service);
        let request = seen.lock().unwrap()[0].clone();
        assert!(request.starts_with("GET /api/info HTTP/1."), "{request}");
        // base64("opencode:s3cret")
        assert!(request.contains("Authorization: Basic b3BlbmNvZGU6czNjcmV0"), "{request}");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
    }

    /// The runner sees only the environment it is given, adds `--standalone`
    /// without a service, and puts stderr in the error.
    #[test]
    fn runs_with_an_explicit_environment_and_reports_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-opencode");
        std::fs::write(&script, "#!/bin/sh\necho \"args: $*\" >&2\necho \"home=$HOME other=${CARGO_MANIFEST_DIR:-none}\"\n[ \"$1\" = ok ] && exit 0\nexit 3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // The test process has CARGO_MANIFEST_DIR (cargo sets it); the child must not.
        let env = vec![(OsString::from("HOME"), OsString::from("/nowhere"))];
        let cli = Cli::isolated(&script, env, dir.path(), dir.path());
        let out = cli.run(&["ok"], true).unwrap();
        assert_eq!(out.trim(), "home=/nowhere other=none");
        let err = cli.run(&["bad", "x"], true).unwrap_err().to_string();
        assert!(err.contains("bad x failed") && err.contains("args: bad x --standalone"), "{err}");
        let err = Cli::isolated(dir.path().join("missing"), vec![], dir.path(), dir.path()).run(&["x"], false).unwrap_err();
        assert!(err.to_string().contains("failed to run opencode"));
    }

    /// `OPENCODE_DB` would send the child to a database asm does not read.
    #[test]
    fn the_children_of_the_ambient_cli_do_not_inherit_a_database_override() {
        let ambient = Cli::ambient(None).command(&["session".into()]).unwrap();
        for var in ["OPENCODE_DB", "OPENCODE_TEST_HOME"] {
            assert!(
                ambient.get_envs().any(|(k, v)| k == var && v.is_none()),
                "{var} must be removed from the child's environment"
            );
        }
        // An explicit environment is exactly that, nothing more to remove.
        let explicit = Cli::isolated("opencode", vec![("HOME".into(), "/x".into())], "/s", "/s").command(&[]).unwrap();
        assert!(explicit.get_envs().all(|(k, _)| k != "OPENCODE_DB"));
    }

    #[test]
    fn an_empty_title_is_refused_before_anything_runs() {
        let cli = Cli::isolated("/nonexistent/opencode", vec![], "/nonexistent", "/nonexistent");
        let err = cli.update_title("ses_x", "   ").unwrap_err().to_string();
        assert!(err.contains("cannot be empty"), "{err}");
    }

    #[test]
    fn reads_the_version() {
        let Some(env) = Isolated::new() else { return };
        assert!(env.cli.version().unwrap().starts_with("2."));
    }
}
