---
title: The sync model
description: The design decisions behind the hub, and what was deliberately left out.
---

The mechanics are on [How sync decides](/hub/sync/); this page is why.

## Hub-and-spoke, not peer-to-peer

Only the hub must be reachable. A laptop behind a NAT, a desktop that sleeps and a server all work, because every machine talks only to the hub. "Discovery" is the hub's listing of machines and sessions.

## The hub is an archive

It never runs an agent and never materializes a session. A headless box with disk can be one. The stored sessions are visible with `asm remote list`, not `asm list`.

## Same agent, same id

A session's identity across machines is `(agent, native id)`. Sync moves the session's **native** files under the **original id** — it does not go through the [import engine](/guides/import/), which re-derives ids and refuses same-agent import.

## Conflicts are refused, never resolved

Divergence is a `409`, not a merge. Two reasons: there is no correct automatic merge of two continuations of an LLM conversation, and the one signal that might pick a winner — a timestamp — is unreliable (asm's own `rename` and `move` append records with no timestamp, and clocks differ). The user picks with `asm push --force`; the other copy remains as a revision.

## The daemon only pushes

Writing into an agent's store unattended would break the safety model, so the daemon is read-and-upload. Pulls are explicit — by you at the keyboard, or, on a machine where you ran `asm control enable`, by a command you sent with the commands token ([Remote control](/hub/control/)).

## Machine-local state stays machine-local

Anything that describes one machine (Claude's `relocated` marker, `sessions/<pid>.json`, jcode's `last_pid`, lock directories) is stripped from what is hashed and compared, and re-derived on install.

## Left out on purpose

- **Garbage collection** of old revisions and blobs — deferred until growth is observed.
- **Sharing between people** — one hub is one person's.
- **Automatic pulls** — see above.
- **A file watcher** — the daemon polls a cheap fingerprint every interval instead.
