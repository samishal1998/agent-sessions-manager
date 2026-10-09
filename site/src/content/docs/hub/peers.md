---
title: Peers — two machines, no hub
description: Move one session straight from one machine to another over ssh or HTTP, with nothing stored in between.
---

A hub is the answer for many machines, a laptop behind a NAT or a desktop that sleeps: only the hub has to be reachable. Two machines that already reach each other can skip it. **Peer mode** moves one session directly from here to there — the same files a push would upload, installed the way a pull installs them — with no archive in between.

```sh
asm peer add desk ssh://me@desk.local       # or https://desk.local:7434 (see below)
asm push 7f3a1c88 --to desk                 # desk now has the session
asm pull 7f3a1c88 --from desk               # later: bring desk's continuation back
asm peer sessions desk                      # what desk has, as `asm list` there shows it
```

## What it does, and does not

- **It compares the two copies on the spot.** Identical → nothing happens. One copy extends the other → the longer one is applied exactly as a hub pull would for that agent: appended for Claude Code and Codex; replaced after a backup for jcode, OpenCode and Antigravity, or, for OpenCode, only renamed when nothing but titles differ. Both changed → **refused**, and nothing is written on either side.
- **It has no history.** There are no revisions, no sync record and no `--force`: a peer transfer never replaces a copy. If the two copies diverged, `asm archive` the one you do not want on one side and transfer again, or sync both machines through a hub, where `asm push --force` keeps the loser as a revision.
- **One session at a time.** `--all`, `--force` and `--move` are the hub's; `asm push --to` and `asm pull --from` take one session.
- **Nothing runs unattended.** No daemon, no remote control toward a peer.

The [sync model](/concepts/sync-model/#hub-and-spoke-with-a-peer-shortcut) says why the hub still wins once there are more than two machines.

## Two transports

### ssh

```sh
asm peer add desk ssh://me@desk.local
```

Every transfer runs `ssh -o BatchMode=yes me@desk.local asm …` and streams the bundle through ssh's stdin or stdout. Nothing is configured on the other machine beyond what ssh already gives you — but three things must hold:

- **ssh must work without a prompt.** `BatchMode` makes a missing key or an unknown host key fail instead of waiting on a prompt nobody sees. Load your key (`ssh-add`) and connect once by hand first.
- **`asm` must be on the PATH of a non-interactive shell there.** ssh runs the remote command through a shell that reads only its non-interactive startup files (`~/.zshenv` for zsh; bash often returns early from `~/.bashrc`), and `~/.local/bin` is frequently not on that PATH. If `asm pull --from desk` says *asm is not on the PATH of a non-interactive shell*, add the directory in the file your shell reads for non-interactive sessions, or link the binary into `/usr/local/bin`.
- **Whoever can ssh in can do everything asm does there.** The peer trusts its ssh users, as it already did. See [Security](/hub/security/#peers).

### HTTP

```sh
# on desk
asm hub serve --peer --port 7434
# on the laptop
ASM_JOIN_TOKEN=asmj_… asm peer add desk http://desk.local:7434
```

`--peer` turns a hub into a peer as well: besides archiving what machines upload, it accepts `asm push --to` straight into **its own agent stores** and serves `asm pull --from` out of them. The three extra routes (`/hub/v1/peer/sessions`, `/hub/v1/peer/send/…`, `/hub/v1/peer/receive`) sit behind the same per-machine credential as everything else; without `--peer` they do not exist, and `asm` on the other side says so. `asm peer add` with an `http(s)://` address does the join exchange once, like `asm join`, and keeps the credential in `peers.json`. The plain-HTTP rules are `asm join`'s: loopback, private LAN and VPN ranges only, unless `--insecure-http`.

A hub's own store is incidental here: a `--peer` hub nobody pushes to stays an empty archive. If the peer is also the hub this machine joined, `asm peer add` reuses that credential rather than joining again (a hub holds one credential per machine, and a second join would replace it).

## Where the session lands

As with a pull: at the same path relative to the home directory on the other machine, or wherever `--project-dir` says (`asm push … --to desk --project-dir /srv/work/app` places it on *desk*; `asm pull … --from desk --project-dir` places it here). A session the other machine already has is updated where it is, and a different `--project-dir` is refused rather than making a second copy.

## Verified and not

Everything on this page was exercised on one Linux machine with the two ends in separate fake homes: the HTTP transport against `asm hub serve --peer`, and the ssh transport through a stand-in `ssh` that ran the remote `asm` locally. A real ssh session to another host, and macOS (`bsdtar` rather than GNU tar on either end), were **not** tried; the wire format is a plain tar of `manifest.json` + `blobs/<sha256>` and the receiving end checks every file against its hash, so a difference would show as a refusal, not a bad install.
