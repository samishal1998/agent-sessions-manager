# Agent store formats

What each agent keeps on disk, and the traps that cost real debugging. Anything
here that is load-bearing is also encoded as a comment next to the code that
depends on it — this document is the map, not the source of truth.

Claude Code and OpenCode were verified against real installs (2.1.234 and
1.17.18, Linux); the Claude Code internals quoted below were read from the
2.1.233 binary and re-verified behaviorally against 2.1.234. **jcode was read
from its source only** — see the note at the end of its section.

These formats are internal to their agents and may change without notice; that
is exactly why `asm` keeps a tested-versions matrix
(`asm-core/src/import/tested_versions.rs`) and warns on drift.

---

## Claude Code

### Layout

```
~/.claude/
  projects/<encoded-cwd>/
      <session-uuid>.jsonl        the transcript — the session itself
      <session-uuid>/             in-project sidecar: subagents, tool results
      memory/                     NOT a session; must be skipped when scanning
  sessions/<pid>.json             liveness records for running processes
  jobs/<job-id>/state.json        background jobs, holding byte offsets
  file-history/<session-uuid>/    uuid-keyed, lives at the ROOT not the project
  session-env/<session-uuid>/     "
  tasks/<session-uuid>/           "
~/.claude.json                    per-project cost/token rollups
```

A session's state therefore spans **five** locations in two roots. Two are
project-scoped and move when the session is relocated (the transcript and the
in-project sidecar directory); three are uuid-keyed under the root and stay put.
`asm delete` must clear all five; `asm move` must move exactly the first two.

### Identity: the filename, not the field

**The session id is the transcript's filename stem.** Records inside the file
carry a `sessionId` field which usually agrees — and a `session_id` field which
in some versions is a *different, snake_case decoy*. Reading identity from
record contents produces sessions that cannot be resumed. `asm` reads it from
the filename and never from the body.

### The project-directory encoder

Project directories are the session's `cwd` with every non-alphanumeric
character replaced, truncated at 200 characters with a hash suffix. From the
2.1.233 binary:

```js
function mAo(e){ return e.replace(/[^a-zA-Z0-9]/g, "-") }
function Iot(e){ let t=0; for(let r=0;r<e.length;r++) t=(t<<5)-t+e.charCodeAt(r)|0; return t }
function wDy(e){ return Math.abs(Iot(e)).toString(36) }
function WE(e){ let t=mAo(e); if(t.length<=200) return t; return `${t.slice(0,200)}-${wDy(e)}` }
```

Porting this to Rust has four traps, all of which the port in
`adapter/claude/path_encode.rs` handles:

1. The regex and `.length` operate on **UTF-16 code units**, so one astral
   character (an emoji) becomes **two** dashes, not one.
2. `(t<<5)-t` is `t*31` in **wrapping i32** arithmetic, not `i64` and not
   saturating.
3. The hash runs over the **raw** path while sanitization and truncation apply
   to the sanitized string — they are different strings.
4. JavaScript's `Math.abs(-2147483648)` is `2147483648`, which does not fit in
   i32. Widen before taking the absolute value.

Reads never invert this encoding: the authoritative `cwd` is inside the
transcript. The encoder exists only so writes land where Claude Code will look.

### Records

One JSON object per line, appended forever. Types that matter:

| `type` | Meaning |
|---|---|
| `user`, `assistant` | the conversation; `message.content` is Anthropic block format |
| `ai-title` | the **auto-summarizer's** title. Last writer wins within this slot |
| `custom-title` | what Claude Code's own rename writes, and what the picker prefers |
| `last-prompt` | carries `leafUuid`: which record resume continues from |
| `relocated` | written by `/cd`; its `relocatedCwd` **overrides** every record's `cwd` |
| `compact_boundary` | a compaction point; `parentUuid` is null and the chain continues via `logicalParentUuid` |

Records form a tree via `parentUuid`, not a list. Forks and resume-at-point
leave **abandoned branches in the same file**, so reading records in file order
yields a conversation that never happened. The faithful reading walks back from
the resume leaf (`last-prompt.leafUuid`) through `parentUuid`, bridging compact
boundaries. In practice these chains can contain cycles — a real 28.7 MB
transcript on this machine hung an unguarded walk — so the walk needs a visited
set and a plausibility check that falls back to file order rather than silently
dropping history.

`isMeta: true` marks records the UI hides but the model still sees.

The displayed title resolves as `agentName || customTitle || aiTitle`, so a
rename must write `custom-title`: writing `ai-title` instead leaves the new name
invisible in Claude Code's own picker whenever a custom title already exists,
and lets the next auto-summarization silently overwrite it.

### Liveness

