/// Rust ignores SIGPIPE, so a closed pipe surfaces as a write error and
/// `println!` panics — `asm list | head` would print a panic instead of
/// exiting quietly. Restore the default disposition, which is what every
/// other command-line tool does.
#[cfg(unix)]
fn restore_sigpipe() {
    // Safety: setting a signal disposition before any threads are spawned.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
}

#[cfg(not(unix))]
fn restore_sigpipe() {}

fn main() -> anyhow::Result<()> {
    restore_sigpipe();
    match asm_cli::run()? {
        Some(asm_cli::Frontend::Tui) => asm_tui::run(),
        Some(asm_cli::Frontend::Serve { host, port }) => asm_web::run(&host, port),
        None => Ok(()),
    }
}
