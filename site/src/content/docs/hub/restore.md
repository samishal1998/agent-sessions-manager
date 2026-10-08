---
title: Restore, per agent
description: What pulling a session does for each agent, and what was verified.
---

Every agent's sessions are **backed up** to a hub; restore works for all five. Each agent's pull respects that agent's own storage model.

| Agent | What a pull does | Where it lands |
|---|---|---|
| **Claude Code** | A transcript is append-only, so a newer copy is **appended** to an older one without rewriting a byte; sidecars are placed first, the transcript last. | Same place relative to `$HOME`, or `--project-dir`. A `relocated` marker is appended when the working directory differs. |
| **OpenCode** | A session is rows (with subagent sessions, todos, queued inputs). An older local copy is replaced only when **every local row is also in the hub's copy**, after a backup. | Filed under this machine's project; the pulling machine need not have used OpenCode before. |
| **OpenCode 2.x** | A session is a document per session (`opencode session export` format) for the session and each subagent. There is no fast-forward: an older local copy is **replaced** (backed up as the same documents, deleted, then imported), when it has not changed here since this machine last synced, or when every message and field here is also in the hub's copy. Never while a turn is running or queued. When only the titles differ it is renamed in place instead (the old titles are saved). | The whole tree is filed under one directory on this machine (`--project-dir`, else the pusher's path resolved against your home). Only between machines on the same OpenCode major version. |
| **jcode** | A snapshot is rewritten whole every turn; an older copy is replaced when its messages are a **prefix** of the hub's, after a backup. | Under this machine's directory; resumed by id. |
| **Codex** | Append-only like Claude, but Codex writes the absolute working directory into the conversation. | **Only where that directory exists** — the same path on both machines. |
| **Antigravity** | A database agy rewrites whole: an older copy is replaced when its transcript is a prefix of the hub's, after a backup. | Nothing in it names a directory; it resumes wherever `agy --conversation <id>` is run. |

## Verified against real installs

Each time between two separate homes (every store, config and asm's own state apart) on one host:

- **Claude Code 2.1.278** — pulled onto another machine at a different path; `claude --resume` continued it there with its conversation intact.
- **OpenCode 1.18.31** — pulled onto a machine that had never run OpenCode, at a different path; `opencode session list` showed it and `opencode run -s` recalled the conversation.
- **OpenCode 2.0.25** — between two separate homes with one fake OpenCode data directory each: a session with a subagent, pushed from one and pulled onto the other (`opencode session export` of both agree on every message), renamed and pulled again (applied as a rename on the other home), a continued copy pulled as a replaced one with a backup, and shown by the hub's admin page. No model was asked: resume itself was not exercised.
- **Codex 0.151.0** — a session Codex made, pulled into the same directory on the other home; `codex exec resume` recalled the conversation, and its continuation came back as a fast-forward that Codex on the first home saw.
- **Antigravity 1.1.22** — a conversation agy made, pulled onto the other home; `agy --conversation` recalled it from a directory the first home never used, and its continuation came back as a replaced copy with a backup.
- **jcode 0.83.0** — jcode's resume by id loaded the pulled session and appended the next turn to it, with no second copy. A model reply was not reached — jcode would not use a login copied into a test home — so that last step is unverified.

:::note
"Replaced after a backup" always means a copy under `<data>/backups/` first. A replace never happens when the local copy has something the hub's lacks — that is a divergence, and is refused.
:::

## `pull --all`

```sh
asm pull --all                    # everything on the hub the filters match
asm pull --all --agent codex
asm pull --all --project ~/code/mercury
```

Each session lands at its own place on this machine. One that cannot (a Codex session whose directory does not exist here) is reported per session and the rest still go.
