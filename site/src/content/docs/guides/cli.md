---
title: The command line
description: How the asm CLI behaves — refs, filters, JSON output and exit behaviour.
---

Every capability is a command, and every command takes `--json` for scripting. The [reference](/reference/cli/overview/) lists every command and option; this page covers the behaviour they share.

## Referring to a session

Commands that act on one session take a **ref**:

- a native id, or a unique prefix of one (`4c93a826`);
- `agent:prefix` when two agents have ids that start the same (`opencode:ses_32d1`).

An ambiguous prefix is an error that lists the candidates; it never picks one.

## Filters

Three global options narrow what a command acts on, and mean the same everywhere:

| Option | Effect |
|---|---|
| `--agent <name>` | One of `claude-code`, `opencode`, `jcode`, `codex`, `antigravity`. |
| `--project <dir>` | Sessions of that project (the whole repository, not just the directory — see [Projects](/concepts/projects/)). |
| `--include-children` | Include subagent and child sessions, hidden by default. |

On `push` and `pull`, `--all` means **everything the filters match** — `asm pull --all --agent codex` pulls every Codex session on the hub — and `--include-children` is its own, separate decision.

## Everyday verbs

```sh
asm list --agent opencode
asm projects --worktrees        # each repository and its checkouts
asm show 4c93a826
asm resume 4c93a826             # hands off to the native agent, in the right directory
asm rename 4c93a826 "New title"
asm move 4c93a826 ~/projects/renamed-dir
asm archive 4c93a826
asm unarchive 4c93a826
asm delete 4c93a826             # backs everything up first
asm doctor                      # store health, duplicate ids, stale locks
```

See [Organize sessions](/guides/organize/) for what each does per agent.

## Scripting

`--json` switches output to JSON on every command. Errors go to stderr with a non-zero exit status. Tables are only drawn when stdout is a terminal; piped, `asm` with no command prints the plain session table instead of opening the TUI.

```sh
asm list --json | jq -r '.[] | select(.status == "live") | .title'
asm export 4c93a826 -o session.ir.json
```

## Safety

Mutating commands refuse a session whose agent is running, never rewrite transcript bytes, and back up before destroying. The full list is in [Safety model](/concepts/safety/).
