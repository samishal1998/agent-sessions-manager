---
title: Concepts
description: The few ideas the rest of the documentation relies on.
---

## Session

A **session** is one conversation with one agent, stored by that agent in its own format. asm never keeps its own copy: the list is read live from each agent's store, and every action goes back through that agent's own mechanism. A session's identity is the pair **(agent, native id)**; the short ids in tables are unique prefixes of the native id.

## Agent

One of Claude Code, OpenCode, jcode, Codex and Antigravity. What asm can do with an agent's sessions differs — see the [support matrix](/reference/agents/) — and the UIs grey out what an agent cannot do instead of failing when you try it.

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
| **Check sync** | On both, but this machine has no record of syncing it. | pull to compare |
| **Diverged** | Continued on both sides. | `asm push --force` picks this copy |

## Daemon

A background process that pushes sessions to the hub as they change. It only pushes: it never writes into an agent's store. See [The daemon](/hub/daemon/).
