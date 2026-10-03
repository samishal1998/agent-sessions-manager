---
title: Organize sessions
description: Rename, move, archive, delete and export — what each does per agent, and what asm refuses.
---

All of these refuse a **live** session, and all go through each agent's own mechanism where one exists.

| Verb | Claude Code | OpenCode | jcode | Codex | Antigravity |
|---|---|---|---|---|---|
| `rename` | appends a `custom-title` record | native | `jcode session rename` | — | — |
| `move <ref> <dir>` | relocates the transcript + sidecars; writes a `relocated` marker | native project change | — | — | — |
| `archive` / `unarchive` | moved into asm's archive store | native flag | moved into asm's archive store | — | — |
| `delete` | backed up, then removed | backed up, then removed | backed up, then removed | — | — |
| `export` | yes | yes | yes | yes | yes |

Dashes are deliberate, not oversights: see [Agent support](/reference/agents/) for why each is missing.

## Rename

```sh
asm rename 4c93a826 "Retry middleware on tower::Layer"
```

## Move

Use this after you renamed or moved a project directory and the agent's picker no longer finds the session.

```sh
asm move 4c93a826 ~/projects/renamed-dir
```

For Claude Code, asm *moves* rather than copies, since Claude's cross-project `--resume` hard-fails when an id exists in two project directories.

## Archive

```sh
asm archive 4c93a826
asm unarchive 4c93a826
```

Claude and jcode sessions leave their store for `<data>/archive/<agent>/<id>/` (a `manifest.json` plus `native/`); OpenCode sessions get its native archived flag and stay listed. Archived sessions remain searchable. `asm sync init` turns the archive into a git repository you can version and push to a remote you choose — asm leaves that transport to you.

## Delete

```sh
asm delete 4c93a826 --yes
```

Everything affected is copied to `<data>/backups/<agent>/<id>/<timestamp>/` first. Without `--yes` asm asks. Backups are never deleted by asm.

## Doctor

```sh
asm doctor
```

Reports each agent's capabilities and detected version, duplicate ids across project directories (which poison Claude's resume), stale locks, and versions that have drifted from the ones asm is verified against. `asm doctor --json` is the machine-readable form the web UI uses to grey out what an agent cannot do.

## Worktrees

```sh
asm worktrees [repo]
```

Lists a repository's git worktrees and the sessions living in each, including worktrees with no sessions.
