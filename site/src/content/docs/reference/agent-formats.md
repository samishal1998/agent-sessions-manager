---
title: Agent store formats
description: What each agent keeps on disk, and the traps that cost real debugging.
---

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

#### Records from another machine's path, after the marker

Verified against Claude Code 2.1.278 with two `CLAUDE_CONFIG_DIR`s standing in
for two machines: a transcript installed at a different path with one appended
`relocated` record, then **fast-forwarded with records carrying the other
machine's `cwd`** after that marker, resumes normally. The whole conversation
carries over, tools run in the local directory, and Claude appends to the same
file under the same id — it creates no second copy.

Claude Code also writes this record itself: resumed from a directory that
differs from the last record's `cwd`, it appends
`{"type":"relocated","relocatedCwd":…,"sessionId":…}` — the same three fields
asm writes, and likewise **no timestamp**. With two markers the last one wins.
So `relocated` records are local bookkeeping that can appear anywhere in the
file, not only at the end: anything comparing transcripts across machines has
to leave them out.

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

**`export` is not a read.** Measured against 1.18.31 by diffing the whole store
around two exports: run from a directory that is not a git repository, each
call bumps the `global` project row's `time_updated`, and *any* OpenCode
invocation from inside a git repository registers that repository as a
project. The export itself is byte-stable across runs. Anything that exports
repeatedly — a sync daemon — should read the rows itself instead.

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

### OpenCode 2.x

Verified against 2.0.25. asm **reads** this schema directly and **writes** it
only by running `opencode` (below).
It is the same file (`opencode.db`), told apart from 1.x by its tables: 2.x has
`session_v2` and `session_message` and no `message`/`part`. (1.18 already
carries a `session_message` table beside the 1.x ones, so the pair alone is not
the test: `session_v2` is.)

```
project           id, worktree, vcs, name, sandboxes, ...      the same table as 1.x
session_v2        one row per session (renamed from 1.x's `session`)
session_message   one row per message: id, session_id, type, seq, time_*, data
event, event_sequence       the log both of those are projected from
session_pending, session_inbox   input waiting to be taken into a turn
workspace, worktree, project_directory
```

- `session_v2` keeps the 1.x columns asm uses (`id`, `parent_id`, `slug`,
  `directory`, `title`, `version`, `agent`, `model`, `cost`, `tokens_*`,
  `time_created`, `time_updated`, `time_archived`) and adds `fork_session_id`,
  `fork_boundary`, `time_idle`, `idle_outcome`, `time_viewed`, `time_suspended`
  and `resume_attempts`. `title` is nullable. `directory` is where the session
  was started (`location.directory` in the transfer document; the project's
  `worktree` is the fallback when it is empty), which may be inside the project
  rather than its root; `path` is that directory relative to the project root
  (`subpath` in the document), unset at the root itself. `parent_id` still
  marks subagents.
- **`time_archived` means nothing in 2.0.25.** Only a migration and an import
  write it, and nothing filters on it, so asm does not treat it as archived: a
  session archived in 1.x lists as an ordinary one (it could never be
  unarchived otherwise). The value still travels in a hub bundle, as data.
- **A migrated database keeps the 1.x tables.** 2.x copies 1.x sessions into
  `session_v2` in a background job (a cursor in the `kv` row `migration.v1-v2`)
  and does not drop `session`, `message` and `part`; a 1.x binary can still add
  rows to them afterwards. `session_v2` plus `session_message` is what makes a
  store 2.x. A 1.x session that is not in `session_v2` yet is listed and read
  from the 1.x tables, read-only (so nothing is hidden), and `asm doctor` and
  `asm list` say how many there are. asm changes nothing in such a store until
  OpenCode has migrated them: rename, delete, import and a hub pull are
  refused, and such a session is not pushed to a hub. A database asm cannot inspect at
  that moment (locked, busy, unreadable) is read as 1.x for listing but never
  written: every change refuses with a message to retry.
- **The tables are projections.** The service folds events from `event` into
  `session_v2`/`session_message`; a streamed tool call is the same `assistant`
  row rewritten in place, and a committed revert deletes the rows after its
  boundary. Anything that writes the projections by hand can be contradicted by
  the log, which is why asm does not.
