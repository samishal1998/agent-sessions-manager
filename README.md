# asm — cross-agent session manager

One inventory, one set of verbs, for the coding-agent sessions scattered across
your machine. `asm` reads the on-disk stores that [Claude Code][cc], [OpenCode][oc]
and [jcode][jc] keep for themselves, presents every session in one list, and lets
you rename, move, archive, delete, export — and **carry a conversation from one
agent into the other**.

```
$ asm
AGENT        ID        TITLE                                     PROJECT                          UPDATED   STATUS
claude-code  fb779332  Build cross-agent session manager system  ~/projects/rust/asm              just now  live
claude-code  20011bb2  fsl-phase-1-compiler-runtime              ~/projects/rust/fdl              6h ago    idle
opencode     ses_32d1  OpenRPC monorepo tooling plan             ~/projects/node/openrpc          2026-07-11 idle
```

Three frontends over one core: a CLI, a terminal UI (`asm tui`), and a local web
UI (`asm serve`). There is a [tour of it here][site].

[site]: https://samishal1998.github.io/agent-sessions-manager/

[cc]: https://claude.com/claude-code
[oc]: https://opencode.ai
[jc]: https://github.com/1jehuang/jcode

## Why

Agents are good at keeping their own history and bad at everything around it.
Sessions pile up in per-agent stores with per-agent identity schemes; there is no
cross-agent list, no way to retitle a session you can no longer identify, no way
to move one after you renamed the project directory, and no way to continue a
conversation in a different agent. `asm` is that layer.

## Status

| Milestone | What | State |
|---|---|---|
| M0 | Core model + Claude Code read adapter, `list`/`show`/`projects` | done |
| M1 | OpenCode adapter, management verbs, `doctor`, `worktrees`, Session IR + `export` | done |
| M2 | Cross-agent `import` (both directions, verified live) | done |
| M3 | Terminal UI | done |
| M4 | Local web UI | done |
| M5 | Full-text search, sync groundwork, docs | done |

Verified against **Claude Code 2.1.234** and **OpenCode 1.17.18** on Linux —
"verified" meaning a real session was imported and then resumed in the target
agent's own CLI with its conversation intact.

