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

fn main() {
    restore_sigpipe();
    let result = match asm_cli::run() {
        Ok(Some(asm_cli::Frontend::Tui { here })) => asm_tui::run(here),
        Ok(Some(asm_cli::Frontend::Serve { host, port, token })) => asm_web::run(&host, port, token),
        Ok(Some(asm_cli::Frontend::Hub { host, port, max_file_bytes, peer })) => {
            asm_web::hub::run(&host, port, max_file_bytes, peer)
        }
        Ok(None) => Ok(()),
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        // The status says what kind of failure it was; see `asm_cli::report_error`.
        std::process::exit(asm_cli::report_error(&e));
    }
}
