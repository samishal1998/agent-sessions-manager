---
title: Changelog
description: What changed in each release.
---

Releases are tagged on [GitHub](https://github.com/samishal1998/agent-sessions-manager/releases). Dates are tag dates.

## 0.6.0 — 2026-10-03

- **Hub status in both UIs.** The TUI and web UI say whether the hub answered, mark every session with what it needs (Synced / Needs push / Not on hub / Needs pull / New on hub / Check sync / Diverged), list what is new on the hub, and push or pull from the list. A hub that is down is a status with a reason, not an error.
- **Daemon visibility.** `asm daemon status` (and the web hub panel and TUI) show whether a daemon is running, when it last pushed, what is waiting and what failed. One daemon per machine, enforced by a lock.
- **Daemon targeting.** `--active-within N` pushes only the sessions being worked in.
- **Daemon management.** `asm daemon start`, `stop`, `install` (systemd user unit / launchd agent) and `uninstall`.
- **Documentation site.**

## 0.5.0 — 2026-10-03

- `--all` on `push` and `pull` now means **everything the filters match** (`--agent`, `--project`), not "include subagents". Subagent sessions have their own flag, `--include-children`.

## 0.4.2 — 2026-09-27

- Nothing waits for the whole store: both UIs fill their lists as the stores give sessions up, and the search index commits early. See [Indexing](/concepts/indexing/).

## 0.4.1 — 2026-09-27

- TUI facelift: a transcript pane you ask for, keys that fit the terminal, filters named above the status line.

## 0.4.0 — 2026-09-22

- **Multiple machines.** A hub (`asm hub serve`), `join`, `push`, `pull`, `remote list`, and `push --move`, moving sessions between machines under their own ids.
- **Restore for all five agents**: Claude Code, OpenCode, jcode, Codex (same path) and Antigravity.
- A push-only **daemon**, `asm daemon`.
- Hub status in the TUI and web UI (first version).

## 0.3.1 — 2026-08-23

- Projects are named by their tail; the web sidebar is resizable.

## 0.3.0 — 2026-08-21

- **Reply to a session** with `asm send`, the TUI's `c` and the web transcript composer; Codex and Antigravity sessions are readable.

## 0.2.0 — 2026-08-19

- `asm update`, so upgrading is not a shell pipeline.
- Select several sessions in the TUI and the web UI and act on them at once.

## 0.1.0

First release: Claude Code, OpenCode and jcode; list, show, rename, move, archive, delete, export, cross-agent import, search, TUI and web UI.
