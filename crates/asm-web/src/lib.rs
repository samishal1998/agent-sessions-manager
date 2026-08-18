//! Local WebUI: an axum JSON API over `asm_core::ops` plus the embedded
//! Vue frontend. Defaults to loopback — this is a personal dashboard, not a
//! service — and says so loudly when bound anywhere else.

mod api;
mod statics;

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

use anyhow::Context;

pub fn run(host: &str, port: u16) -> anyhow::Result<()> {
    let addr = resolve_bind(host, port)?;

    if !addr.ip().is_loopback() {
        warn_exposed(&addr);
    }

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
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("cannot bind {addr}"))?;
        eprintln!("asm web ui on http://{}/  (ctrl-c to stop)", display_addr(&addr));
        axum::serve(listener, app).await?;
        Ok(())
    })
}

/// Turn a host string into an address to bind.
///
/// A bare IP is used as given (which is also how an IPv6 literal like `::1`
/// avoids being mistaken for a `host:port` string); anything else goes
/// through name resolution.
fn resolve_bind(host: &str, port: u16) -> anyhow::Result<SocketAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }
    (host, port)
        .to_socket_addrs()
        .with_context(|| format!("cannot resolve host '{host}'"))?
        .next()
        .with_context(|| format!("host '{host}' resolved to no addresses"))
}

/// `0.0.0.0` is not a browsable address; point the user at something they
/// can actually open.
fn display_addr(addr: &SocketAddr) -> String {
    if addr.ip().is_unspecified() {
        match addr.ip() {
            IpAddr::V4(_) => format!("127.0.0.1:{}", addr.port()),
            IpAddr::V6(_) => format!("[::1]:{}", addr.port()),
        }
    } else {
        addr.to_string()
    }
}

fn warn_exposed(addr: &SocketAddr) {
    let scope = if addr.ip().is_unspecified() {
        "every network interface".to_string()
    } else {
        format!("{}", addr.ip())
    };
    eprintln!(
        "\n\
         ┌─ WARNING ─────────────────────────────────────────────────────────\n\
         │ Binding {scope}, not just loopback.\n\
         │\n\
         │ This API has NO authentication. Anyone who can reach it can read\n\
         │ every conversation in your sessions and rename, move, archive,\n\
         │ import, or DELETE them. The cross-origin guard only stops other\n\
         │ web pages; it does not stop a direct request.\n\
         │\n\
         │ Only do this on a network you trust, and stop the server when you\n\
         │ are done.\n\
         └───────────────────────────────────────────────────────────────────\n"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_ips_are_used_verbatim() {
        assert_eq!(resolve_bind("127.0.0.1", 7433).unwrap().to_string(), "127.0.0.1:7433");
        assert_eq!(resolve_bind("0.0.0.0", 80).unwrap().to_string(), "0.0.0.0:80");
        // An IPv6 literal must not be parsed as host:port.
        let v6 = resolve_bind("::1", 7433).unwrap();
        assert!(v6.is_ipv6());
        assert_eq!(v6.port(), 7433);
        assert!(v6.ip().is_loopback());
    }

    #[test]
    fn hostnames_resolve() {
        let addr = resolve_bind("localhost", 7433).unwrap();
        assert!(addr.ip().is_loopback());
        assert_eq!(addr.port(), 7433);
    }

    #[test]
    fn unresolvable_hosts_are_an_error_not_a_silent_fallback() {
        assert!(resolve_bind("no-such-host.invalid", 7433).is_err());
    }

    #[test]
    fn loopback_is_the_only_quiet_case() {
        assert!(resolve_bind("127.0.0.1", 1).unwrap().ip().is_loopback());
        assert!(!resolve_bind("0.0.0.0", 1).unwrap().ip().is_loopback());
        assert!(!resolve_bind("192.168.1.5", 1).unwrap().ip().is_loopback());
    }

    #[test]
    fn unspecified_addresses_display_as_something_browsable() {
        assert_eq!(display_addr(&resolve_bind("0.0.0.0", 7433).unwrap()), "127.0.0.1:7433");
        assert_eq!(display_addr(&resolve_bind("::", 7433).unwrap()), "[::1]:7433");
        assert_eq!(display_addr(&resolve_bind("192.168.1.5", 80).unwrap()), "192.168.1.5:80");
    }
}
