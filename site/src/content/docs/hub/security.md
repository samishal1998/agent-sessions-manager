---
title: Security
description: The hub's threat model, stated plainly.
---

## Transport

The hub speaks HTTP and **you** provide TLS: run it on a tailnet or LAN you trust, or behind a reverse proxy (`tailscale serve`, Caddy). `asm join` refuses plain HTTP to anything outside loopback, private ranges and VPN ranges unless you pass `--insecure-http`; `https://` is always allowed. The client is the system `curl`, so HTTPS works with your system's trust store and no TLS stack is linked into asm.

## Credentials

- The **join token** buys a machine its **own** credential. The hub keeps only a hash of each credential; the join token is compared in constant time and a wrong token writes nothing.
- `asm hub revoke <machine>` shuts one laptop out without touching the rest; `asm hub token --rotate` retires the join token without disconnecting machines that already joined.
- Every route except the join is behind a bearer check, so an unknown path returns 401, not 404.
- The hub's admin page and API use a **separate admin token** (`asm hub admin-token`) that no machine credential can stand in for, and that opens none of the machine routes. See [Administering the hub](/hub/admin/).

## Keeping secrets out of the process list

- The join token comes from `$ASM_JOIN_TOKEN` (or `--token -`, on stdin). A token passed as a flag is visible to every user of the machine while asm runs (the help says so).
- asm hands every credential to `curl` **on stdin**, never in argv. `curl` is run with `-q` and `--noproxy '*'`, so neither a `~/.curlrc` nor an `http_proxy` sees them, and without `-L`, so a redirect cannot replay the credential.
- curl's request and reply files live in a `0700` directory under asm's data dir.

## What the hub validates

Agent names, session ids (`[A-Za-z0-9._-]{1,128}`), blob hashes (64 hex digits) and every file name in a manifest against a per-agent whitelist — a manifest name is never used as a path component. Uploads are hashed while written and discarded on mismatch, and a size cap applies per file.

## What it does not do

- **It is one person's hub.** Every joined machine can read every session on it, and sessions contain whatever your agents saw: source code, command output, possibly secrets.
- **It does not encrypt at rest.** Blobs are stored as the agents wrote them. Use disk encryption on the hub machine.
- **The web UI is separate and has no authentication** — see [its security note](/guides/web-ui/#security).