jcode 0.78.0 is supported too, with two exceptions noted in
[Agent support](#agent-support).
`asm doctor` warns when your installed versions have drifted from those.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/samishal1998/agent-sessions-manager/main/install.sh | sh
```

That downloads the binary for your platform from the latest release, checks it
against the release's `SHA256SUMS`, and installs it to `~/.local/bin`. A failed
download or a checksum mismatch stops before anything is written, so an
existing install is never left broken.

Once installed, `asm update` does the same thing without the pipeline: it
checks the release, verifies the download against `SHA256SUMS`, and replaces
the running binary in place. A bad download leaves the working one alone, and
it refuses rather than installing something it could not verify. It needs curl
or wget, the same as the installer.

| Variable | |
|---|---|
| `ASM_VERSION` | install a specific tag instead of the latest |
| `ASM_INSTALL_DIR` | where the binary goes (default `~/.local/bin`) |
| `ASM_BASE_URL` | fetch assets from a mirror or a local directory instead |
| `GITHUB_TOKEN` | only needed if the repository is ever made private again |

Releases are cut by tagging: `git tag v0.1.0 && git push origin v0.1.0` builds
Linux and macOS binaries for x86_64 and arm64, publishes them with checksums,
and is what `install.sh` reads. The workflow can also be dispatched without a
tag to dry-run the build.

## Build from source

```sh
# The web UI's assets are embedded at compile time, so build them first.
cd crates/asm-web/frontend && bun install && bun run build && cd -
cargo build --release
```

Requires a recent stable Rust (developed on 1.94) and, for the web UI only,
Node or Bun. The binary is self-contained: SQLite is bundled, and the frontend
is embedded.

## Commands

```sh
asm                          # interactive TUI on a terminal; plain table when piped
asm list --agent opencode    # filter by agent, --project, --all (include subagents)
asm projects --worktrees     # projects are repositories; show each one's checkouts
asm show 4c93a826            # metadata card; refs are unique id prefixes or agent:prefix
asm resume 4c93a826          # hands off to the native agent, in the right directory

asm rename 4c93a826 "New title"
asm move 4c93a826 ~/projects/renamed-dir
asm archive 4c93a826         # Claude: moved into asm's archive; OpenCode: native flag
asm unarchive 4c93a826
asm delete 4c93a826          # backs everything up first

asm import 4c93a826 --to opencode          # the flagship
asm import 4c93a826 --to opencode --dry-run --mode seed
asm export 4c93a826 -o session.ir.json     # versioned, agent-neutral JSON

asm search "path encoder"    # full-text across every transcript, every agent
asm search --agent opencode "jsonrpc"
asm index                    # refresh the index and report on it

asm update                   # replace this binary with the latest release
asm update --check           # just say whether there is a newer one

asm doctor                   # store health, duplicate ids, stale locks
asm worktrees                # git worktrees of a repo, with the sessions in each
asm sync init && asm sync status
asm serve                    # web UI on http://127.0.0.1:7433
asm serve --port 8080
asm serve --host 0.0.0.0     # every interface — read the warning it prints
```

Every command takes `--json`.

### In the web UI

`asm serve` gives the same verbs a mouse: sessions as cards with the agent shown
as an icon (its name is one hover away), per-row actions, multi-select filtering
by agent, project filters in the sidebar, full-text search with highlighted
snippets, and a transcript panel that renders text, reasoning, tool calls and
expandable tool output. Tick several sessions and the bar above the list
archives, imports, moves, exports or deletes the whole set at once, reporting
per session what did not work. It is responsive down to a phone, where the sidebar
becomes an overlay and the transcript takes the full screen.

To see it without a browser — or to re-check a change — `bun run shots` in
`crates/asm-web/frontend` screenshots the running UI at three widths and fails
on any console error (needs `bunx playwright install chromium` once).

### In the TUI

Every per-session verb is a keystroke, so the terminal UI is not a read-only
view of the CLI:

| Key | |
|---|---|
| `⏎` | resume in the native agent (the TUI steps aside and comes back) |
| `r` `a` `d` | rename · archive/unarchive · delete (confirmed, backed up) |
| `m` `i` `e` | move to another project · import into the other agent · export IR |
| `␣` `*` | tick this session · tick everything the filter shows |
| `s` `/` | full-text search across transcripts · filter the list |
| `D` | store health (the same report as `asm doctor`) |
| `⇥` `R` `q` | focus the transcript · rescan · quit |

With anything ticked, `a` `d` `m` `e` `i` run over the whole selection instead
of the row under the cursor; with nothing ticked they behave as before. A batch
attempts every session, so one failure cannot strand the rest, and what did not
work is listed per session afterwards.

Bulk unarchive only reaches OpenCode sessions, because only they stay listed
once archived — Claude and jcode sessions leave their store for asm's archive
and are restored by reference with `asm unarchive <id>`. That is the same
division the single-session verb has always had.

## Importing across agents

`asm import` converts through a documented intermediate representation (see
[docs/ir-schema.md](docs/ir-schema.md)) and writes a **native** session in the
target agent, so the target's own picker lists it and its own resume works:

```
$ asm import 36405fad --to opencode
Imported as opencode:ses_7bdfed0f167b6c507797045ba6.

Loss report:
2 of 3 messages converted
1 opaque reasoning blocks dropped (provider-bound; summaries only)
conversation re-attributed to openai/gpt-5.6-sol (the target install's last-used model)

Resume with: opencode -s ses_7bdfed0f167b6c507797045ba6   (run in the project dir)
```

Two modes:

- `--mode full` (default) translates the transcript into the target's native
  records. Highest fidelity, and the most exposed to the target's format
  changing under it.
- `--mode seed` distills the session into a narrative handoff document that
  becomes the first message of a fresh session. Lower fidelity, essentially
  immune to format churn.

Imports are **idempotent**: target ids are derived deterministically from the
source session, so re-importing reports `In sync` instead of creating a
duplicate.

Some things cannot cross and `asm` says so rather than pretending:
provider-signed reasoning blocks, tools the target does not have (the names are
kept verbatim so the history still reads), and nested subagent transcripts.

## Agent support

| | Claude Code | OpenCode | jcode |
|---|---|---|---|
| List, show, search, projects | yes | yes | yes |
| Liveness | yes | yes | yes |
| Resume | yes | yes | yes |
| Export to Session IR | yes | yes | yes |
| Rename | yes | yes | yes |
| Archive / unarchive | yes | yes | yes |
| Delete | yes | yes | yes |
| Move to another directory | yes | yes | no |
| Import **from** (source) | yes | yes | yes |
| Import **into** (target) | yes | yes | no |
| Verified against a real install | 2.1.234 | 1.17.18 | 0.78.0 |

Two jcode verbs are missing, for the same reason: its whole session — metadata
*and* the entire conversation — is one JSON document, and jcode ships no
command for either job. Moving a session means editing `working_dir` inside
that document, and importing means writing a whole one. Rewriting another
tool's file to change one field, with no sanctioned path and no way to check
the result, is not a trade this project makes. Everything else goes through
jcode's own `session rename` or moves whole files without touching their
contents.

`asm doctor --json` reports each agent's capabilities, and the web UI greys out
what an agent cannot do, so the limits are visible rather than discovered by
error.

## What counts as a project

A project is a **git repository**, not a directory. Every worktree of a
repository is the same project, and so is a session started in a subdirectory
of one — an agent whose working directory wandered into `crates/foo` has not
started working on a different codebase. Sessions from different agents in the
same repository share one project too.

Identity comes from `git rev-parse --git-common-dir`, which every worktree of a
repository agrees on. Directories outside any repository stand alone. Worktrees
with no sessions are still listed, so an idle checkout is visible rather than
missing.

## Reading transcripts

Agents embed XML-ish envelopes in message text — injected context, slash-command
echoes, background-task notifications, subagent results, tool errors. The web UI
parses the ones it knows into labelled, collapsible blocks, and renders anything
else as a nested tree rather than a wall of angle brackets.

Detection is deliberately narrow, because transcripts are mostly tool output and
tool output is mostly code: a scan of the transcripts on this machine found 400
distinct "tags", nearly all of them generics like `Vec<T>` and `Option<String>`.
So a tag counts only if it is known or lowercase-with-a-separator, starts a line,
and has a matching close tag. Everything else stays plain text, and nothing is
ever dropped.

## Search

`asm search` runs SQLite FTS5 over every message of every session, in an index
kept in asm's own data directory — the agents' stores are never written to.

The index is incremental: each session carries an opaque content fingerprint,
and only sessions whose fingerprint moved are re-extracted. On this machine, 30
sessions index in about 5 seconds cold and refresh in ~0.15s warm, so
`asm search` refreshes by default; pass `--no-refresh` to skip it.

Fingerprints are per-agent because the naive choice is wrong for OpenCode: its
`session.time_updated` lags behind its own message rows, so keying on it would
silently lose streamed tool output. File-backed sessions key on size and mtime;
row-backed ones on the message table's own count and high-water mark.

**Subagent transcripts are indexed**, which matters more than it sounds — in
delegating sessions they are the majority of the searchable text, so indexing
only the parent conversation hides most of the corpus.

The index is disposable: anything unreadable, or written by a different schema
version, is rebuilt rather than migrated. Tool *inputs* are indexed in full
(commands, paths, patterns); tool *outputs* only in part, since they dominate
transcript bulk. `asm index` also reclaims space after re-extraction (FTS5's
`optimize` restructures but does not return pages to the filesystem; `VACUUM`
does).

`s` opens the same search in the TUI, and the web UI has a transcript search box
next to its filter.

Archived sessions stay searchable even though they have left their agent's
store and no longer appear in `asm list`; results mark them `(archived)` so it
is clear they need restoring before they can be resumed.

### Known limitations

- Deleting a session's rows from the FTS table is a scan of that table, so a
  full rebuild is linear in sessions × messages. At personal scale (seconds)
  this is fine; it would need a rowid map to scale further.
- OpenCode staleness is judged by the session's `time_updated`. If OpenCode
  ever writes a message without bumping it, that session would look unchanged.

## Safety model

This tool writes into stores owned by other programs, so the rules are strict:

- **Never touch a live session.** Mutating a session whose agent is running is
  refused outright.
- **Never rewrite transcript bytes.** Claude Code's background jobs hold raw
  byte offsets into transcript files; `asm` only appends or renames whole files.
- **Never duplicate a session id.** Claude Code's cross-project `--resume`
  hard-fails when an id exists in two project directories, so `asm` moves rather
  than copies and refuses an import that would collide. `asm doctor` reports
  pre-existing duplicates.
- **Back up before destroying.** `asm delete` copies every affected path into
  `~/.local/share/asm/backups/<agent>/<id>/<timestamp>/` first.
- **Never write into a busy store.** OpenCode mutations are refused while an
  OpenCode instance holds its lock directory.
- **Only ever write through sanctioned paths where they exist** — imports into
  OpenCode go through `opencode import`, not raw SQL.

The web UI has **no authentication** and is a personal dashboard, not a service.
It binds `127.0.0.1` by default. `--host` accepts any IP or hostname, and
`0.0.0.0` / `::` bind every interface — but anyone who can reach that address
can read every conversation and rename, move, archive, import, or delete
sessions, so `serve` prints a warning whenever it binds outside loopback. On a
machine with a public IP, "every interface" means the internet, not just your
LAN. Mutating endpoints do reject cross-origin browser requests, but that only
stops other web pages; it does not stop a direct request.

## Layout

```
crates/asm-core   domain model, agent adapters, Session IR, import engine  (no UI deps)
crates/asm-cli    clap frontend
crates/asm-tui    ratatui terminal UI
crates/asm-web    axum API + embedded Vue frontend
crates/asm        the single `asm` binary
```

Architecture and the per-agent format details worth knowing before touching an
adapter are in [docs/agent-formats.md](docs/agent-formats.md).

## Data asm writes

Only inside its own directory (`$XDG_DATA_HOME/asm`, override with `ASM_DATA_DIR`):

```
archive/<agent>/<id>/   archived sessions (manifest.json + native/)
backups/<agent>/<id>/   pre-delete backups, timestamped
index/sessions.db       the search index (derived; safe to delete)
```

`asm sync init` turns `archive/` into a git repository so archived sessions can
be versioned and pushed to a remote of your choosing. asm does not manage the
transport — `sync status` prints the git command to run.

## License

MIT or Apache-2.0, at your option — [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE).
