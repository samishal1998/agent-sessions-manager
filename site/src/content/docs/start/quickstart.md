---
title: Quickstart
description: Five minutes from install to a searchable, synced set of sessions.
---

import { Steps } from '@astrojs/starlight/components';

<Steps>

1. **See what you have.**

   ```sh
   asm list
   ```

   One table, every agent. Columns are agent, id prefix, title, project, last update and status (`live` means an agent process owns it right now). Add `--agent opencode` or `--project ~/code/mercury` to narrow it; `--include-children` shows subagent sessions too.

2. **Look at one.** Any unique id prefix works, or `agent:prefix` when two agents collide.

   ```sh
   asm show 4c93a826
   asm resume 4c93a826      # hands off to the native agent, in the right directory
   ```

3. **Search inside transcripts.**

   ```sh
   asm search "path encoder"
   ```

   The first run builds an index; the next ones are incremental. See [Search](/guides/search/).

4. **Browse it with a UI.**

   ```sh
   asm            # terminal UI
   asm serve      # web UI on http://127.0.0.1:7433
   ```

5. **Tidy up.** Every verb goes through the agent's own mechanism and refuses to touch a live session.

   ```sh
   asm rename 4c93a826 "Retry middleware on tower::Layer"
   asm move 4c93a826 ~/projects/renamed-dir
   asm archive 4c93a826
   ```

6. **Carry a conversation to another agent.**

   ```sh
   asm import 4c93a826 --to opencode --dry-run   # shows the loss report, writes nothing
   asm import 4c93a826 --to opencode
   ```

7. **Use two machines.** On one machine, start a hub; on the other, join it:

   ```sh
   asm hub serve                                       # prints the join command
   ASM_JOIN_TOKEN=asmj_… asm join http://hub-host:7434  # on the second machine
   asm push --all
   asm pull 7f3a1c88
   ```

   Then let a [daemon](/hub/daemon/) keep the hub current.

</Steps>

## Where to go next

- The [concepts](/start/concepts/) page explains the vocabulary used everywhere else: sessions, projects, the IR, the hub, sync states.
- [The terminal UI](/guides/tui/) and [the web UI](/guides/web-ui/) have the same verbs as keystrokes and clicks.
- [Multiple machines](/hub/overview/) is the long read for sync.
