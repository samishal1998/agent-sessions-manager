//! Local WebUI: an axum JSON API over `asm_core::ops` plus the embedded
//! Vue frontend. Binds localhost only — this is a personal dashboard, not
//! a service.

mod api;
mod statics;

use std::net::{Ipv4Addr, SocketAddr};

pub fn run(port: u16) -> anyhow::Result<()> {
    // Warm the search index off the request path so the first search is
    // fast; a failure here is not fatal, searches just return what is
    // already indexed.
    std::thread::spawn(|| match asm_core::index::Index::open() {
        Ok(mut index) => {
            if let Err(e) = index.refresh(|_| {}) {
                eprintln!("search index refresh failed: {e}");
            }
        }
        Err(e) => eprintln!("could not open search index: {e}"),
    });

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let app = api::router().fallback(statics::serve_static);
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        eprintln!("asm web ui on http://{addr}/  (ctrl-c to stop)");
        axum::serve(listener, app).await?;
        Ok(())
    })
}
