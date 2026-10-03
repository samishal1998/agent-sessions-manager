---
title: Search
description: Full-text search over every message of every session, and how the index stays fast.
---

```sh
asm search "path encoder"
asm search --agent opencode jsonrpc
asm search --limit 50 "tower layer"
asm index --stats        # report on the index without refreshing it
```

`asm search` runs SQLite FTS5 over every message of every session, in an index kept in asm's own data directory — the agents' stores are never written to. Words are AND-ed; quote a phrase. The TUI's `s` and the web UI's transcript search box use the same index.

## Freshness

The index is **incremental**: each session carries an opaque content fingerprint, and only sessions whose fingerprint moved are re-extracted. `asm search` refreshes by default and prints how far it has got; `--no-refresh` skips it. Archived sessions stay searchable even though they have left their agent's store — results mark them `(archived)` so it is clear they need restoring before they can be resumed.

## What is indexed

- Every message, including **subagent transcripts** — in delegating sessions they are the majority of the searchable text.
- Tool *inputs* in full (commands, paths, patterns); tool *outputs* only in part, since they dominate transcript bulk.

## On a machine with thousands of sessions

Nothing waits for the whole store. See [Indexing](/concepts/indexing/) for the measurements and the design.

## Known limitations

- Deleting a session's rows from the FTS table is a scan of that table, so a full rebuild is linear in sessions × messages. At personal scale (seconds) this is fine.
- OpenCode staleness is judged by the session's message count and high-water mark; if OpenCode ever writes a message without either moving, that session would look unchanged.

The index is disposable — delete `index/sessions.db` and it is rebuilt.