- **Order is `seq`**, unique per session. `session_message.data` is the message
  JSON with `id` and `type` moved into their own columns, and `time_updated` moves whenever
  the row is rewritten, so a message count with the newest `time_updated` is a
  complete change marker.

Message kinds, as read (`type` column; the schema is
`packages/schema/src/session-message.ts` in the OpenCode source):

| `type` | `data` | In the IR |
|---|---|---|
| `user` | `text`, `files[{data,mime,source{type,uri?},name?}]`, `agents[{name}]`, `skills[{id,name,text?}]` | user text and file parts (inline bytes are not carried); `agents`, `skills` in extensions |
| `assistant` | `agent`, `model{id,providerID}`, `content[]`, `finish`, `cost`, `tokens`, `error{type,message}`, `snapshot`, `time{created,streamed?,completed?}` | text, reasoning and tool parts; the rest in extensions (`opencode`); an assistant with no content and no error is dropped |
| `shell` | `shellID`, `command`, `status` (`running`/`exited`/`timeout`/`killed`), `exit?`, `output{output,cursor,size,truncated}`, `time` | a `shell` tool call and its result (assistant side, as 1.x records `!` commands) |
| `system`, `synthetic` | `text`, `description?` | system text |
| `skill` | `skill`, `name`, `text` | system text |
| `compaction` | `status` (`running`/`completed`/`failed`), `reason` (`auto`/`manual`), `summary`, `recent`, or `error` | system text of the summary (or the failure); `reason`, `recent`, `status` in extensions |
| `idle`, `agent-switched`, `model-switched`, `location-switched` | `outcome` / `agent` / `model` / `location` | dropped (bookkeeping; every assistant row records its own agent and model) |

An assistant's `content` items are `text{text}`, `reasoning{text, state?}` and
`tool{id,name,state,time}`. A tool's `state` is `streaming{input: string}` (the
JSON typed so far), `running{input,metadata}`, `completed{input,content,metadata?}`
(`content` is never empty) or `error{input,error{type,message},content?}`.
`content` is a list of `{type:"text",text}` and `{type:"file",uri,mime,name?}`.
asm turns a completed result into the joined text (files become a one-line
reference), an error into its message, and leaves streaming and running calls
without a result. Unlike 1.x, a failed tool keeps its message in `error.message`
and a completed one has no `output` string.

**Liveness.** The service claims a session by setting `time_suspended` when a turn
starts and clears it when the turn succeeds, fails or is interrupted by the user.
An interruption by shutdown leaves the claim for the next start to resume
(`resume_attempts` counts those). The service writes itself to
`$XDG_STATE_HOME/opencode/service.json` (`id`, `version`, `url`, `pid`, and
a `password` when the service has one). asm reports a session live when it is
claimed *and* that service is proven up the way OpenCode's own probe proves it:
the pid is alive and `GET <url>/api/info` (loopback only, with the password
as Basic auth when there is one) answers with that pid and version. A pid alone
is not proof (it may have been reused by another process). A claim whose
service is not up is an interrupted turn, shown idle. This was derived from the
OpenCode source and binary, not observed on a running turn. Only `service.json`
is read: OpenCode names the file by release channel (`latest`, `dev`, `beta` and
`next` use it; any other channel writes `service-<channel>.json`), so a service
of another channel is not seen, and asm stands alone (below) and shows its
turns as idle. 1.x's `locks/` directory does not appear in 2.x.

**Resume** is unchanged: `opencode -s <id>` from the session's directory.

#### Writing, and the transfer document

asm never writes the projections: the log can contradict them and a running
service owns them. It runs the CLI instead, and the document it builds and
reads is `SessionTransfer.Data`, what `opencode session export <id>` prints and
`opencode session import <file>` takes:

```
{ "info":     { id, parentID?, fork?, projectID, agent?, model?{id,providerID,variant},
                cost, tokens{input,output,reasoning,cache{read,write}}, outcome?,
                time{created,updated,idle?,viewed?,archived?}, title?,
                location{directory,workspaceID?}, subpath?, metadata?, permissions?, revert? },
  "messages": [ { id, type, ...data } ... ] }
```

