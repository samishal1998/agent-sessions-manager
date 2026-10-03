---
title: The terminal UI
description: Browse, search and act on sessions from the keyboard, including the hub view.
---

```sh
asm          # on a terminal
asm tui      # explicitly
```

Every per-session verb is a keystroke, so the TUI is not a read-only view of the CLI. Press `?` for every key, grouped; the footer shows the ones that fit.

## Layout

The list uses the whole width. The **transcript pane** is closed until you ask for it (`→`), so wide project names and the sync column stay readable; a transcript search opens it and follows the match you are on. What is narrowing the list is named above the status line, and `esc` clears it.

## Keys

See the complete [key reference](/reference/tui-keys/). The ones to learn first:

| Key | |
|---|---|
| `?` | Every key, grouped. |
| `⏎` | Resume in the native agent (the TUI steps aside and comes back). |
| `/` · `s` | Filter the list as you type · full-text search across transcripts. |
| `A` · `P` | Pick agents (space toggles) · pick a project (type to narrow). |
| `r` `a` `d` | Rename · archive/unarchive · delete (confirmed, backed up). |
| `m` `i` `e` | Move · import into the other agent · export IR. |
| `␣` `*` | Tick this session · tick everything the filter shows. |
| `p` `c` | Push to the hub · reply to the session. |
| `H` | The hub view. |
| `esc` | Back out one layer; with nothing left, quit. |

## Acting on many sessions

With anything ticked, `a` `d` `m` `e` `i` run over the whole selection instead of the row under the cursor. A batch attempts every session, so one failure cannot strand the rest, and what did not work is listed per session afterwards.

:::note
Bulk unarchive only reaches OpenCode sessions, because only they stay listed once archived. Claude and jcode sessions leave their store for asm's archive and are restored by reference with `asm unarchive <id>`.
:::

## The hub

When this machine has [joined a hub](/hub/setup/), three things appear:

- a **sync** column in the list — one word per session, in the shared [vocabulary](/start/concepts/#sync-states): `✓ Synced`, `↑ Needs push`, `↓ Needs pull`, `⇅ Diverged`…
- a **chip at the right of the status line**: connection first (`⟳ hub…`, `⚠ hub unreachable — H, then r retries`), then what is out of step (`⇄ hub: 2 to push · 1 to pull`), then whether a [daemon](/hub/daemon/) is on (`daemon on` / `no daemon` / `daemon stuck`).
- the **hub view** (`H`): every session here and on the hub, the ones that need a decision first. `⏎` does what the row needs (push it, pull it, or says why neither), `p` pushes, `f` hides what is level, `r` asks the hub again. The top line reports the daemon: running with its last push time and what is waiting, or not running with the command to start one.

A hub that stops answering is a status, not an error: the last known states stay on screen, dimmed and marked old, and push/pull refuse until it is back. A status check makes one short attempt rather than retrying like a transfer, so a dead hub is reported in about a second.

## Replying

`c` sends a message into the selected session and streams the reply — see [Reply to a session](/guides/reply/) for what that does and does not do.
