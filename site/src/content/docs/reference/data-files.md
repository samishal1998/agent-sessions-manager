---
title: Data files
description: Every file asm writes, and which are safe to delete.
---

asm writes only inside its own directory — `$XDG_DATA_HOME/asm` (default `~/.local/share/asm`), overridable with `ASM_DATA_DIR` — plus the agents' stores through the sanctioned paths described in the [safety model](/concepts/safety/).

| Path | What | Safe to delete? |
|---|---|---|
| `archive/<agent>/<id>/` | Archived sessions: `manifest.json` + `native/`. | **No** — it is the only copy of an archived session. |
| `backups/<agent>/<id>/<timestamp>/` | Copies taken before a delete or a hub replace. | Only if you no longer want the undo. asm never deletes them. |
| `index/sessions.db` | The search index. | Yes; it is rebuilt. |
| `hub-client.json` | This machine's hub URL, machine id and **credential** (mode `0600`). | Deleting it un-joins the machine. |
| `machine-uid` | This machine's identity for hubs, only where the OS has no machine id of its own. | A new identity: the hub sees a new machine on the next join. |
| `hub-state.json` | Per session, the last revision agreed with the hub and the local fingerprint at that time. | Yes, but every session then shows **Not compared** until pulled or pushed. |
| `daemon/daemon.lock` | Held by a running daemon. | Harmless; recreated. |
| `daemon/status.json` | What the daemon last reported. | Yes. |
| `daemon/daemon.log` | Output of `asm daemon start`. | Yes. |
| `tmp/` | curl's request and reply files (mode `0700`). | Yes, when nothing is running. |
| `hub/` | **On a hub machine:** the hub's store. | **No.** |

## On a hub

```text
hub/hub.json                          name and the join token's hash
hub/machines.json                     joined machines: id, name, credential hash, last seen
hub/blobs/<sha256>                    content-addressed file contents
hub/sessions/<agent>/<id>/head        the current revision
hub/sessions/<agent>/<id>/revisions/  every revision's manifest
```

`asm sync init` is separate: it turns `archive/` into a git repository so archived sessions can be versioned and pushed to a remote you choose. That path leaves the transport to you; the hub is the one asm runs itself.

## Environment variables

| Variable | Effect |
|---|---|
| `ASM_DATA_DIR` | Overrides asm's data directory. |
| `XDG_DATA_HOME` | Parent of asm's directory, and where OpenCode keeps its database. |
| `CLAUDE_CONFIG_DIR` · `JCODE_HOME` · `CODEX_HOME` · `ASM_ANTIGRAVITY_ROOT` | Where to find each agent's store. |
| `ASM_JOIN_TOKEN` | The token `asm join` uses. |
| `ASM_VERSION` · `ASM_INSTALL_DIR` · `ASM_BASE_URL` | Used by `install.sh`. |

Point all of these at a scratch directory to try asm on fake data without touching your real stores.