- **Reading it without OpenCode.** asm builds the same document from read-only
  queries: `info` the way OpenCode shapes a `session_v2` row (a model without a
  `variant` gets `"default"`; an unset field is absent), `messages` the stored
  `data` with `id` and `type` put back, in `seq` order, minus what OpenCode's
  export leaves out (an assistant message that has not completed, a shell or
  compaction still running). Run against the same store, it and `opencode
  session export` print the same document, which a test checks for every
  message kind.
- **What an import keeps and rewrites.** It keeps session and message ids, the
  parent link, titles, `metadata`, `permissions`, cost, tokens, the message
  order, `idle`/`outcome` and `time.created`. It renumbers `seq` 1..n,
  recomputes `projectID` from the directory it is given (`--directory`, which
  must exist; without it the current directory, not `info.location`), makes a
  new slug, sets `time.updated` to the import time and clamps `time.viewed` to
  `time.idle`. `fork`, `revert`, `subpath` and `workspaceID` are not imported.
  A parent must be imported before its children; an id already there answers
  `Session already exists` with exit status 0, so asm looks first and checks
  after; a message id cannot be reused under another session; an import is
  atomic. `--sanitize` redacts content and is never used for a backup.
- **Refused while busy.** A session is busy when `time_suspended` is set (a turn
  claimed it, or its service died and left the claim for the next start to
  resume) or `session_inbox`/`session_pending` holds anything for it. The
  message says which: a turn that is running, work that is queued, or "an
  unfinished turn from an OpenCode that is no longer running; start OpenCode once
  to finish or abandon it, then retry" (asm itself never starts the service that
  would resume it).
- **Service or standalone.** With no flag the CLI uses the service in
  `service.json`; with the service down it would start a managed one whose boot
  resumes claimed sessions. asm adds `--standalone` unless that service is
  proven up (see Liveness). A stale `service.json` whose pid another process now
  owns, or whose port nothing answers, is not a service. `OPENCODE_DB` and
  `OPENCODE_TEST_HOME` are removed from the environment asm gives `opencode`:
  they would make it write a database other than the one asm reads.
