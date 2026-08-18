fn main() -> anyhow::Result<()> {
    match asm_cli::run()? {
        Some(asm_cli::Frontend::Tui) => asm_tui::run(),
        Some(asm_cli::Frontend::Serve { host, port }) => asm_web::run(&host, port),
        None => Ok(()),
    }
}
