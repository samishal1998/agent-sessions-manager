//! `asm daemon start | stop | install | uninstall`: running the daemon
//! without writing a service unit by hand.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use asm_core::hub::daemon::{self, DaemonState};

/// What the daemon should do, as flags; the same set for every verb that
/// starts one.
#[derive(Clone, Copy)]
pub struct Opts {
    pub interval: u64,
    pub active_within: Option<u64>,
}

impl Opts {
    fn args(self) -> Vec<String> {
        let mut a = vec!["daemon".into(), "--interval".into(), self.interval.to_string()];
        if let Some(m) = self.active_within {
            a.extend(["--active-within".into(), m.to_string()]);
        }
        a
    }
}

fn exe() -> anyhow::Result<PathBuf> {
    std::env::current_exe().and_then(|p| p.canonicalize()).context("cannot find this asm binary")
}

fn data() -> anyhow::Result<PathBuf> {
    asm_core::paths::data_dir().context("cannot determine asm data dir")
}

fn wait_until(mut what: impl FnMut() -> bool, secs: u64) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if what() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    what()
}

fn running() -> anyhow::Result<Option<u32>> {
    let st = daemon::status()?;
    Ok((st.state != DaemonState::NotRunning).then(|| st.file.map_or(0, |f| f.pid)))
}

/// Run a daemon in the background, logging to `<data>/daemon/daemon.log`.
pub fn start(opts: Opts) -> anyhow::Result<()> {
    asm_core::hub::client::load()?; // not joined → say so here, not in a log
    if let Some(pid) = running()? {
        bail!("a daemon is already running (pid {pid}); `asm daemon status`");
    }
    let log = data()?.join("daemon").join("daemon.log");
    std::fs::create_dir_all(log.parent().unwrap())?;
    let out = std::fs::OpenOptions::new().create(true).append(true).open(&log)?;
    let mut cmd = Command::new(exe()?);
    cmd.args(opts.args()).stdin(std::process::Stdio::null()).stdout(out.try_clone()?).stderr(out);
    // Its own process group, so closing this terminal does not take it too.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().context("cannot start the daemon")?;
    let up = wait_until(|| child.try_wait().ok().flatten().is_some() || matches!(running(), Ok(Some(_))), 5);
    if up && let Some(pid) = running()? {
        println!("Daemon running (pid {pid}); log {}.", log.display());
        return Ok(());
    }
    let tail: Vec<String> = std::fs::read_to_string(&log)?.lines().rev().take(3).map(String::from).collect();
    bail!("the daemon did not stay up:\n  {}", tail.into_iter().rev().collect::<Vec<_>>().join("\n  "))
}

/// Ask the running daemon to stop and wait for it to let go of its lock.
pub fn stop() -> anyhow::Result<()> {
    let st = daemon::status()?;
    let Some(pid) = st.file.as_ref().map(|f| f.pid).filter(|_| st.state != DaemonState::NotRunning) else {
        println!("No daemon is running.");
        return Ok(());
    };
    // SIGTERM through kill(1): the pid is the one the lock holder wrote.
    let sent = Command::new("kill").arg(pid.to_string()).status().is_ok_and(|s| s.success());
    if !sent || !wait_until(|| matches!(running(), Ok(None)), 10) {
        bail!("pid {pid} did not stop; `kill -9 {pid}` if it is really stuck");
    }
    println!("Stopped the daemon (pid {pid}). If it was installed as a service, `asm daemon uninstall` keeps it from coming back.");
    Ok(())
}

fn home() -> anyhow::Result<PathBuf> {
    asm_core::paths::home().context("cannot determine the home directory")
}

fn unit_path() -> anyhow::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    return Ok(home()?.join("Library/LaunchAgents/dev.asm.daemon.plist"));
    #[cfg(not(target_os = "macos"))]
    return Ok(std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or(home()?.join(".config"))
        .join("systemd/user/asm-daemon.service"));
}