- **Rename** is `opencode api session.update --param sessionID=ID -d
  '{"title":"..."}'`; an empty title would make OpenCode generate one with a
  model, so it is refused. **Delete** is `opencode session delete ID`, recursive
  over subagents, after a backup that holds everything stored (an abandoned
  partial turn too; an import leaves unsettled messages out again), is fsynced
  with its directory and read back before anything is deleted, is private to the
  user (directory 0700, files 0600), and comes with a `manifest.json` that names
  each session's directory. Restoring is `opencode session import <file>
  --directory <directory>` per session, the root first. Just before the delete
  the tree is read again; if a session was added, a turn started or a message
  arrived since the backup, nothing is deleted and the backup stays. There is **no archive**: 2.0.25 sets
  `time_archived` only by migration and import and nothing filters on it. **Move**
  (`api session.move`) only queues a request for a persistent server.
- **Import from another agent** builds a document with deterministic ids
  (`ses_`/`msg_` from the source id, so a second import is a no-op), user text
  as `user`, assistant text, reasoning and tool calls as one `assistant` message
  with its tool results folded into `completed` (or, for a failure, `error{type,
  message}`) states, tool names mapped (`Bash` and `bash` become `shell`, `Task`
  becomes `subagent`, `Read`/`Edit`/`Write`/`Glob`/`Grep`/`WebFetch`/`WebSearch`
  as 2.x names them, anything else kept), a tool input that is not an object
  wrapped as `{"value": ...}`, a call with no recorded result as an `error` of
  type `interrupted`, and a closing `idle` message. Attachments (2.x stores
  their bytes inline) and nested subagent runs are left out, and the loss report
  says so.

#### The hub bundle

A 2.x session is pushed as one file, `transfer.json`:
`{generation: "opencode-v2", root, sessions: {<id>: {info, messages}}}`, the
session and every descendant in the document above, with
`extra.generation = "opencode-v2"` in the manifest. A 1.x bundle is
`rows.json` (database rows) and has no generation; a pull checks which one it
holds against which one the machine has, and refuses a mismatch.

- **Identity.** What the hub records as the copy's identity is a hash of, per
  session in id order, `id`, `parentID`, `title`, `agent`, `model`, `metadata`,
  `permissions`, `cost`, `tokens` and every settled message. (What decides
  whether one copy contains the other is finer: each session's `id`, `parentID`,
  `title`, `metadata` and `permissions`, and each message, one by one. Cost,
  tokens, agent and model follow the messages, so a plain continuation does not
  make a merely-behind copy look diverged.) It leaves out what an import rewrites
  (`time.updated`, `time.viewed`, `projectID`, the slug) and where the session is
  filed (`location`, `subpath`, `workspaceID`), so two machines holding the same
  conversation agree. It is not the push fingerprint, which also moves with
  `time_updated` and is only ever compared on one machine.
- **Dropped from the bundle**, because the format has no place for it:
  `time_suspended` and `resume_attempts` (a claim and its retry count, which
  belong to one machine's service), the queued `session_inbox` and
  `session_pending` rows, the machine-local `workspace_id`, and the per-session
  instruction state (`instruction_state`, `instruction_entry`,
  `instruction_blob`: what the session's next turn builds its instructions from;
  neither `session export` nor `session import` in the 2.0.25 source touches
  them, and OpenCode rebuilds it on the next turn). Dropped by an
  import: `fork`, `revert`, `subpath`; and `time.viewed` is clamped. A session
  that is mid-turn is pushed as it stands; its unfinished message is left out
  until it completes.
- **Install** imports the root and then each descendant, parents first, with
  `--directory` the target directory (all of them: a pulled tree is one project
  here). New: none of the ids is here. In sync: same identity. Behind: replace it,
  which is back up the local tree as documents, check it did not change meanwhile,
  `opencode session delete`, import the hub's; if that import fails the old tree
  is imported back (a half-imported tree is removed on what the store holds, not
  on what the CLI reported) and the backup is named. A copy that differs only in
  titles is not replaced: the titles are applied with a rename, the old ones
  saved first (the pull reports `renamed`). Ahead and diverged change nothing.
  There is no fast-forward on 2.x because `session import` only creates.
- **What an install checks first.** The bundle holds at most 1000 sessions and
  200 000 messages; every session id starts with `ses` and every message id with
  `msg_`; every session is part of the tree under the root; and the root is
  nobody's child (an import would otherwise hang the tree under whichever local
  session had that id). A store that cannot be inspected, or is still migrating
  1.x sessions, is refused before anything is read. The refusal for the other
  generation says which case it is: no OpenCode installed here, a 1.x-format
  database with 2.x installed (start OpenCode 2.x once so it migrates, then pull
  again), or a different major version (naming the version that pushed it).
- **Trust.** A pulled session carries its `permissions`, `metadata`, agent and
  model exactly as pushed, and its messages as they were. Pull only from
  machines you control. (Remote-control pulls only ever install copies pushed by
  named machines of your own hub.)

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

## Codex

- **Base:** `$CODEX_HOME`, else `~/.codex`. Sessions are **rollout** JSONL files under `sessions/`, with metadata in `state_5.sqlite`'s `threads` table.
- **Listing:** the `threads` table drives it, and asm also sweeps `sessions/` for rollouts with no `threads` row — Codex hides those until you resume them by id.
- **The working directory is inside the conversation** (turn context, environment messages), not only in metadata, so a rollout cannot be re-homed to another path by editing metadata: hub restore is same-path only.
- **Locking:** Codex holds an `flock` on `thread-writer-locks/<id>.lock` while writing; a hub install takes the same lock, so a live session is refused and Codex is kept out meanwhile.
- **Resume** finds a rollout placed under `sessions/` by id and writes the `threads` row itself; `migrate-rollouts --apply` is *not* an adoption path (it refuses a thread with no row).

## Antigravity

- **Base:** `~/.gemini/antigravity-cli` (a hard-coded root in agy; `ASM_ANTIGRAVITY_ROOT` overrides it for asm).
- **One SQLite database per conversation**, `conversations/<id>.db`, with steps as protobuf blobs against an unpublished schema. asm does not decode them: it reads `brain/<id>/.system_generated/logs/`, the JSONL rendering agy writes alongside.
- **No project directory is recorded.** `cache/last_conversations.json` maps a directory to its *most recent* conversation only.
- **Restore** writes the database (a `VACUUM INTO` snapshot taken on a read-only connection) plus `brain/<id>/` and one metadata entry; it never writes `last_conversations.json`, which would clobber another conversation's binding.
