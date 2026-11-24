//! Terminal UI: a session browser over `asm_core::ops`.
//!
//! Structure: the draw loop never blocks — scans, transcript loads, and
//! mutations run on a worker thread and come back over a channel. Resume
//! temporarily leaves the alternate screen, runs the agent interactively,
//! and re-enters the TUI when it exits.

mod app;
mod theme;
mod ui;
mod worker;

use app::{App, LoopOutcome};

/// `here` is where to start narrowed to (`asm --here`), and whether to every
/// worktree of its repository (`asm -W`).
pub fn run(here: Option<(asm_core::ops::Here, bool)>) -> anyhow::Result<()> {
    let worker = worker::spawn();
    let mut app = App::new(worker);
    if let Some((here, all)) = here {
        app.here = Some(here);
        app.all_worktrees = all;
    }
    app.request_scan();

    loop {
        let mut terminal = ratatui::try_init()?;
        let outcome = app.event_loop(&mut terminal);
        ratatui::restore();
        match outcome {
            Ok(LoopOutcome::Quit) => return Ok(()),
            Ok(LoopOutcome::Print(line)) => {
                println!("{line}");
                return Ok(());
            }
            Ok(LoopOutcome::RunCommand(mut command)) => {
                // Interactive hand-off to the native agent, then back in.
                match hand_over(&mut command) {
                    Ok(status) if !status.success() => {
                        app.set_status(format!("agent exited with {status}"));
                    }
                    Ok(_) => app.set_status("resumed session ended".to_string()),
                    Err(e) => app.set_status(format!("failed to launch agent: {e}")),
                }
                app.request_scan();
            }
            Err(e) => return Err(e),
        }
    }
}

/// Run `command` with the terminal handed to it, as a shell runs a job: in a
/// process group of its own that is the terminal's foreground group, so the
/// terminal names it, Ctrl-C and Ctrl-Z reach only the agent, and its title
/// is its own. asm takes the terminal back when it exits.
#[cfg(unix)]
fn hand_over(command: &mut std::process::Command) -> std::io::Result<std::process::ExitStatus> {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    const TTY: libc::c_int = libc::STDIN_FILENO;

    /// Make `group` the terminal's foreground group. A background group that
    /// tries is stopped by SIGTTOU, so it is ignored for the call.
    fn foreground(group: libc::pid_t) {
        // SAFETY: plain syscalls on a descriptor we own; the old disposition
        // is put back. Async-signal-safe, so also fine between fork and exec.
        unsafe {
            let old = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
            libc::tcsetpgrp(TTY, group);
            libc::signal(libc::SIGTTOU, old);
        }
    }

    // SAFETY: read-only queries.
    let ours = unsafe { libc::getpgrp() };
    if unsafe { libc::tcgetpgrp(TTY) } != ours {
        // Not the foreground of a terminal (piped, or already in the
        // background): there is no terminal to hand over.
        return command.status();
    }
    // SAFETY: setpgid, getpid and tcsetpgrp are async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            libc::setpgid(0, 0);
            foreground(libc::getpid());
            Ok(())
        });
    }
    // An agent that fails to start (not on PATH) has already taken the
    // terminal in its child: take it back before saying so.
    let child = command.spawn().inspect_err(|_| foreground(ours))?;
    let pid = child.id() as libc::pid_t;
    let status = loop {
        let mut raw = 0;
        // SAFETY: waits on our own child.
        if unsafe { libc::waitpid(pid, &mut raw, libc::WUNTRACED) } < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            foreground(ours);
            return Err(error);
        }
        if libc::WIFSTOPPED(raw) {
            // Ctrl-Z in the agent: stop asm with it, as the shell expects of
            // the job it started, and when the shell continues asm, give the
            // agent the terminal back and continue it too.
            foreground(ours);
            // SAFETY: signals to ourselves and to the agent's group.
            unsafe {
                libc::raise(libc::SIGTSTP);
                foreground(pid);
                libc::kill(-pid, libc::SIGCONT);
            }
            continue;
        }
        break std::process::ExitStatus::from_raw(raw);
    };
    foreground(ours);
    Ok(status)
}

#[cfg(not(unix))]
fn hand_over(command: &mut std::process::Command) -> std::io::Result<std::process::ExitStatus> {
    command.status()
}
