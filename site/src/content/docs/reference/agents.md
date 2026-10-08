---
title: Agent support
description: What asm can do with each agent's sessions, where they live, and why the gaps are gaps.
---

## Matrix

| | Claude Code | OpenCode | jcode | Codex | Antigravity |
|---|---|---|---|---|---|
| List, show, search, projects | yes | yes | yes | yes | yes |
| Liveness | yes | yes | yes | no | no |
| Resume | yes | yes | yes | yes | yes |
| Export to Session IR | yes | yes | yes | yes | yes |
| Rename | yes | yes | yes | no | no |
| Archive / unarchive | yes | yes (1.x only) | yes | no | no |
| Delete | yes | yes | yes | no | no |
| Move to another directory | yes | yes (1.x only) | no | no | no |
| Import **from** (source) | yes | yes | yes | yes | yes |
| Import **into** (target) | yes | yes | no | no | no |
| Send a message into a session | yes | yes (1.x only) | no | yes | yes |
| Back up to a hub | yes | yes | yes | yes | yes |
| Restore from a hub | yes | yes | yes | same path | yes |
| Read/import verified against | 2.1.234 | 1.17.18; 2.0.25 (read, rename, delete; import accepted, resume not exercised) | 0.78.0 | 0.151.0 | 1.1.16 |
| Hub restore verified against | 2.1.278 | 1.18.31; 2.0.25 (same major version only) | 0.83.0 | 0.151.0 | 1.1.22 |

`asm doctor --json` reports each agent's capabilities, and the web UI greys out what an agent cannot do, so the limits are visible rather than discovered by error. Importing a session into an agent warns when the installed version has drifted from the verified one (`asm doctor` does not check this). OpenCode is verified once per major version: an installed 2.x is compared with 2.0.25, a 1.x with 1.17.18. For 2.0.25 "verified" means the real binary accepted the import and the session read back from its store; resuming an imported session was not exercised.

## Where each agent keeps its sessions

| Agent | Store | Override |
|---|---|---|
| Claude Code | `~/.claude/projects/<encoded-dir>/<id>.jsonl` plus sidecars | `CLAUDE_CONFIG_DIR` |
| OpenCode | `~/.local/share/opencode/opencode.db` (SQLite) | `XDG_DATA_HOME` |
| jcode | `~/.jcode/sessions/<id>.json` | `JCODE_HOME` |
| Codex | `~/.codex/state_5.sqlite` (the `threads` table) and `sessions/` rollouts | `CODEX_HOME` |
| Antigravity | `~/.gemini/antigravity-cli/` (`conversations/`, `brain/`, `cache/`) | `ASM_ANTIGRAVITY_ROOT` |

asm reads both schemas of OpenCode's database, told apart each time it looks: 1.x (`session`, `message`, `part`) and 2.x (`session_v2`, `session_message`). A database with neither is skipped as if the agent were not installed.

**OpenCode 2.x.** Listing, `show`, search, `export`, resume (`opencode -s <id>`), the TUI/web transcripts, rename, delete, import into OpenCode and the hub all work; archive, unarchive, move and `send` do not.

