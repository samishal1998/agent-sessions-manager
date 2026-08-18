//! Terminal UI: a session browser over `asm_core::ops`.
//!
//! Structure: the draw loop never blocks — scans, transcript loads, and
//! mutations run on a worker thread and come back over a channel. Resume
//! temporarily leaves the alternate screen, runs the agent interactively,
//! and re-enters the TUI when it exits.

mod app;
mod ui;
mod worker;

use app::{App, LoopOutcome};

pub fn run() -> anyhow::Result<()> {
    let worker = worker::spawn();
    let mut app = App::new(worker);
    app.request_scan();

    loop {
        let mut terminal = ratatui::try_init()?;
        let outcome = app.event_loop(&mut terminal);
        ratatui::restore();
        match outcome {
            Ok(LoopOutcome::Quit) => return Ok(()),
            Ok(LoopOutcome::RunCommand(mut command)) => {
                // Interactive hand-off to the native agent, then back in.
                match command.status() {
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