`~/.claude/sessions/<pid>.json` maps a running process to its session id. A
session is live if that PID exists. Mutating a live session is refused: the
agent has the file open and holds state you would corrupt.

### Append-only is a hard constraint

`~/.claude/jobs/<id>/state.json` stores `linkScanOffset`, a **raw byte offset**
into a transcript. Rewriting a transcript — even reformatting it identically —
invalidates those offsets. Only two operations are safe: appending, and renaming
the whole file. `asm` never rewrites transcript bytes, which is also why
`asm rename` appends an `ai-title` record rather than editing one.

### Duplicate ids poison resume

`claude --resume <id>` searches across project directories and hard-fails when
two matches exist. So:

- `asm move` **moves** the transcript; it never copies and leaves the original.
- `asm import --to claude-code` scans every project directory first and reports
  `In sync` instead of writing a second copy.
- `asm doctor` reports pre-existing duplicates, since they break resume for that
  id until one is removed.

### Relocation

Moving a session between project directories is not just a file move. The
sequence `asm move` implements, matching what `/cd` does internally:

1. Refuse if the session is live; scan for pre-existing duplicates.
2. Compute the destination directory with the encoder above.
3. Create it `0700`.
4. **Move** the transcript.
5. Move the in-project `<uuid>/` sidecar if present.
6. Append a `relocated` record — this is what keeps the session out of the old
   directory's picker and records the authoritative new `cwd`.
7. Repoint task-output symlinks inside the moved sidecar.
8. Rehome background jobs whose `linkScanPath` pointed at the old transcript
   (`linkScanOffset` stays valid precisely because the bytes did not change).
9. Leave the uuid-keyed root sidecars alone.
10. Leave the trust latch in `~/.claude.json` alone — trusting a new directory
    is the user's security decision, so Claude Code should prompt.

---

## OpenCode

### Layout

```
~/.local/share/opencode/
  opencode.db                     everything: projects, sessions, messages, parts
  opencode.db-wal, -shm           live while OpenCode runs
  storage/session_diff/<id>.json  per-session file sidecar
~/.local/state/opencode/locks/    non-empty while an instance holds the store
```

### Reading: honor the WAL

Open read-only **without** `immutable=1`. With `immutable=1` SQLite skips the
WAL and silently returns stale data — a session written seconds ago simply will
not be there. Never copy the `.db` on its own either; the WAL is part of the
state.

### Schema notes

- `session.time_*` are **epoch milliseconds** (Claude uses RFC 3339 strings —
  mixing them silently corrupts ordering).
- `session.parent_id` marks subagent/child sessions. **A session with a parent
  is invisible to `opencode session list`**, so anything `asm` imports as a root
  session must omit it.
- `session.model` is JSON (`{"id":…,"providerID":…}`), not a plain string.
- Rows written by older CLI generations (1.2.x) coexist with current ones and
  have NULL/empty `directory`, `agent`, and `model`; the project's `worktree` is
  the fallback for the project root.
- `session.time_archived` is a native archived flag, so `asm archive` on an
  OpenCode session sets a column instead of moving files.

### Writing

`opencode import` / `opencode export` are the sanctioned path and the one `asm`
uses for imports: it schema-validates the document, **preserves the ids you
supply** (which is what makes imports idempotent), computes the project binding
from its own working directory, and goes through the migration-aware code path.
Run it with the target project directory as cwd.

#### The tool-state union is not one shape

`state` is a union discriminated on `status`, and the members differ in more
than that field. The error member has a **required `error` string and no
`output` or `title`**:

```
ToolStateCompleted { status: "completed", input, output, title, time, metadata? }
ToolStateError     { status: "error",     input, error,        time, metadata? }
```

Emitting the completed shape with `status: "error"` fails the whole import with
`Missing key at ["state"]["error"]` — and since failed tool calls are ordinary
in real transcripts, that is most sessions. Reading has the mirror trap: the
error message is in `error`, so an exporter that only reads `output` silently
drops every failed tool result.

#### A failed import leaves a partial session

`opencode import` commits the session row, then each message row, and only then
decodes parts. A part that fails validation therefore aborts *after* the session
exists. Any "have I imported this already?" check based on the session row alone
will treat that wreckage as a completed import and refuse to retry — so the test
must require evidence of content (at least one `message` row), and a failed
import should roll its partial session back.

The narrow exceptions — rename, archive, delete — are column-level statements,
because 1.17.x ships no CLI verbs for them. Those refuse to run while the lock
directory is non-empty and write a row-level JSON backup first. `DELETE` clears
child tables explicitly rather than trusting foreign-key cascades, which SQLite
only enforces when the connection opts in.

### Imported sessions must reference a servable model

