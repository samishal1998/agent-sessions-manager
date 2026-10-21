---
title: The command line
description: How the asm CLI behaves — refs, filters, JSON output and exit behaviour.
---

Every capability is a command, and every command that prints a result takes `--json` for scripting. The [reference](/reference/cli/overview/) lists every command and option; this page covers the behaviour they share.

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
| `--include-children` | Include subagent and child sessions, hidden by default. `asm list` nests them under their parent. |

On `push` and `pull`, `--all` means **everything the filters match** — `asm pull --all --agent codex` pulls every Codex session on the hub — and `--include-children` is its own, separate decision.

### Subagent sessions

`asm list` folds a session's [child sessions](/start/concepts/#child-sessions) behind it and marks the parent with `+N` (how many) at the end of its row; `asm list --include-children` shows them under it:

```text
AGENT     ID        PARENT    TITLE                      …  STATUS
opencode  ses_71bc            Billing webhook retries    …  idle
opencode  ses_71c0  ses_71bc  ↳ Review the retries       …  idle
opencode  ses_71c4  ses_71c0    ↳ Explore the queue      …  idle
```

A child needs no flag to be named: `asm show ses_71c4`, `asm delete ses_71c4` and the other verbs find it by its id. Deleting an OpenCode session deletes everything under it, and the prompt lists what (`--yes` skips the prompt, not the backup). `asm list --json` is the same flat array either way; each session has a `parent`.

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

`--json` switches output to JSON: a bare array or object on stdout, nothing else there. The exceptions are the commands that cannot have a document: `tui`, `serve`, `hub serve`, the foreground `daemon` and `resume`. A failure prints one `{"error": {"code", "message"}}` object to stderr and exits with a status for its code (10 not found, 11 ambiguous, 12 session in use, and so on). A command that would prompt, such as `delete`, refuses under `--json` unless you pass `--yes`. Tables are only drawn when stdout is a terminal; piped, `asm` with no command prints the plain session table instead of opening the TUI. The full contract, every output shape and the exit codes are in [JSON and exit codes](/reference/json/).

```sh
asm list --json | jq -r '.[] | select(.state == "live") | .title'
asm list --status live --ids-only | xargs -n1 asm show
asm children ses_71c4 --json            # the subagent tree under a session
asm messages 4c93a826 --role assistant --tail 3
asm events                              # one JSON line per session that is added, changed or removed
asm capabilities --json                 # versions, and what each agent's store can do
asm export 4c93a826 -o session.ir.json
```

`select(.state == "live")` works on a session and on a search hit alike; a session's `status` is an object (`{"state": "live"}`) and a search hit's is a bare string, so compare `state`.

## Safety

Mutating commands refuse a session whose agent is running, never rewrite transcript bytes, and back up before destroying. The full list is in [Safety model](/concepts/safety/).
