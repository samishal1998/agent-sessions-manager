---
title: Reply to a session
description: Send a message into an existing session and stream the reply.
---

```sh
asm send 4c93a826 "now add tests for the retry path"
echo "summarize what we decided" | asm send 4c93a826 -
```

The TUI binds this to `c`, and the web transcript has a composer at the bottom. It goes through each agent's own headless resume — `claude --resume … -p`, `opencode run -s`, `codex exec resume` — so the turn is the agent's, with the agent's tools, permissions and model.

:::caution[Before you use it]
- **It appends to the session; it does not fork.** Verified per agent: same native id, same transcript, no new row.
- **The agent can edit files in that project.** This is a real turn, not a read-only query, and it spends tokens.
- **Live sessions are refused.** If another terminal is driving the session, asm will not send into it — none of these CLIs promise to handle two writers.
:::

## Which agents

Claude Code, OpenCode, Codex and Antigravity. **jcode is not supported**: its stream format has not been captured from a real run, and a normalizer written from a flag name is a guess, not support.