/// systemd splits ExecStart on spaces, so a path or argument with one is
/// quoted; `%` is a specifier escape and must be doubled.
fn systemd_word(s: &str) -> String {
    let s = s.replace('%', "%%").replace('\\', "\\\\").replace('"', "\\\"");
    if s.contains(char::is_whitespace) { format!("\"{s}\"") } else { s }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn unit_text(exe: &Path, opts: Opts, data_dir: Option<&str>) -> String {
    let mut words = vec![systemd_word(&exe.to_string_lossy())];
    words.extend(opts.args().iter().map(|a| systemd_word(a)));
    if cfg!(target_os = "macos") {
        let args: String =
            std::iter::once(exe.to_string_lossy().into_owned()).chain(opts.args()).map(|a| format!("<string>{}</string>", xml(&a))).collect();
        let env = data_dir.map_or(String::new(), |d| {
            format!("\n  <key>EnvironmentVariables</key><dict><key>ASM_DATA_DIR</key><string>{}</string></dict>", xml(d))
        });
        return format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n  <key>Label</key><string>dev.asm.daemon</string>\n  <key>ProgramArguments</key><array>{args}</array>{env}\n  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><true/>\n</dict></plist>\n"
        );
    }
    let env = data_dir.map_or(String::new(), |d| format!("Environment={}\n", systemd_word(&format!("ASM_DATA_DIR={d}"))));
    format!(
        "[Unit]\nDescription=Push coding-agent sessions to the asm hub\n\n[Service]\nExecStart={}\n{env}Restart=on-failure\nRestartSec=30\n\n[Install]\nWantedBy=default.target\n",
        words.join(" ")
    )
}

/// Write the user service for this machine and, unless told not to, turn it
/// on. Idempotent: running it again rewrites the unit with the new flags.
// ponytail: only ASM_DATA_DIR is carried into the service; agent-specific
// env (CLAUDE_CONFIG_DIR, …) is not, since a service shell would not have
// it either. Add them here if someone installs with a custom store.
pub fn install(opts: Opts, start_now: bool) -> anyhow::Result<()> {
    asm_core::hub::client::load()?;
    let path = unit_path()?;
    let text = unit_text(&exe()?, opts, std::env::var("ASM_DATA_DIR").ok().as_deref());
    std::fs::create_dir_all(path.parent().context("unit path has no directory")?)?;
    asm_core::fsutil::write_atomic(&path, text.as_bytes())?;
    println!("Wrote {}.", path.display());
    if !start_now {
        println!("Not started (--no-start).");
        return Ok(());
    }
    if cfg!(target_os = "macos") {
        run("launchctl", &["load", "-w", &path.to_string_lossy()])?;
    } else {
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", "--now", "asm-daemon"])?;
    }
    println!("The daemon starts with your login session and restarts if it dies. `asm daemon status` shows it.");
    Ok(())
}

pub fn uninstall() -> anyhow::Result<()> {
    let path = unit_path()?;
    if !path.exists() {
        println!("No service installed ({}).", path.display());
        return Ok(());
    }
    if cfg!(target_os = "macos") {
        let _ = run("launchctl", &["unload", "-w", &path.to_string_lossy()]);
    } else {
        let _ = run("systemctl", &["--user", "disable", "--now", "asm-daemon"]);
    }
    std::fs::remove_file(&path)?;
    if !cfg!(target_os = "macos") {
        let _ = run("systemctl", &["--user", "daemon-reload"]);
    }
    println!("Removed {}.", path.display());
    Ok(())
}

fn run(program: &str, args: &[&str]) -> anyhow::Result<()> {
    match Command::new(program).args(args).status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => bail!("`{program} {}` failed ({s})", args.join(" ")),
        Err(e) => bail!("cannot run {program}: {e}. The unit is written; enable it yourself."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTS: Opts = Opts { interval: 30, active_within: Some(90) };

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_unit_runs_this_binary_with_the_chosen_flags() {
        let t = unit_text(Path::new("/opt/my tools/asm"), OPTS, Some("/data/a b"));
        assert!(t.contains("ExecStart=\"/opt/my tools/asm\" daemon --interval 30 --active-within 90\n"), "{t}");
        assert!(t.contains("Environment=\"ASM_DATA_DIR=/data/a b\"\n"), "{t}");
        assert!(t.contains("WantedBy=default.target") && t.contains("Restart=on-failure"));
    }

    #[test]
    fn specifiers_and_quotes_cannot_break_out_of_a_unit_line() {
        assert_eq!(systemd_word("100%"), "100%%");
        assert_eq!(systemd_word("a\"b"), "a\\\"b");
        assert_eq!(xml("<a&b>"), "&lt;a&amp;b&gt;");
    }
}