| On a 2.x store | |
|---|---|
| Rename, delete, import into OpenCode | Done by running `opencode` (`api session.update`, `session delete`, `session import`), never by editing the database: 2.x rebuilds its tables from an event log that a running service owns. |
| Refused while a turn runs | Any change to a session that a turn has claimed, or that has input queued, is refused, saying which: a turn that is running, work that is queued, or "an unfinished turn from an OpenCode that is no longer running; start OpenCode once to finish or abandon it, then retry". A delete checks the session's subagents too. |
| Delete | Writes a backup first, and makes sure it is on disk (private to you, everything stored, even an abandoned partial turn): one `opencode session import`-able JSON document per session of the tree (`00-<id>.json`, the root first) and a `manifest.json` naming each session's directory, under `<data>/backups/opencode/<id>/<timestamp>/`. To restore, run `opencode session import <file> --directory <directory>` for each, in order. If a session appeared or a turn started while the backup was being made, nothing is deleted. |
| Archive, unarchive | `OpenCode 2.x has no archive`: 2.0.25 has no way to archive a session, in its CLI, its API or its events (a session archived in 1.x lists as an ordinary one). A remote-control **move** away from such a machine is refused when it is planned: the machine reports that it cannot archive OpenCode, and a send does the job. |
| Move to another directory | Refused: the only way is a request queued for a persistent server, and a one-shot run would leave the session claimed with nothing to run it. |
| `send` | Not offered: the event vocabulary of `run --format json` has not been checked against 2.x. |
| Hub push and pull | Work between machines on the same major version. A 1.x bundle is database rows and a 2.x one is session documents: pulling one onto the other is refused in words. A pulled session carries its permissions, metadata, agent and model exactly as pushed, so pull only from machines you control. See [Restore, per agent](/hub/restore/). |
| Sessions OpenCode has not migrated yet | OpenCode 2.x copies 1.x sessions into its new tables in the background and leaves the old ones behind. While some are not copied, `asm doctor` and `asm list` say how many ("N sessions in this OpenCode database have not been migrated by OpenCode 2.x yet; start OpenCode once"), asm still lists them (read from the old tables, read-only; they cannot be pushed to a hub), and it refuses every change to the store (rename, delete, import, a hub pull) until they are migrated. A database asm cannot inspect at that moment is never changed either. |

asm runs `opencode` for these with `--standalone` (a private server for that one request, which leaves nothing running) unless `$XDG_STATE_HOME/opencode/service.json` names a service that is really up, in which case it talks to that one. "Really up" is not just a live pid: the url in the file must answer `/api/info` on loopback with that pid, as OpenCode's own client requires, so a stale file whose pid another process has since taken is ignored. It never runs a flagless command without such a service: that would start a background service, whose boot resumes sessions an earlier service left claimed, which is a model call nobody asked for. A service that dies in the instant between asm checking and asking is the one gap in that. Only `service.json` is known: OpenCode builds of a custom release channel register in `service-<channel>.json`, which asm does not read, so for them asm always stands alone and shows their running turns as idle.

Two smaller limits: a 2.x session has no git branch in the listing, and a session is reported as running only while the OpenCode background service that holds its turn is up (a turn run with `opencode --standalone` shows as idle). See [OpenCode 2.x](/reference/agent-formats/#opencode-2x) for the layout and the formats.

## Why the gaps are gaps

### Codex is read-only

Its metadata lives in `state_5.sqlite`, a schema 48 sqlx migrations deep and still gaining columns, and Codex ships no `rename`, `archive` or `import` command to write through instead — so every mutation would be a raw write to a moving target that its own picker might then disagree with. Reading is safe and complete: the `threads` table drives the listing, and asm additionally sweeps the `sessions/` tree for rollouts that have no `threads` row, which Codex hides until you resume them by id.

Hub restore is the exception — it places a rollout and the `threads` row Codex itself would write, only into an existing state database, and only where the session's working directory exists.

### Antigravity

Antigravity is what Gemini CLI became — Google now refuses Gemini Code Assist for individuals outright, telling you to migrate — and its store is different in kind: one SQLite database per conversation, with every step held as a protobuf blob against a schema Google does not publish. asm reads the conversation instead from the JSONL rendering Antigravity writes alongside it, under `brain/<id>/.system_generated/logs/`. Same steps, same author, no guessed field numbers.

:::caution[A real gap]
**An Antigravity conversation does not record its project directory.** Neither the conversation database nor the summaries index keeps one (`workspace_uris` is empty), and the only mapping on disk, `cache/last_conversations.json`, remembers just the most recent conversation per directory. Sessions it does not cover are listed with no project rather than a guessed one.
:::

### jcode: no move, no import-into

Its whole session — metadata *and* the entire conversation — is one JSON document, and jcode ships no command for either job. Moving a session means editing `working_dir` inside that document, and importing means writing a whole one. Rewriting another tool's file to change one field, with no sanctioned path and no way to check the result, is not a trade this project makes. Everything else goes through jcode's own `session rename` or moves whole files without touching their contents.

### jcode: no reply

Its stream format has not been captured from a real run, and a normalizer written from a flag name is a guess, not support.
