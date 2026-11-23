---
title: How sync decides
description: Revisions, the last-synced base, and why conflicts are refused rather than resolved.
---

## Identity

A session is the same session when its **agent and native id** match — never because of its path or timestamps. The path only decides *where a pull lands*. Two separately started sessions in one repository are never merged, and timestamps are shown but never decide anything: clocks differ between machines, and a rename does not move a session's timestamp.

## Revisions

Each push uploads the session's files as content-addressed **blobs** (a file the hub already has is not uploaded again — a one-line change to a transcript uploads one blob), then a **revision** naming them, then moves the session's **head**. Each revision records the revision it started from (`parent_rev`).

```mermaid
sequenceDiagram
  participant M as Machine
  participant H as Hub
  M->>H: which blobs are missing?
  H-->>M: [sha, sha]
  M->>H: PUT blob (only those)
  M->>H: PUT revision (parent = last-synced head)
  alt parent is still the head
    H-->>M: 201, head moves
  else another machine pushed first
    H-->>M: 409 diverged (current head)
  end
```

Blobs go first, the revision second and the head last, so a crash mid-push leaves only unreferenced blobs. The hub refuses a revision whose parent is not the current head; that compare-and-swap is the entire concurrency story.

## The base

Each machine remembers, per session, the **last revision it agreed on with the hub** (`hub-state.json`). That *base* is how asm knows which side moved:

| Local vs. base | Hub head vs. base | Result |
|---|---|---|
| unchanged | unchanged | **Synced** |
| changed | unchanged | **Needs push** |
| unchanged | moved | **Needs pull** |
| changed | moved | **Diverged** |
| no record | exists | **Not compared**, until [compared](#comparing-without-pulling): then one of the above |

## Comparing without pulling

A session both sides have, but that this machine has never compared (there is no record of syncing it: a first run on a machine that already had the session, a deleted `hub-state.json`), starts as **Not compared**. asm then looks at it for you, in two steps, without installing anything.

**1. Identity.** It reads and hashes *this* machine's copy and compares the conversation's identity with the one the hub recorded when it was pushed. If they are equal the session is recorded as synced and shows **Synced**. This is the usual outcome for a session another machine pushed and this one already had.

**2. Content, when they differ.** Different identities do not say which side is ahead, and neither do sizes or timestamps: the two sides' sizes cover different files, and each machine's clock is its own. So asm downloads the hub's copy of the one file the agent compares — the transcript, rollout, snapshot or rows — and asks it the question a pull would ask, without writing anything: is this machine's copy a *prefix* of the hub's, is the hub's a prefix of this one, or neither? The answer is the row's state, exactly as if a sync record existed:

| Content shows | Row | What it records |
|---|---|---|
| the same conversation | **Synced** | the session as synced |
| the hub's copy extends this one | **Needs pull** | nothing |
| this copy extends the hub's | **Needs push** | the hub's revision as the base, as a pull would for a copy that is ahead, so the push that follows is accepted |
| each has something the other lacks | **Diverged** | nothing |

The comparison is the pull's own, per agent (Claude Code and Codex by transcript bytes, jcode by its message list, Antigravity by its transcript, Pi and oh-my-pi by their session file without omp's title slot, OpenCode 1.x by its rows and 2.x by its messages and session fields), so what a row says is what `asm pull` would find. It is read-only for every agent, and it does not run `opencode`. An oh-my-pi rename is not a divergence, and a pull of a Pi or oh-my-pi session is still [refused](/hub/restore/).

Reading the hub's copy is bounded. A listing reads it only when the hub's copy is at most **2 MiB**, and only within the 25 sessions each listing compares; the web UI's **Compare** button on one row reads up to **256 MiB**. Anything it cannot answer — a larger copy, an agent with nothing to compare by (an Antigravity conversation with no transcript, a jcode session whose latest turns are not yet in its snapshot, a bundle from the other OpenCode generation), a hub that stops answering — stays **Differs**, never an error, and a listing stops asking a hub that has failed to answer. What it works out is remembered until either side changes, and a result from a listing that did not read the hub's copy does not stand in for the larger Compare.

