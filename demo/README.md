# demo store

Builds a synthetic store for all five agents, so the landing page and the
README can show the real UI without putting anybody's actual sessions on the
internet.

Nothing here is used by `asm` itself. It exists because the screenshots on
the site have to be reproducible: when the UI changes, someone needs to be
able to regenerate them, and the alternative — screenshotting real sessions —
publishes project names and transcripts.

Every adapter honours an environment override for its store root, which is
the only reason this works.

```sh
python3 demo/gen.py /tmp/demo demo/schema.sql

H=/tmp/demo
env HOME=$H \
    CLAUDE_CONFIG_DIR=$H/.claude \
    JCODE_HOME=$H/.jcode \
    XDG_DATA_HOME=$H/.local/share \
    CODEX_HOME=$H/.codex \
    ASM_ANTIGRAVITY_ROOT=$H/.gemini/antigravity-cli \
    asm serve --port 7455 &

node demo/shot.mjs ./shots 7455
```

`schema.sql` is a schema-only dump of a real `opencode.db` — tables and
indexes, no rows. OpenCode's schema is drizzle-managed and moves, so it is
captured rather than hand-written.

## The one thing that is real

`shot.mjs` captures `reply.png` by actually sending a message through the
composer into the demo store's own Claude Code session, and screenshotting
the reply while it is still in flight. The session is synthetic but valid, so
`claude --resume` genuinely opens it and answers in context — which is what
makes that screenshot a real turn rather than a mock-up. It needs credentials
in `$H/.claude`; without them the other four shots still work and that one
fails.

Since a real agent writes into the store, regenerate it (`gen.py`) before
each screenshot run rather than reusing a store a previous run has appended
to.
