---
title: Import across agents
description: Carry a conversation from one agent into another, with a loss report.
---

`asm import` converts through a documented intermediate representation (see the [IR schema](/reference/ir-schema/)) and writes a **native** session in the target agent, so the target's own picker lists it and its own resume works.

```sh
$ asm import 36405fad --to opencode
Imported as opencode:ses_7bdfed0f167b6c507797045ba6.

Loss report:
2 of 3 messages converted
1 opaque reasoning blocks dropped (provider-bound; summaries only)
conversation re-attributed to openai/gpt-5.6-sol (the target install's last-used model)

Resume with: opencode -s ses_7bdfed0f167b6c507797045ba6   (run in the project dir)
```

## Modes

| Mode | What it does | Trade-off |
|---|---|---|
| `--mode full` (default) | Translates the transcript into the target's native records. | Highest fidelity; most exposed to the target's format changing under it. |
| `--mode seed` | Distills the session into a narrative handoff document that becomes the first message of a fresh session. | Lower fidelity; essentially immune to format churn. |

```sh
asm import 4c93a826 --to opencode --dry-run --mode seed   # report only, write nothing
asm import 4c93a826 --to claude-code --project-dir ~/code/elsewhere
asm import 4c93a826 --to opencode --show-toolmap          # the tool-name mapping, then exit
```

## Directions

Any agent can be an import **source**. Only **Claude Code** and **OpenCode** can be a **target** — the others have no sanctioned way to write a session in. (Same-agent import is refused: use [push/pull](/hub/overview/) to move a session between machines under its own id.)

## Idempotent

Target ids are derived deterministically from the source session, so re-importing reports `In sync` instead of creating a duplicate.

## What cannot cross

asm says so rather than pretending: provider-signed reasoning blocks, tools the target does not have (their names are kept verbatim so the history still reads), and nested subagent transcripts. See [what does not cross](/reference/agent-formats/#what-does-not-cross-between-agents).

## Export

```sh
asm export 4c93a826 -o session.ir.json
```

writes the IR as versioned JSON — useful for your own tooling.
