---
title: JSON and exit codes
description: "The machine interface of asm: every --json output, the contract version and its compatibility promise, errors, exit codes and the NDJSON streams."
---

`asm` is built to be driven by other programs. This page is the contract: what `--json` prints, how a failure is reported, what the exit status means, and what will and will not change between releases. The same contract covers the [web API](#web-api)'s machine routes.

## The contract version

`asm capabilities --json` (and `GET /api/meta`) report `json_contract_version`, currently **1**.

**The promise.** Within one contract version a shape only gains. A release may add a field to an object, a command, a flag, a route, a value to an enumeration or an error code; it never removes or renames a field, changes what one means, changes its type, or turns an array into an object. Readers must ignore fields they do not know, and treat a value of an enumeration they do not know as "something new".

**When the number is bumped.** Only for a change that breaks that promise: removing or renaming a field, changing the meaning or type of one, changing an existing command's output from one kind of document to another, or removing a command, flag or exit code. Such a release says so in the [changelog](/about/changelog/), and the number is the first thing a program should check. Adding things never bumps it.

Everything else on this site (table layouts, human text, the order of keys, the wording of a `message`) is for people and may change at any time.

## Conventions

- **`--json` writes JSON to stdout and nothing else there.** Progress, warnings and notes go to stderr as text. `ASM_JSON=1` in the environment is the same flag.
- **A document is bare**: an array for a listing, an object for one thing. There is no envelope around results; the existing outputs have always been this way and stay so.
- **Timestamps** are RFC 3339 (`2026-10-09T10:00:05Z`). **Paths** are absolute strings. **Agent names** are `claude-code`, `opencode`, `jcode`, `codex`, `antigravity`.
- **A session is addressed** by `agent:id`, a native id, or a unique prefix of one (`asm show 7f3a1c88`). The `id` and `ref.native_id` fields of a session are what to give back.
- Commands that stream print **NDJSON**: one compact JSON object per line, flushed as it is written. See [streams](#streams).

## Errors and exit codes

With `--json`, a failed command prints **one JSON object to stderr and nothing to stdout**:

```json
{"error": {"code": "ambiguous", "message": "'7f3a1c' is ambiguous; matches: claude-code:7f3a1c88-…, claude-code:7f3a1c99-…",
           "candidates": ["claude-code:7f3a1c88-…", "claude-code:7f3a1c99-…"]}}
```

`message` is for people. `code` is for programs. `candidates` is present only for `ambiguous`: the `agent:id` of each session the reference matches, so a caller can ask again with one. This includes mistakes clap catches (an unknown flag, a missing argument), which are `usage`. Without `--json` the text on stderr is what it always was (`Error: …`), and the exit status is the same.

| `code` | Exit | HTTP | Meaning |
|---|---|---|---|
| *(success)* | 0 | 200 | |
| `internal` | 1 | 500 | Anything unexpected: a bug, or a failure asm has no word for. |
| `usage` | 2 | 400 | The arguments or request are wrong. Also what clap itself exits with. A request refused as invalid (a bad title, a bad directory) is `usage`. |
| `not_found` | 10 | 404 | No session matches; no such machine, archive entry or store. |
| `ambiguous` | 11 | 409 | The reference matches several sessions; see `candidates`. |
| `live_session` | 12 | 423 | The session is in use by a running agent, or its store is busy. Nothing was changed. |
| `unsupported` | 13 | 501 | The agent, or its installed version, cannot do this (OpenCode 2.x has no archive; Codex cannot be renamed). |
| `conflict` | 14 | 409 | The change would overwrite or contradict what is there: a destination that exists, two copies that both changed. |
| `unreachable` | 15 | 502 | A hub or peer did not answer. Try again later. |
| `io` | 16 | 500 | Reading or writing a file or database failed. |
| `unauthorized` | 17 | 401 | A credential is missing or wrong (the web API's bearer token, a hub's machine credential). |

Exit status 0 is success. A command that prints a document and then exits nonzero is describing a partial failure in the document itself, as below.

**Exceptions, per command.**

- `asm control push|pull|send|move --wait` keeps the statuses it has always had, because scripts test them: **0** ok, **1** not ok, **2** still waiting, **3** could not reach the hub. It prints the job or plan document on stdout; a failure it cannot recover from is still reported as an error object on stderr, but exits with these statuses rather than the table's.
- `asm push` and `asm pull` print their report and exit **1** if any item failed, and `push --to` / `pull --from` exit **1** after printing a diverged result. The report is the place to read what failed.
- `asm resume` exits with the status of the agent it started.
- A command killed by a signal has the shell's status for it.

**Prompts.** A command that would ask a question (`asm delete`, `asm control move`) never prompts under `--json`: without `--yes` it fails with `usage` and the message `confirmation required: pass --yes`.

## What each command prints

### The session object

```json
{
  "ref": {"agent": "claude-code", "native_id": "7f3a1c88-…", "location": {"kind": "jsonl_file", "path": "/…/7f3a1c88-….jsonl"}},
  "id": "7f3a1c88-…",
  "title": "Fix the build", "slug": null,
  "project_root": "/home/me/code/app", "git_branch": "main",
  "created": "2026-10-09T10:00:00Z", "updated": "2026-10-09T10:00:09Z",
  "model": "claude-fable-5",
  "usage": {"cost_usd": null, "input_tokens": null, "output_tokens": null, "cache_read_tokens": null, "cache_write_tokens": null},
  "status": {"state": "idle"},
  "state": "idle",
  "parent": null, "agent_version": "2.1.230", "size_bytes": 1948
}
```

- `ref.location.kind` is `jsonl_file`, `sqlite_row` or `archive`.
- `status` is an **object**: `{"state": "idle"}`, `{"state": "archived"}` or `{"state": "live", "pid": 4242}`.
- `state` (added in 0.16) is the same word as a flat string: `live`, `idle` or `archived`. `id` (added in 0.16) is `ref.native_id`. Use these when you only want the word or the id.
- `parent` is the native id of the parent for a [child session](/start/concepts/#child-sessions), `null` for a top-level one.

:::note[A fix in 0.16]
`asm search --json` hits have always had a `status` that is a bare **string** (`"idle"`), while a session's `status` is an **object**. A filter like `select(.status == "live")` therefore never matched a session. Both are unchanged, and both now also carry `state`, the flat string, so `select(.state == "live")` is right on a session and on a search hit.
:::

### Commands

| Command | Output |
|---|---|
| `list` | Array of sessions, newest first. Top-level sessions; with `--include-children`, every session. `--status` narrows it. |
| `show REF` | One session. |
| `children REF` | Flat array of the sessions under `REF` in tree order, each a session plus `depth` (1 for a direct child). `parent` links the tree. |
| `projects` | Array of `{root, repo, agents[], worktrees[{path, branch, is_main, session_count}], session_count, size_bytes, last_updated}`. |
| `worktrees [REPO]` | Array of `{path, branch, is_main, detached, locked, prunable, session_count, sessions[]}`: git's worktrees, and the sessions living in each. |
| `search QUERY` | Array of hits: `{agent, native_id, title, project_root, parent, role, seq, timestamp, snippet, score, status, state}`. `snippet` wraps matches in `\u0001` … `\u0002`. `status` and `state` are the same string here. |
| `messages REF` | [NDJSON](#asm-messages) of messages; `--format json` one array. |
| `export REF` | The session as [IR](/reference/ir-schema/): `{ir_version, source, title, …, messages[]}`. With `-o FILE`, the IR goes to the file and stdout is `{"ok": true, "agent", "id", "path", "messages"}`. |
| `import REF --to AGENT` | `{mode, target, in_sync, dry_run_artifact, resume_hint, warnings[], loss{…}}`. `--show-toolmap` prints `{"target", "tools": [{"from", "to"}]}`. |
| `rename REF TITLE` | `{"ok": true, "agent", "id", "title"}`. |
| `move REF DIR` | `{"ok": true, "agent", "id", "dir", "new_transcript", "warnings": []}`. |
| `archive REF` | `{"ok": true, "agent", "id", "archived_to"}`; `archived_to` is `null` when the agent archives with a flag. |
| `unarchive REF` | `{"ok": true, "query", "restored_to"}`; `restored_to` is `null` when the agent clears a flag in place. |
| `delete REF --yes` | `{"ok": true, "agent", "id", "backup_dir", "removed": [], "descendants", "note"}`. |
| `capabilities` | See [below](#capabilities). |
| `doctor` | `{"agents": [{agent, store_root, store_found, session_count, duplicate_session_ids[], warnings[], capabilities{…}, schema, unmigrated}], "warnings": []}`. The top-level `warnings` lists stores that could not be read at all. `schema` is `v1`, `v2`, `absent`, `unsupported` or `unknown` for OpenCode and `null` for the others; `unmigrated` is, for OpenCode 2.x, whether sessions are waiting to be migrated. |
| `index` | `{"refresh": {scanned, reindexed, unchanged, removed, messages_indexed, failed[]}, "stats": {path, sessions, messages, bytes, oldest_indexed_ms}}`. |
| `sync init` / `status` / `commit` | `{archive_root, created_repo, remote_set}` · `{archive_root, archive_exists, git_initialized, branch, remote, last_commit, uncommitted, sessions[], total_bytes, notes[]}` · `{committed, message, files_changed}`. |
| `update` | **One** object: `{current, latest, update_available, target}` and, once it ran, `updated` (bool) and, if it installed, `updated_to` and `path`. |
| `join URL` | `{url, machine: {id, name, joined, last_seen}}`. |
| `push` / `pull --all` | `{"items": [{agent, native_id, label, outcome: {"state": "ok"\|"skipped"\|"failed", "note"\|"reason"\|"error"}}]}`. |
| `pull REF` / `push REF --to` / `pull REF --from` | `{agent, id, title, slug, from, outcome, project_root, path}`; `outcome` is `{"result": "new"\|"fast_forward"\|"replaced"\|"renamed"\|"in_sync"\|"ahead"\|"diverged"}` (`fast_forward` adds `appended`, a byte count). |
| `remote list` / `remote machines` | Array of rows (project, agent, id, title, machine, state, action, …) · array of machines. |
| `peer list` / `add` / `remove` | Array of `{name, address}` · `{name, address}` · `{name, address}`. |
| `peer sessions NAME` | Array of sessions, as `list` on that machine. |
| `hub token` / `admin-token` / `commands-token` | `{"token": "…"}`. |
| `hub machines` / `revoke` | Array of `{id, name, joined, last_seen}` · the machine removed. |
| `daemon status` | `{state, file: {pid, version, started, interval, hub, machine, passes, last_pass, last_push, pending, idle, hub_reachable, last_error, …}, quiet_for}`; `file` is `null` if a daemon never ran here. |
| `daemon start` / `stop` | `{"started": true, "pid", "log"}` · `{"stopped": bool, "pid"}`. |
| `daemon install` / `uninstall` | `{"installed": true, "unit", "started"}` · `{"uninstalled": bool, "unit"}`. |
| `control enable` / `status` / `disable` | `{enabled, allow[], …}` · `{enabled, allow[], daemon_running}` · `{"enabled": false}`. |
| `control push` / `pull` / `send` / `move` / `show` / `cancel` / `retry` | A command (job) or a plan with its steps. |
| `control jobs` | Array of jobs. |
| `send REF MESSAGE` | NDJSON of the reply's events; see [streams](#streams). |

Commands with no document, by nature: `asm tui`, `asm serve`, `asm hub serve`, the foreground `asm daemon`, `asm resume` (it hands over the terminal), and the two ssh ends of a peer transfer (`hub receive`, `hub send`, whose stdout is their protocol). `asm events` and `asm list --follow` are streams. A test in the repository walks the command tree and fails if a command is neither in this list nor answers `--json` with JSON or an error object, so this page cannot quietly fall behind.

### Capabilities

`asm capabilities --json` is what to read first:

```json
{
  "asm_version": "0.16.0",
  "json_contract_version": 1,
  "ir_version": 1,
  "index_schema_version": 5,
  "agents": [
    {"agent": "opencode", "installed": true, "store_root": "/…/opencode.db",
     "capabilities": {"list": true, "read_transcript": true, "liveness": true, "rename": true, "archive": false,
                      "relocate": false, "delete": true, "import_ir": true, "export_ir": true,
                      "resume_native": true, "send_message": true},
     "hub_sync": true, "schema": "v2", "unmigrated": false}
  ]
}
```

One entry per agent asm knows, installed or not. For one that is not installed, `installed` is `false`, `store_root` is `null`, and `capabilities` is what asm can do for that agent once it is. `schema` and `unmigrated` are `null` for every agent but OpenCode. `hub_sync` says whether a session of that agent can be pushed to a hub and restored on another machine. Sessions are not counted here: that is a full read of every store; `asm doctor` has the counts.

## Streams

| Stream | One line is |
|---|---|
| `asm messages REF` | A message: `{"index", "role", "ts", "parts", "source_id"}`. |
| `asm events`, `asm list --follow` | `{"event": "ready"}` once, then `{"event": "added"\|"changed"\|"removed", "session": {…}}`. |
| `asm send REF MESSAGE --json` | A live event of the agent's reply: `{"event": "started"\|"text"\|"reasoning"\|"tool_call"\|"tool_result"\|"usage"\|"raw"\|"done", …}`; `done` carries `ok` and `error`. |
| `GET /api/sessions?stream=1` | `{"sessions": [...]}` per store, then `{"done": true, "problems": []}`. |

A stream is flushed a line at a time and ends when its reader goes away.

### `asm messages`

One object per line, from the session's [IR](/reference/ir-schema/), so every agent that exports has them (Claude Code, OpenCode 1.x and 2.x, jcode, Codex, Antigravity):

```json
{"index": 1, "role": "assistant", "ts": "2026-10-09T10:00:05Z",
 "parts": [{"type": "text", "text": "Running ls."}, {"type": "tool_call", "call_id": "t1", "name": "Bash", "input": {"command": "ls"}}],
 "source_id": "a1"}
```

`index` is the message's place in the whole transcript, counted before any filtering, so it stays the same however the output is narrowed. `ts` and `source_id` are left out when the agent has none. `role` is `user`, `assistant` or `system`. A part is `text`, `reasoning`, `tool_call`, `tool_result`, `file` or `agent` (a nested subagent run), exactly as in the IR.

Options are applied in this order: `--range A..B` (indexes `A` up to but not including `B`; `A..` and `..B` are open), then `--role` and `--text`, then `--tail N`.

- `--role user|assistant|system|tool`. `tool` is a message that holds a tool call or a tool result (which the IR files under the assistant and the user).
- `--text` keeps only `text` parts, and drops a message that has none left.
- `--format json` prints one array instead of lines.

### `asm events`

Polls the stores every `--interval` seconds (default 2, at least 0.5) and prints what changed. Nothing is printed for what was already there; `ready` says the first look is done, so a program can start changing things. `changed` is any difference in the session's JSON (a new message moves `updated` and `size_bytes`; a rename moves `title`). `removed` carries the session as it was last seen. A store that cannot be read is reported on stderr and its sessions are not reported removed. `--status` narrows what is watched; `--agent`, `--project` and `--include-children` apply as they do to `list`. Each look lists every store, so on a machine with thousands of sessions raise the interval.

## Listing for scripts

`list`, `search` and `children` take:

- `--status live|idle|archived`: only sessions in that state. On `search` this narrows the index, so `--limit` counts what is kept.
- `--ids-only`: one id (the native id) per line and nothing else. `search` names a session once however many hits it has.
- `-0`, `--null`: the same, ended by NUL instead of a newline, for `xargs -0`.

```sh
asm list --status live --ids-only | xargs -n1 asm show --json
asm search --status archived -0 "kiwi" | xargs -0 -n1 asm unarchive
asm list --json | jq -r '.[] | select(.state == "live") | .title'
asm children ses_71c4 --json | jq -r '.[] | "\(.depth)\t\(.id)\t\(.title)"'
```

## Web API

`asm serve` also exposes the machine routes below under the same contract. The rest of `/api/` is the web UI's own and may change with it; see the [web API reference](/reference/web-api/).

| Route | Returns |
|---|---|
| `GET /api/meta` | `{home, hostname, asm_version, json_contract_version, capabilities}`; `capabilities` is the `agents` array of `asm capabilities`. |
| `GET /api/sessions?agent=&project=&status=&q=&all=&stream=` | Sessions, as `asm list`. `status` is `live`, `idle` or `archived`; `q` is a case-insensitive substring of the title, slug, id or project path (not full text: that is `/api/search`). |
| `GET /api/sessions/{agent}/{id}` | One session. |
| `GET /api/sessions/{agent}/{id}/messages?role=&tail=&range=&text=&format=` | The `asm messages` rows as NDJSON (`application/x-ndjson`), or an array with `format=json`. |
| `GET /api/events?interval=&agent=&project=&status=&all=` | Server-sent events (`text/event-stream`): `ready`, then `added`, `changed`, `removed`. Each event's `data` is the line `asm events` prints. Comment lines (`: still here`) keep the connection open. |
| `GET /api/search?q=&agent=&project=&status=&limit=` | Search hits, as `asm search`. |

**Errors.** The routes added in 0.16 (`/api/sessions/{agent}/{id}`, its `/messages`, `/api/events`) and every authentication failure answer with the object `{"error": {"code", "message", "candidates"?}}` and the HTTP status in the table above. The `/api/session/{agent}/{id}/…` routes that existed before (`ir`, `rename`, `archive`, `unarchive`, `delete`, `move`, `import`, `send`) and `/api/sessions` keep `error` as the plain string the bundled UI shows, and add `code` (and `candidates`) beside it: `{"error": "…", "code": "ambiguous", "candidates": […]}`, with the same statuses (404, 409, 423, 501, 400, …). A client reads either with `body.code ?? body.error.code`. The remaining UI routes (hub, bulk, index) still answer a bare `{"error": "…"}`.

**A bearer token.** `asm serve --token-file PATH` (or `ASM_WEB_TOKEN` in the environment) makes every `/api/` request need `Authorization: Bearer <token>`; without it the answer is `401` with `unauthorized` and a `WWW-Authenticate: Bearer` header. The token is compared in constant time and is never logged or echoed. The UI's own files stay open, but the bundled UI sends no token and will not load data: token mode is for programs calling the API; the UI needs no token on loopback, which is still the default bind. A token does not make a network-wide bind safe to expose: it is plain HTTP.

`asm serve --port 0` lets the system choose a port; the banner on stderr prints the address actually bound.
