---
title: Concepts
description: The few ideas the rest of the documentation relies on.
---

## Session

A **session** is one conversation with one agent, stored by that agent in its own format. asm never keeps its own copy: the list is read live from each agent's store, and every action goes back through that agent's own mechanism. A session's identity is the pair **(agent, native id)**; the short ids in tables are unique prefixes of the native id.

## Agent

One of Claude Code, OpenCode, jcode, Codex and Antigravity. What asm can do with an agent's sessions differs — see the [support matrix](/reference/agents/) — and the UIs grey out what an agent cannot do instead of failing when you try it.

## Child sessions

Some agents start a session from inside another one: an OpenCode subagent (which can start subagents of its own), a session jcode spawns, a Codex thread spawned from another. asm calls these **child sessions**. A child is a session in its own right, with its own id, that you can show, resume, rename, search and delete; it is *grouped* under the session it belongs to, and by default folded behind it.

| Agent | Child sessions |
|---|---|
| Claude Code | None. A subagent's work is **inside the parent's transcript**, as a nested run attached to the turn that spawned it, so there is nothing to group. |
| OpenCode (1.x and 2.x) | `parent_id`, at any depth. 2.x's `fork_session_id` forks are *siblings*, not children, and list as ordinary sessions. |
| jcode | `parent_id`. jcode's own debug/test sessions are not children: they are never listed, and `--include-children` does not show them. |
| Codex | Spawn edges (`thread_spawn_edges` in `state_5.sqlite`). A thread whose parent is not in the store is listed as a top-level session. |
| Antigravity | None. |

A child whose parent cannot be found (deleted, archived on its own, filtered out) is shown as a top-level session rather than disappearing.

- **`asm list`** shows top-level sessions; one that has children ends its row with `+N` in the status column. `--include-children` lists them under it, indented, with a `PARENT` column. `--json` stays a flat array of sessions, each with a `parent` (null for a top-level session).
- **The [TUI](/guides/tui/#subagent-sessions)** folds children behind their parent with the same `+N`; `z` unfolds the selected session and `T` shows every child as a tree.
- **The [web UI](/guides/web-ui/#subagent-sessions)** does the same, with a chevron on the parent row and a switch to show them as a tree.
- **Addressing:** a child is found by its id or a unique prefix like any session, so `asm show`, `resume`, `rename`, `delete` and the rest work on one without `--include-children`.
- **Search** returns hits inside a child, marked `↳ in <parent>`, and opening one lands on the child with its parent unfolded.
- **Delete:** OpenCode deletes a session's whole tree with it, so `asm delete`, the TUI and the web UI say how many sessions go with the one you chose before they ask; the backup holds all of them. jcode deletes the one session and its children stay, as top-level sessions. Deleting a parent and its children in one batch is fine: the children go first.
- **Hub:** an OpenCode subagent travels in its parent's bundle, so `push --all`, `remote list` and the hub views in the TUI and the web UI leave children out (`--include-children` puts them back in the first two). A pushed child's manifest records its `parent`.

## Project

A **project is a git repository**, not a directory. Every worktree and every subdirectory of a repository is the same project; sessions from different agents in the same repository share one. See [Projects](/concepts/projects/).

## Live, idle, archived

- **live** — an agent process currently owns the session. asm will not mutate it.
- **idle** — nothing owns it.
- **archived** — moved out of the agent's store into asm's archive (or flagged, for OpenCode). Still searchable.

## Session IR

The agent-neutral JSON form a session converts through for `export` and `import`. It models what converts cleanly and carries everything else in per-agent `extensions`, so a round trip can restore what a different agent could not hold. See the [IR schema](/reference/ir-schema/).

## Hub, machine, push, pull

A **hub** is an archive one machine serves; other **machines** join it with a token and **push** their sessions to it. Any machine can **pull** a session another machine pushed. What crosses is the session's own files under its original id, so a pulled session is the same session, not a copy. See [Multiple machines](/hub/overview/).

## Sync states

Everywhere a session is compared with the hub, one vocabulary is used — in the CLI, the TUI and the web UI, taken from the same code:

| State | Meaning | Next step |
|---|---|---|
| **Synced** | The hub has exactly this copy. | — |
| **Needs push** | Changed here since the last sync. | push |
| **Not on hub** | The hub does not have it yet. | push |
| **Needs pull** | Another machine pushed a newer copy. | pull |
| **New on hub** | On the hub, not on this machine. | pull |
| **Not compared** | On both, but this machine has never compared the two copies, and the content could not be read to tell which is ahead (a large copy, or the hub did not answer). Shown as **Differs** once the copies are known to differ. | pull to compare: identical copies become Synced, a hub copy that extends yours is applied, one that moved on both sides is reported as diverged. A small copy is read for you, and the row is then Synced, Needs push, Needs pull or Diverged: see [How sync decides](/hub/sync/#comparing-without-pulling) |
| **Diverged** | Continued on both sides. | `asm push --force` picks this copy |

## Daemon

A background process that pushes sessions to the hub as they change. It only pushes: it never writes into an agent's store, unless you turned on [remote control](/hub/control/). See [The daemon](/hub/daemon/).
