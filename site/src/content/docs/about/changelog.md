---
title: Changelog
description: What changed in each release.
---

Releases are tagged on [GitHub](https://github.com/samishal1998/agent-sessions-manager/releases). Dates are tag dates.

## 0.18.1 — 2026-10-26

- **A calmer, denser web UI.** The page and cards are neutral slate in both modes (near-white in light, dark slate in dark); only raised surfaces, panels and selected items keep a slight purple tint, so the accent stands out. Fields, buttons, checkboxes and session rows are shorter, radii are smaller, and the purple glow behind the page is gone. Touch screens keep 44px targets.

## 0.18.0 — 2026-10-26

- **Sessions of where you are.** The global `--here` narrows any command to the git worktree containing the current directory (a linked worktree, not its main one), and `-W`/`--all-worktrees` to every worktree of its repository; outside git both mean the current directory. Bare `asm --here` or `asm -W` opens the TUI narrowed that way. In the TUI, `.` turns it on and off and `w` switches between this worktree and all of them (on a `P` project, `w` narrows to the worktree you are in, else the main one). A worktree nested inside the one you are in, as Claude Code keeps them at `.claude/worktrees/<name>`, is its own: `--here` leaves its sessions out and `-W` brings them in. `asm projects` and `asm worktrees` honour both, keeping only the project and worktrees you are in. The flag is `--all-worktrees` because `asm projects --worktrees` already has the shorter name.

## 0.17.0 — 2026-10-25

- **The web API can run a push, pull or bulk action as a task.** `POST /api/bulk`, `/api/hub/pull`, `/api/hub/pull-all` and `/api/hub/compare` take `?async=1`: they answer `202 {"job": id}` at once, and `GET /api/tasks` and `/api/tasks/{id}` follow the job through `queued`, `running`, and `ok` with the body the route would have answered or `failed` with its error, counting sessions done as it goes. The same request again while it runs is the same job. Without `?async` nothing changes.
- **`asm join` says when the hub's name does not resolve.** A plain-HTTP join to a name this machine could not look up (a Tailscale name with MagicDNS off, say) was refused as "plain HTTP to an address outside loopback, a private LAN or a VPN". It now says the name does not resolve and suggests the hub's 100.x address.
- **`asm join` pointed at the web UI says so.** Joining `asm serve`'s port (7433) instead of the hub's (7434) failed with `hub answered 200: <!doctype html>…`. It now says the address served a web page, not a hub, and names both default ports.
- **The TUI keeps saying what it is doing until it is done.** A push, pull, send or other action asked for while a scan or a hub check was running showed nothing, and the scan's session count then overwrote "pushing…". What was asked for now stays on the status line until its own answer arrives, and more than one waiting is shown as `⟳ N queued`.
- **The web UI shows that a push, pull or bulk action was heard, is running, and finished.** Clicking Push, Pull, Compare, Pull all or a bulk verb gave no sign of anything until the work was done. The clicked button now turns into a spinner at once and stays disabled until its task ends, hub rows being worked on read "Pushing…" / "Pulling…", and a tasks button beside the toolbar's status (and in the Hub view) opens a panel of the server's recent tasks: each one's state, how long it took, how far it got and what it said, with a failure's error or a report's problems one click away. Clicking the same thing again while it runs starts nothing and points at the task already doing it.

## 0.16.1 — 2026-10-22

- **A refresh of the index opens each OpenCode store once, not once per session.** Every refresh (after every scan in the TUI, before every `asm search`) asked each row-backed session's store for its message count and newest timestamp over a connection of its own; a store of thousands of sessions paid thousands of opens to learn nothing had changed. The watermarks are now read in one query per store. Fingerprints are unchanged, so nothing is re-extracted on upgrade.
- **`asm index --agent AGENT` and `--project DIR` now mean it.** The flags were accepted and ignored: `asm index --agent codex` read every store. A narrowed refresh reads only what it names and leaves the rest of the index as it was.

## 0.16.0 — 2026-10-21

- **The CLI and the web API are a dependable machine interface.** [JSON and exit codes](/reference/json/) is the contract: every `--json` shape, a `json_contract_version` (1) that is bumped only when a shape stops being additive, and the promise that within a version a shape only gains. Nothing that was printed before changed: `list --json` and every other `--json` output are the same bare arrays and objects with the same fields.
- **`asm capabilities --json`** says what this asm is and what each agent's store can do: `asm_version`, `json_contract_version`, `ir_version`, `index_schema_version`, and per agent `installed`, `store_root`, the adapter's `capabilities`, `hub_sync`, and for OpenCode `schema` (`v1`/`v2`) and whether 2.x is still migrating. `asm doctor --json` gains the same `schema` and `unmigrated` per agent, and a top-level `warnings` for a store that could not be read at all (one unreadable store no longer fails the whole report).
- **Failures are structured, and their exit statuses say which kind.** With `--json` (or `ASM_JSON=1`) a failure is one `{"error": {"code", "message", "candidates"?}}` object on stderr and nothing on stdout, for `not_found`, `ambiguous` (with the matching `agent:id`s), `live_session`, `unsupported`, `conflict`, `unreachable`, `io`, `usage`, `unauthorized` and `internal`. Exit statuses: 1 internal, 2 usage (as clap's own), 10 not found, 11 ambiguous, 12 session in use, 13 unsupported, 14 conflict, 15 unreachable, 16 io, 17 unauthorized. Without `--json` the text is unchanged; **the exit status of a failure is now finer than 1** (a not-found was 1 and is 10), so a script that tests for exactly `1` should test for non-zero. `control ... --wait` keeps its 0/1/2/3, `push`/`pull` keep exiting 1 after a report with failures, and `resume` keeps the agent's own status.
- **`--json` everywhere it was claimed.** `rename`, `move`, `archive`, `unarchive`, `delete`, `daemon start|stop|install|uninstall`, `control disable`, `import --show-toolmap`, `export -o` and `worktrees` (which now carries each worktree's sessions, beside the fields it had) print a document. `update --json` printed two documents when it installed; it is now one. A command that would ask a question (`delete`, `control move`) never prompts under `--json`: without `--yes` it is a `usage` error, `confirmation required: pass --yes`. A test walks the command tree and fails when a command is neither answering `--json` nor listed as exempt (`tui`, `serve`, `hub serve`, the foreground `daemon`, `resume`).
- **`asm messages REF`** prints a session's messages as one JSON object per line (`index`, `role`, `ts`, `parts`, from the IR), for every agent that exports, OpenCode 1.x and 2.x included, with `--role user|assistant|system|tool`, `--range A..B`, `--tail N`, `--text` (text parts only) and `--format json` for one array. **`asm children REF`** is the tree under a session as a flat array with `depth` and `parent`.
- **`--status live|idle|archived`** on `list`, `search` and `children`; **`--ids-only`** and **`-0`/`--null`** on the same three print bare ids for `xargs`.
- **`asm events`** (and `asm list --follow`) follows the stores: `{"event":"ready"}`, then `added`, `changed` and `removed` lines carrying the session, polled every `--interval` seconds (default 2, at least 0.5). It ends when its reader goes away.
- **Web API.** `GET /api/sessions/{agent}/{id}`, `GET /api/sessions/{agent}/{id}/messages`, and `GET /api/events` (server-sent events, the same payloads as `asm events`) are new; `/api/sessions` takes `agent`, `project`, `status` and `q`, and `/api/search` takes `status`; `/api/meta` adds `asm_version`, `json_contract_version` and `capabilities`. The new routes answer failures as `{"error": {"code", "message", "candidates"?}}` with 404, 409 (ambiguous, with candidates), 423 (session in use), 400, 501 and so on. The `/api/session/…` routes and `/api/sessions` keep `error` as the string the UI shows and gain `code` beside it, and a refused change now answers with the status its code says (423 for a session in use, 501 for what an agent cannot do and 400 for an invalid request, where all three were 409). `asm serve --token-file PATH` (or `ASM_WEB_TOKEN`) requires `Authorization: Bearer <token>` on `/api/`, compared in constant time and never logged; the bundled UI sends no token, so this is for programs. `asm serve --port 0` prints the port it actually bound.
- **A fix: `state` and `id` on sessions and search hits.** A session's `status` is an object (`{"state": "idle"}`) and a search hit's is a bare string, so the documented `jq` example `select(.status == "live")` never matched a session. Both stay as they were, and both now also carry `state` (`live`, `idle` or `archived`); a session also carries `id` (its native id). The example is `select(.state == "live")`.
- **asm-core** has crates.io metadata (description, repository, readme, keywords) and a crate-level overview of its entry points. It is not published, and nothing was curated or renamed.
- Verified on one Linux machine with fixture stores in a throwaway home for every agent the commands touch (OpenCode on both its 1.x and its 2.x schema), the built `asm`, and a real `asm serve` on a free port; not tried with the agents' own binaries, macOS, or a browser.

## 0.15.0 — 2026-10-20

- **Child sessions are first-class: grouped as a tree or folded away, counted, searchable and safe to delete.** OpenCode subagents (1.x and 2.x, at any depth), jcode's spawned sessions and Codex's spawned threads are *child sessions*. Everywhere that lists sessions now reads them and groups them under their parent instead of hiding them from the code that needs them. `asm list` still shows top-level sessions, with `+N` at the end of a row that has children folded behind it; `--include-children` lists them under their parent, indented, with a `PARENT` column; `--json` is the same flat array, each session carrying `parent`. The TUI folds children behind their parent (`z` unfolds the selected session, `T` shows every child as a tree); the web UI does the same with a chevron on the parent row and a **Show subagent sessions as a tree** switch, remembered in the browser. A child whose parent cannot be found (a Codex thread whose parent is gone, a filtered list) is a top-level row, not a missing one: `asm list` and the web UI now list such a session, which they used to hide, so `asm list --json` can contain a session with a non-null `parent`. (`GET /api/sessions` without `?all=true` still leaves out every child.) Claude Code has no child sessions (its subagents are inside the parent's transcript), Antigravity has none, and OpenCode 2.x's `fork_session_id` forks are siblings, so they list as ordinary sessions. See [Child sessions](/start/concepts/#child-sessions).
- **A child is addressed like any session.** `asm show`, `resume`, `rename`, `move`, `archive` and `delete` find a subagent session by its id or a unique prefix without `--include-children`, and so do the TUI and the web UI, which can act on one. A prefix that names one top-level session still names it when a child's id begins the same way; two children with nothing to choose between are ambiguous, as before.
- **Deleting a parent says what goes with it.** OpenCode deletes a session's whole tree. `asm delete` now names the sessions under it (their number, then up to five ids and titles, indented by depth), the TUI and the web UI give the number, and the report says how many were removed. For jcode, whose children stay, the prompt says they will be top-level sessions. `--yes` proceeds as before. A batch that includes a parent and its children deletes the children first, so they no longer fail as "not found" for having gone with the parent.
- **A search hit inside a child names its parent.** `asm search`, the TUI and the web UI mark it `↳ in <parent>`, and opening it lands on the child with whatever folded it unfolded. The search index records each session's parent, so it is rebuilt once, the first time this version opens it.
- **jcode's debug sessions are no longer tied to `--include-children`.** They share jcode's flag for sessions it hides, but they are not children, and a listing that includes children to group them would have shown them as top-level sessions. They are now never listed: `--include-children` no longer shows them. Their ids still work with `asm show`, `delete` and the rest, and the search index and the hub's exact-id lookups still see them, as before.
- **Project counts, store health and the hub views are unchanged by all of this**: a project counts the sessions an agent's own picker lists, `asm doctor` and the TUI's `D` count the same, and the hub views and `push --all` leave subagents out (a subagent travels in its parent's bundle). A pushed hub manifest now records the session's `parent`, and so does an archived session's manifest (so an archived child is still a child in the index); manifests and archives written earlier have none and read as top-level sessions.
- Verified with fixtures for every agent that has children (OpenCode 1.x on a three-level tree and 2.x on its schema, jcode, Codex with a spawn edge and an orphan), the built `asm` against stores in a throwaway home, and the web UI's markup rendered server-side; not tried in a real terminal, a browser, or with the agents' own binaries.

## 0.14.1 — 2026-10-20

- **The hub no longer asks you to pull a session that is ahead of the hub's.** A session both sides have but this machine had no sync record for (a first run, or a deleted `hub-state.json`) showed **Differs** with *Pull it*, whichever copy was newer: one report was a local session two weeks newer than a smaller hub copy. A listing now reads the hub's copy of the one file the agent compares (up to 2 MiB; the web **Compare** button up to 256 MiB) and asks what a pull would, read-only, for every agent including OpenCode 1.x and 2.x: the row becomes **Needs push** (and the hub's revision is recorded as the base, as a pull does for a copy that is ahead, so the push is accepted), **Needs pull**, **Diverged** or **Synced**. Sizes and timestamps still decide nothing; where the content cannot be read the row stays **Differs**, with an advisory *looks newer here / on the hub* in its hint when time and size agree, and no state or action changes with it. `compare` in `asm remote list --json` and `POST /api/hub/compare` gain `relation` and `lean`. Verified on one Linux machine with real `asm hub serve` hubs and fixtures; OpenCode 2.x against 2.0.25 only.
- **Indexing no longer slows down as the index grows, and a search no longer waits for it.** Re-reading a session deleted its old rows by scanning the whole search index, so the cost of a build grew with every session already in it: a first build over 2,700 sessions (494 MB of transcripts) took almost five minutes, and a refresh after twenty sessions changed took five seconds. The rows are now found by the range recorded for each session, which takes 12 seconds and under a second. A search also used to wait behind the indexer's write (opening the index tuned a table on every open): in the TUI's `/` search and in `asm search --no-refresh` that was five seconds, the busy timeout, whenever a build was running, and now it is not noticeable. The TUI's rescan lists your sessions once rather than three times, so previewing a session or searching while it runs no longer queues behind it, and it remembers what git said about each project directory for 30 seconds (`R` asks again). The search index is rebuilt once, the first time this version opens it. Measured on generated stores on one Linux machine, with the agents' own stores untouched.

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