### The advisory lean

When the content could not be read, a row that still says **Differs** may carry a *lean*: this machine's copy looks newer when it was updated later than the hub's *and* is not smaller; the hub's copy looks newer the other way round; and when the time and the size point different ways there is no lean. It is a guess from two clocks and two sizes that cover different files, so it appears only in the row's hint and in `--json` (`compare.lean`), it never changes the row's state or what the row offers to do — pulling it is still how to find out, and nothing changes if it turns out this copy is ahead — and it is never used for any decision, a push or a pull.

Each listing compares at most 25 sessions, and remembers what it found, so a machine with thousands of uncompared sessions settles over a few refreshes instead of stalling the first. In the web UI's [Hub view](/guides/web-ui/#the-hub-view) a row's **Compare** button does one immediately.

## Recomparing

A session this machine *has* a record for is not read on every listing: its state comes from [the base](#the-base), and "changed here" means its size or modification time moved. That is cheap, and almost always right, but a record can be wrong:

- the file was **touched without changing** (a backup restored it, a sync tool rewrote it, an editor saved it unchanged): **Needs push**, though the hub has exactly this copy;
- another machine **pushed the same conversation again**: **Needs pull**, though there is nothing to pull;
- both of those at once, or a touched file under a hub copy that went on: **Diverged**, though the hub's copy simply extends this one.

**Recompare** answers from content instead, whatever the record says: the same two steps as [comparing without pulling](#comparing-without-pulling), reading the hub's copy up to 256 MiB, and nothing installed. Its answer replaces the record:

| Content shows | Afterwards |
|---|---|
| the same conversation | **Synced**, recorded against the hub's current head |
| this copy extends the hub's | **Needs push**, with the hub's head as the base, so the push is accepted |
| the hub's copy extends this one | **Needs pull**: the record is dropped, and the pull compares both copies itself |
| each has something the other lacks | **Diverged**: the record is dropped, so a plain push is refused rather than built on a stale base |
| unknown (the hub's copy could not be read) | nothing changes, and the session is reported as skipped |

With the record dropped, a later listing works those two out again the way it does for any [uncompared session](#comparing-without-pulling): the web UI remembers the answer while it runs, but a new `asm remote list` reads the hub's copy again only up to 2 MiB, so a larger one shows **Differs** until it is pulled.

Recompare from the web UI (**Recompare all** on the Hub view, **Compare again** in a row's details, or **Recompare** on sessions ticked in the list), or from a terminal:

```sh
asm remote recompare <id> [<id>…]   # these sessions
asm remote recompare --all          # every session here and on the hub that the filters match
```

A session that is only on one side has nothing to compare, and is reported as skipped when named.

## Diverged

Both machines continued the same session. Nothing is merged, and a pull of a diverged session installs nothing — it reports that it diverged. You choose which side wins:

```sh
asm push 7f3a1c88 --force     # this copy becomes the head
```

`--force` makes a new revision; the replaced copy stays on the hub as an earlier revision, so nothing is lost. To take the *other* machine's copy instead, delete the local one (`asm delete` backs it up first) and `asm pull` it.

## Pull obeys the safety model

A pull refuses a live session, never makes a second copy of an id, and only ever appends to a transcript that is a prefix of the hub's. See [Restore, per agent](/hub/restore/) for how each agent's pull works.

## What is never synced

Machine-local state stays out of the bundle: Claude's `sessions/<pid>.json`, background `jobs/`, per-project `memory/` and `~/.claude.json`; OpenCode's lock directory (1.x) and, on 2.x, a session's turn claim, its retry count and its queued input; jcode's `active_pids/`. Claude's `relocated` marker is machine-local state inside a shared byte stream, so it is stripped when comparing and re-derived on install.
