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
- **Remote control** has its own **commands token** (`asm hub commands-token`): it creates, lists, cancels and retries commands for machines that opted in, and opens neither the admin API nor any machine route. A machine credential cannot create commands. What a stolen commands token can do is bounded to asking opted-in machines to push or pull sessions the hub already holds. **Archive** is a separate opt-in (`asm control enable --allow push,pull,archive`), never in the default list, and is only ever the last step of a *move*. Three checks stand around it, and they are not the same kind: the **confirmation** (the plan needs the source machine's id as `confirm_archive`; the `400` for a missing one names the id) protects against *accidents*, not against someone who holds the commands token; the **owner's opt-in** and the **machine's own proof** do the bounding. The hub enforces the order *pull before archive*, and the machine additionally proves that the session is unchanged since the copy that was sent and that the hub still holds that copy. So the reach of a stolen commands token is: only machines whose owner opted into archive, only sessions the token holder can name, at most 10 concurrent plans per source machine, all of it reversible with `asm unarchive`, and nothing deleted. See [Remote control](/hub/control/).
- The hub's admin page and API use a **separate admin token** (`asm hub admin-token`) that no machine credential can stand in for, and that opens none of the machine routes. See [Administering the hub](/hub/admin/).

## Keeping secrets out of the process list

- The join token comes from `$ASM_JOIN_TOKEN` (or `--token -`, on stdin). A token passed as a flag is visible to every user of the machine while asm runs (the help says so).
- asm hands every credential to `curl` **on stdin**, never in argv. `curl` is run with `-q` and `--noproxy '*'`, so neither a `~/.curlrc` nor an `http_proxy` sees them, and without `-L`, so a redirect cannot replay the credential.
- curl's request and reply files live in a `0700` directory under asm's data dir.

## What the hub validates

Agent names, session ids (`[A-Za-z0-9._-]{1,128}`), blob hashes (64 hex digits) and every file name in a manifest against a per-agent whitelist — a manifest name is never used as a path component. Uploads are hashed while written and discarded on mismatch, and a size cap applies per file.

## Peers

[Peer mode](/hub/peers/) moves a session straight into another machine's agent store, so its trust model is different from the hub's, and it is opt-in on both transports.

- **An HTTP peer is a hub started with `--peer`.** Without the flag the `/hub/v1/peer/*` routes do not exist. With it, any machine holding a credential for that hub can install sessions into **the hub host's own agent stores** and read every session there — the hub is no longer only an archive. Turn it on only on a machine whose agents you want other machines to write into, and `asm hub revoke` a machine to shut it out of both the archive and the peer routes.
- **An ssh peer trusts whoever has shell access**, as the machine already did: `asm hub receive` and `asm hub send` run as the ssh user and can do nothing that user could not do by hand. asm adds no credential of its own on this path; the ssh key is the credential.
- **What arrives is checked, not trusted.** The bundle is unpacked into a private scratch directory under asm's data dir, and only `manifest.json` and regular files at `blobs/<sha256>` whose contents hash to their name are accepted — a symlink, a stray file or a wrong hash refuses the whole bundle. The manifest goes through the same validation a hub applies (session id, per-agent file whitelist, no path components, nothing beneath a symlink entry), and the install is the same code a hub pull runs, with the same refusals: a running session, an id already present in two places, a directory that is not where the session is filed, a copy that diverged. A `--peer` hub also applies its `--max-file-mb` cap to the whole bundle.
- **`peers.json` is `0600`** and holds an HTTP peer's credential beside its URL; an ssh peer stores only the host.

## What it does not do

- **It is one person's hub.** Every joined machine can read every session on it, and sessions contain whatever your agents saw: source code, command output, possibly secrets.
- **It does not encrypt at rest.** Blobs are stored as the agents wrote them. Use disk encryption on the hub machine.
- **The web UI is separate and has no authentication** — see [its security note](/guides/web-ui/#security).
