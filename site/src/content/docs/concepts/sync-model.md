---
title: The sync model
description: The design decisions behind the hub, and what was deliberately left out.
---

The mechanics are on [How sync decides](/hub/sync/); this page is why.

## Hub-and-spoke, with a peer shortcut

Only the hub must be reachable. A laptop behind a NAT, a desktop that sleeps and a server all work, because every machine talks only to the hub. "Discovery" is the hub's listing of machines and sessions.

[Peer mode](/hub/peers/) is the one exception, and it is deliberately smaller than the hub. `asm push --to` and `asm pull --from` move one session straight between two machines that already reach each other (ssh, or an `asm hub serve --peer`). The receiving end does the same comparison a pull does — identical, one copy extends the other, or diverged — but **on the two copies in front of it, with no history**: there are no revisions, no last-synced base, no `--force`, and a divergence is refused outright because nothing stored anywhere could keep the losing copy.

That is why the hub still wins past two machines: with three, "which copy is newest" needs a place both others agreed with, which is what the base and the revisions are. It also wins whenever the two machines cannot see each other at the same moment — a laptop behind a NAT, a desktop asleep when you want its session — because the hub is the one end that is always there. Peer mode is for the case where neither of those problems exists and running a hub would be the only reason to run one.

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
