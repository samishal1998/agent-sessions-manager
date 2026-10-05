---
title: Safety model
description: The rules asm follows because it writes into stores owned by other programs.
---

asm writes into stores owned by other programs, so the rules are strict.

- **Never touch a live session.** Mutating a session whose agent is running is refused outright. (Pushing is a read, so running sessions are backed up too.)
- **Never rewrite transcript bytes.** Claude Code's background jobs hold raw byte offsets into transcript files; asm only appends or renames whole files.
- **Never duplicate a session id.** Claude Code's cross-project `--resume` hard-fails when an id exists in two project directories, so asm *moves* rather than copies and refuses an import that would collide. `asm doctor` reports pre-existing duplicates.
- **Back up before destroying.** `asm delete` copies every affected path into `<data>/backups/<agent>/<id>/<timestamp>/` first. So does any hub replace.
- **Never write into a busy store.** OpenCode mutations are refused while an OpenCode instance holds its lock directory.
- **Only write through sanctioned paths where they exist.** Imports into OpenCode go through `opencode import`, not raw SQL; jcode renames go through `jcode session rename`.
- **A pull obeys all of the above.** It refuses a live session, never makes a second copy of an id, and only ever appends to a transcript that is a prefix of the hub's.
- **The daemon never writes into an agent's store on its own.** It only pushes. The one exception is a pull you allowed with `asm control enable`, which obeys every rule above and installs only under your home folder (see [Remote control](/hub/control/)).

## What asm writes, and where

Only inside its own directory (`$XDG_DATA_HOME/asm`, override with `ASM_DATA_DIR`) — archive, backups, index, hub state — plus the agents' stores through the paths above. See [Data files](/reference/data-files/).

## The web UI

It has no authentication and binds loopback by default; see [its security note](/guides/web-ui/#security). The hub's own threat model is on the [Security](/hub/security/) page.
