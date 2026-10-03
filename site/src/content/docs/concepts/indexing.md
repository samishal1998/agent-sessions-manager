---
title: Indexing and scale
description: How search stays fast on a machine with thousands of sessions.
---

Nothing waits for the whole store. Measured on a synthetic store of 2,600 sessions (1,500 Claude, 400 OpenCode, 400 Codex, 300 jcode; 130 MB), release build, warm cache:

| | |
|---|---|
| first sessions in the UI | **~30 ms** to the browser (one project directory), **~270 ms** to the first painted row |
| whole list | 0.23 s (`asm list`); the web UI has all 2,600 counted at ~0.33 s |
| first sessions searchable | **0.22 s** — the index commits early, then in bigger batches |
| whole index, cold | ~10 s, on a thread of its own, with progress on screen |
| refresh when nothing changed | 0.33 s |

Both UIs fill their list as the stores give sessions up and say so while they are reading; an empty list says which empty it is. Indexing runs on its own thread, so a search during the first build finds what has been read so far rather than waiting.

## How the index works

- **What it already knows** is loaded in one query rather than two per session.
- **Transcripts are read on several threads**, and writes are batched — bounded by **bytes**, not by session count, because one machine's session is a hundred kilobytes and another's is a hundred megabytes.
- A session is stamped with the fingerprint it had **before** it was read, so a turn written while asm reads it is picked up next time instead of being taken for already indexed.

## Fingerprints are per-agent

The naive choice is wrong for OpenCode: its `session.time_updated` lags behind its own message rows, so keying on it would silently lose streamed tool output. File-backed sessions key on size and mtime; row-backed ones on the message table's own count and high-water mark. The same fingerprints drive the [daemon](/hub/daemon/) and push's "has this changed?" pre-check.

## Disposable

The index is derived data: anything unreadable, or written by a different schema version, is rebuilt rather than migrated. `asm index` also reclaims space after re-extraction (FTS5's `optimize` restructures but does not return pages to the filesystem; `VACUUM` does).
