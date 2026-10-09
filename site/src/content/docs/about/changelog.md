---
title: Changelog
description: What changed in each release.
---

Releases are tagged on [GitHub](https://github.com/samishal1998/agent-sessions-manager/releases). Dates are tag dates.

## 0.14.0 — 2026-10-09

- **Peer mode: move a session between two machines with no hub.** `asm peer add <name> ssh://[user@]host` (or the URL of an `asm hub serve --peer`), then `asm push <session> --to <name>` and `asm pull <session> --from <name>`; `asm peer sessions <name>` lists what the other machine has. The session crosses as the same bundle a push uploads, packed into one tar stream by the system `tar`, and the receiving end installs it the way a pull does: identical copies are left alone, a copy that extends the other is applied (appended, or replaced after a backup, or only renamed, per agent), and two copies that both changed are refused with nothing written. There is no history on this path — no revisions, no sync record, no `--force` — so it is for two machines that already reach each other; the hub remains the answer for more machines, a NAT or a sleeping laptop. Over ssh, asm runs `ssh host asm …` and needs `asm` on the PATH of a non-interactive shell there (the error says so when it is not). Over HTTP, `asm hub serve --peer` adds three routes behind the usual machine credential that read and write the hub host's own agent stores; without `--peer` they do not exist. Verified on one Linux machine with the two ends in separate homes and a stand-in `ssh`; a real remote host and macOS tar were not tried. See [Peers](/hub/peers/).

## 0.13.1 — 2026-10-09

- **Joining a hub again no longer makes a second machine.** A machine now tells the hub who it is (a salted hash of the OS machine id), and a hub that sees the same identity keeps the record and id and issues a new credential. A machine that already joined several times is recognised by its last credential, and its stale same-name records (unseen for an hour) are dropped. Two machines that merely share a name stay separate.
- **Re-registering no longer shows synced sessions as "Pull first".** The sync record was keyed on the machine id the hub issued, so every `asm join` reset it and every session became *Not compared*. It is now kept per hub and machine identity, and an existing record carries over. A *Not compared* session says so instead of claiming the hub has a newer copy.

## 0.13.0 — 2026-10-08

- **OpenCode 2.x sessions: read, rename, delete, import, and the hub.** OpenCode 2.x moved sessions to `session_v2` and `session_message` (rebuilt from an event log); asm now reads both that and the 1.x schema, so `asm list`, `show`, `search`, `projects`, `export`, the transcripts in the TUI and web UI, and the search index work for a 2.x store, with subagents (children) and the usage totals. A session is shown as running while the OpenCode background service holds its turn. (2.0.25 ignores `time_archived`, so a session archived in 1.x lists as an ordinary one.)
- **Writing a 2.x store goes through `opencode`, never the database.** `asm rename` runs `opencode api session.update`, `asm delete` backs the session and its subagents up as `opencode session import`-able documents (with a `manifest.json` of their directories, on disk and private, before anything is deleted, and re-checked just before) and then runs `opencode session delete`, and `asm import --to opencode` builds a session document and runs `opencode session import`. asm adds `--standalone` unless the background service in `service.json` is proven up (its pid is alive and its url answers `/api/info` with that pid), so it never starts a service that would resume an interrupted turn, and it refuses to change a session that has a turn claimed or input queued. There is no archive or move in OpenCode 2.0.25, so `asm archive` and `asm move` say so (and a remote-control move away from such a machine is refused when planned); `send` is still 1.x only.
- **A store asm cannot be sure of is never written.** A database that cannot be inspected right now (locked, busy) is read as before but every change refuses and says to retry, and a 2.x database that still has 1.x sessions OpenCode has not migrated is reported by `asm doctor` and `asm list`, lists those sessions read-only, and is not changed until OpenCode has finished.
- **Hub push and pull for OpenCode 2.x.** A 2.x session travels as one `opencode session export` document per session of its tree, built from the database without running `opencode`. A pull imports it (the whole tree under one directory on that machine) and brings an older copy up to date by replacing it after a backup, or by renaming it when only titles differ; it never replaces a copy changed here. The hub's admin page shows the transcript. A 1.x session cannot be pulled onto a 2.x machine, or the reverse: the refusal says to use a machine with the same major version. A pulled session carries its permissions, metadata, agent and model as pushed: pull only from machines you control. `asm doctor` no longer reports 1.x lock directories on a 2.x store.

## 0.12.1 — 2026-10-08

- **A store asm cannot read no longer breaks everything.** On a machine with OpenCode 2.x (whose database moved sessions to `session_v2`), `asm list` failed, the web UI said it could not reach the hub, and indexing stopped, all with `no such table: session`. Now an OpenCode store with a schema asm does not read is skipped like an agent that is not installed, and any one agent whose store cannot be read is left out of listings (and reported on stderr by `asm list`, and in the index report) instead of failing the rest. Reading OpenCode 2.x's new schema is not supported yet.

## 0.12.0 — 2026-10-06

- **Remote control, phase 3: move a session to another machine.** `asm control move <session> --from <machine> --to <machine> [--yes] [--wait]` is a plan of three steps: push on the first machine (always exact), pull on the second of exactly that copy, then archive on the first. Nothing is deleted, and `asm unarchive` brings the session back; a finished move prints that line. The hub enforces the order *pull before archive*, and the archive is guarded in three different ways. You confirm it when the plan is made (the machines are looked up first, then `Continue? [y/N]`, or `--yes`; the hub requires the source machine's id as `confirm_archive`, which protects against accidents, not against a holder of the commands token). The source's owner must have allowed it separately (`asm control enable --allow push,pull,archive`; `archive` is not in the default list). And the source machine itself proves, before it archives, that the session is unchanged since the copy that was sent (its sidecar files included) and that the hub still holds that copy; otherwise the step ends `changed_since_move`, with no Retry (a retry could only fail again): run the move again. It also refuses a running session (`live`), one that cannot be archived (`not_archivable`, also when an older archived copy is in the way), and a pull onto a machine that has the session archived ends `archived_here` (`asm unarchive` there, then Retry). If the push or the pull does not succeed, nothing is archived, and a waiting archive says so. `jobs`, `show`, `cancel`, `retry` and `--wait` work on a move like on a send (exit statuses unchanged), and plans now say whether they are a *Send* or a *Move*. The admin page has **Move to another machine** and **Move to machine…**, lists what each machine allows, and greys out a source without archive with the command that turns it on. `asm control send` still copies. See [Moving a session](/hub/control/#moving-a-session).

## 0.11.0 — 2026-10-05

- **Remote control, phase 2: send a session to another machine.** `asm control send <session> --from <machine> --to <machine> [--wait]` makes a *plan*: the first machine pushes, and only once that succeeded does the second pull, installing exactly the revision the push produced. A plan is a chain of steps, each waiting for the one before it; if a step does not succeed, the ones after it are cancelled and `retry` picks the plan up from the step that stopped. `asm control jobs`, `show`, `cancel` and `retry` understand plans, the hub has `/hub/v1/plans` routes, and the hub refuses to delete a session while a command for it is still in flight. This copies the session (the source machine keeps its own copy; nothing is archived or deleted): archiving the source, to make it a true move, comes next. See [Remote control](/hub/control/).
- **`--wait` for scripts.** `push`, `pull` and `send` wait up to ten minutes and end with a status you can act on: 0 ok, 1 not ok, 2 still waiting (it names the step and the machine, which stays queued until its daemon asks and expires after 7 days), 3 hub unreachable (a failed poll is tried five times first). A plan that ends not ok says which step stopped and what to do about its code.
- **Commands tab.** A send is one entry with its steps as a timeline, and **Send to machine…** on the Sessions tab starts one. A step waiting for a machine says when it last asked the hub, and warns when that was over two minutes ago. Cancelling a plan says what has finished and what will not run, Retry says which step it queued again, and a long result is cut to three lines with **Show more**.
- A plan's diverged pull no longer suggests a force push from the target machine, which would replace the hub's copy with the divergent one.

## 0.10.0 — 2026-10-05

- **Remote control, phase 1.** From anywhere with a commands token you can ask a machine to push or pull a session through the hub and follow the result: `asm control push|pull <machine> <session> [--wait]`, `jobs`, `show`, `cancel`, `retry`. A machine answers only after `asm control enable` there, from its daemon, which now also asks the hub for commands every few seconds. A pull is pinned to the exact revision that was pushed, installs only under the machine's home folder, and refuses a running session. The hub admin page has a Commands tab and shows which machines answer. See [Remote control](/hub/control/). `asm hub commands-token` mints the token, which opens nothing else.

## 0.9.0 — 2026-10-04

- **Read transcripts on the hub admin page.** Open a stored session from the admin Sessions tab and read its conversation for every agent (Claude Code, OpenCode, jcode, Codex and Antigravity); the hub rebuilds it from its own copy in a throwaway directory. The Sessions tab gained search, agent and machine filters and sorting, and the page was refined (aligned header, a last-updated time, stat cards, tabs that wrap on phones, narrow-screen lists).
- **The colour-mode switcher is three icon buttons** (sun, moon, monitor) with names and tooltips.

## 0.8.1 — 2026-10-04

- **Light and dark modes.** A colour-mode switcher (Light, Dark, System) sits at the bottom of the sidebar (in the page header on phones, and on the hub admin page) and is remembered per browser. The purple-and-rose palette now comes in both modes, each checked for contrast.

## 0.8.0 — 2026-10-04

- **The web UI is rebuilt on Hearth UI's structure**, not just recoloured: the app frame is a Hearth dashboard shell with a mobile navigation drawer, filters are Hearth inputs, chips and a project combobox, the Hub screen and the hub admin page use Hearth stat cards, data tables, details sheets and tabs, and confirmations are dialogs instead of browser prompts. The Sessions screen starts higher (about seven rows above the fold at 1440×900), row actions line up and collapse into a menu on narrow screens, and the active screen is in the URL.

## 0.7.0 — 2026-10-03

- **Shallow compare.** Sessions both sides have but this machine never compared are now compared without a pull: identical copies become **Synced** on their own, and a difference says ****Differs**. The web Hub view's **Compare** button does one on demand.
- The web UI says which machine this is at the top of the sidebar.
- **A new look.** The web UI is built on Hearth UI with a night-sky purple and rose theme, with buttons, badges, cards, alerts and tooltips from the library.
- **A hub admin page.** `asm hub admin-token` turns on `/admin` on the hub: machines (revoke), the join token, every stored session (delete), storage collection and an activity log, behind a separate admin token. The hub still cannot control a machine's sessions.

## 0.6.2 — 2026-10-03

- **A dedicated Hub view in the web UI.** The sidebar's **Hub** screen shows the connection, the daemon (with its recent events) and every machine on the hub, then a table of every session here and on the hub: its sync state, where it lives, when it changed, and a button for what it needs. Expand a row for the full id, project and branch, the hub revision, size, who pushed it and when, and the terminal command that does the same. The card above the session list is now a compact summary with a link to it.
- **Hub view accessibility and layout**, from an audit of the new screen: it reflows to one block per session on narrow widths with the title readable, disabled buttons look disabled and stay focusable with their reason in the name, filter chips match the state colours and show pressed state in forced-colors mode, and the table's expand controls and live regions are properly wired.
- **"Check sync" is now "Not compared".** The old name read as an action; the state means the hub and this machine both have the session but have never been compared. Its hint says what pulling it will do.

## 0.6.1 — 2026-10-03

- **Web UI accessibility and polish.** Keyboard focus is a real 2px outline that survives forced-colors mode; the mobile drawer respects reduced motion; session metadata wraps instead of clipping on phones; faint text and field borders meet contrast; there is a skip link and a heading outline.
- **Colour and type.** Synced, Delete and the hub filter chips carry their colour; one type scale and shared status tint tokens replace scattered sizes and `rgba()` values; the hub panel groups with space.

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