Resume continues a session with the model recorded on its messages. Importing a
Claude session verbatim pins `anthropic/claude-…`, and if that install has no
Anthropic provider configured, resume fails with `ProviderModelNotFoundError`
before the model ever sees the conversation. `asm` therefore re-attributes
imported conversations to the target install's most recently used model and says
so in the loss report; the original model stays recorded in the IR provenance.

---

## What does not cross between agents

| Thing | Why |
|---|---|
| Signed/encrypted reasoning blocks | provider-bound by construction; only readable summaries survive |
| Tool identities | different tool sets; names are mapped where an equivalent exists and otherwise kept verbatim as inert-but-readable history |
| Nested subagent transcripts | no equivalent container in the target format |
| Cost and token rollups | recomputed by the target agent from its own usage |
| Trust and permission state | a security decision that belongs to the user, not to a migration tool |

---

## jcode

Read from the project's source rather than from a running install, so treat
this section as "what the code says" rather than "what was observed".

### Layout

```
$JCODE_HOME (default ~/.jcode)/
  sessions/<id>.json              the whole session, metadata and conversation
  sessions/<id>.journal.jsonl     append-only journal beside each snapshot
  active_pids/<session-id>        one file per running session; contents = PID
  streaming_pids/<session-id>     set only while a response is streaming
  internal_pids/<session-id>      spawned/debug sessions, hidden from presence UIs
```

This is a third storage shape. Claude Code appends one JSONL per session and
OpenCode keeps rows in SQLite; jcode rewrites a single JSON document.

### Listing cannot take a shortcut

The snapshot serializes in struct order, and the fields a listing needs —
`working_dir`, `short_name`, `status` — come **after** the `messages` array.
So there is no cheap head scan of the kind the Claude adapter uses. Reading
deserializes the document with `messages` typed as serde's `IgnoredAny`, which
walks the conversation without allocating it, then reads the metadata that
follows.

### Fields that matter

| Field | Meaning |
|---|---|
| `id` | session id; also the filename stem |
| `title` / `custom_title` | generated vs user-set. **`custom_title` wins**, as in Claude Code |
| `short_name` | memorable name ("fox"); what `jcode --resume` takes and what the UI shows |
| `working_dir` | the project directory |
| `parent_id` | spawned child sessions |
| `is_debug` | debug/test sessions, hidden like subagents elsewhere |
| `status` | externally tagged: `"Active"`, `"Closed"`, `{"Crashed":{…}}`, `{"Error":{…}}` |
| `last_pid` | the process that last owned the session |

Timestamps are chrono RFC 3339, like Claude Code's and unlike OpenCode's epoch
milliseconds.

### Liveness

A file per running session under `active_pids/`, named by session id, whose
contents are the PID. As with every other agent, the file existing is not
enough — a crashed process leaves it behind, so the PID must still resolve to a
live process.

### Content blocks

`messages[].content[]` is internally tagged on `type`:

| Block | Crosses to the IR as |
|---|---|
| `text` | text |
| `reasoning`, `reasoning_trace` | reasoning, replayable |
| `anthropic_thinking` | reasoning, opaque (signature is provider-bound) |
| `open_ai_reasoning` | reasoning, opaque (encrypted content cannot travel) |
| `tool_use` / `tool_result` | tool call / tool result |
| `image` | file reference; the inline base64 is not copied |
| `open_ai_compaction` | counted in the loss report, not carried |

### Writing

`jcode session rename <session|short-name> <title>` is jcode's own rename and
the path `asm rename` takes. It writes `custom_title` and leaves the generated
`title` alone — the same split Claude Code makes — so asm's title precedence
and jcode's agree by construction rather than by luck. It needs no credentials,
which is what makes it usable as a verification probe.

Archive and delete only move or remove whole files: the snapshot, the `.bak`
jcode writes beside it, and the journal. Nothing rewrites the document.

`cache/session-picker-list-v2.json` caches jcode's picker list (including, as
it happens, sessions belonging to *other* agents — jcode reads those too). It
is derived from the sessions directory and regenerates, so asm drops it after
adding or removing a session rather than leaving a stale picker.

Two verbs stay unsupported. **Moving** a session means editing `working_dir`
inside the snapshot, and **importing** means writing a whole snapshot; jcode
has a command for neither. The entire conversation lives in that one document,
so rewriting it to change a field would rewrite all of someone's history with
no sanctioned path and no way to check the result.

### Verifying a write without credentials

jcode needs a configured provider to hold a conversation, but not to manage
sessions — which makes `jcode session rename <id> …` a clean existence probe.
It succeeds when jcode can resolve the session and fails when it cannot, so
archive and delete can both be checked against jcode's own view:

```
before archive → jcode resolves it       after archive → jcode does not
after unarchive → jcode resolves it      after delete  → jcode does not
```
