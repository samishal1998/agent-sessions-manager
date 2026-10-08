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
| Archive / unarchive | yes | yes | yes | no | no |
| Delete | yes | yes | yes | no | no |
| Move to another directory | yes | yes | no | no | no |
| Import **from** (source) | yes | yes | yes | yes | yes |
| Import **into** (target) | yes | yes | no | no | no |
| Send a message into a session | yes | yes | no | yes | yes |
| Back up to a hub | yes | yes | yes | yes | yes |
| Restore from a hub | yes | yes | yes | same path | yes |
| Read/import verified against | 2.1.234 | 1.17.18 | 0.78.0 | 0.151.0 | 1.1.16 |
| Hub restore verified against | 2.1.278 | 1.18.31 | 0.83.0 | 0.151.0 | 1.1.22 |

`asm doctor --json` reports each agent's capabilities, and the web UI greys out what an agent cannot do, so the limits are visible rather than discovered by error. `asm doctor` also warns when an installed version has drifted from the verified one.

## Where each agent keeps its sessions

| Agent | Store | Override |
|---|---|---|
| Claude Code | `~/.claude/projects/<encoded-dir>/<id>.jsonl` plus sidecars | `CLAUDE_CONFIG_DIR` |
| OpenCode | `~/.local/share/opencode/opencode.db` (SQLite) | `XDG_DATA_HOME` |
| jcode | `~/.jcode/sessions/<id>.json` | `JCODE_HOME` |
| Codex | `~/.codex/state_5.sqlite` (the `threads` table) and `sessions/` rollouts | `CODEX_HOME` |
| Antigravity | `~/.gemini/antigravity-cli/` (`conversations/`, `brain/`, `cache/`) | `ASM_ANTIGRAVITY_ROOT` |

OpenCode's database is read with the schema of OpenCode 1.x (`session`, `message`, `part`). OpenCode 2.x keeps sessions in `session_v2`/`session_message`, which asm does not read yet: such a store is skipped as if the agent were not installed.

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
