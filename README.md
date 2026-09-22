# asm — cross-agent session manager

One inventory, one set of verbs, for the coding-agent sessions scattered across
your machine. `asm` reads the on-disk stores that [Claude Code][cc], [OpenCode][oc],
[jcode][jc], [Codex][cx] and [Antigravity][ag] keep for themselves, presents
every session in one list, and lets
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
[cx]: https://github.com/openai/codex
[ag]: https://antigravity.google

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

asm hub serve                # on one machine: a hub, prints the join command
ASM_JOIN_TOKEN=asmj_… asm join https://hub.example.ts.net   # on each of the others
asm push --all               # upload what changed since the last push
asm remote list              # this machine and the hub, grouped by project
asm pull 7f3a1c88            # install a session from another machine here
asm push 7f3a1c88 --move     # hand a session to another machine: push, then archive here
asm daemon                   # keep pushing whatever changes, so a closed laptop loses nothing

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

| | Claude Code | OpenCode | jcode | Codex | Antigravity |
|---|---|---|---|---|---|
| List, show, search, projects | yes | yes | yes | yes | yes |
| Liveness | yes | yes | yes | no | no |
| Resume | yes | yes | yes | yes | yes |
| Export to Session IR | yes | yes | yes | yes | yes |
| Rename | yes | yes | yes | no | no |
| Archive / unarchive | yes | yes | yes | no | no |
| Delete | yes | yes | yes | no | no |
| Move to another directory | yes | yes | no | no | no |
| Import **from** (source) | yes | yes | yes | yes | yes |
| Import **into** (target) | yes | yes | no | no | no |
| Send a message into a session | yes | yes | no | yes | yes |
| Back up to a hub | yes | yes | yes | yes | yes |
| Restore from a hub | yes | yes | yes | same path | not yet |
| Verified against a real install | 2.1.234 | 1.17.18 | 0.78.0 | 0.151.0 | 1.1.16 |

Restore through a hub was verified separately, each time between two
separate homes (every store, config and asm's own state apart) on one host:

- **Claude Code 2.1.278**: pulled onto another machine at a different path,
  `claude --resume` continued it there with its conversation intact.
- **OpenCode 1.18.31**: pulled onto a machine that had never run OpenCode, at
  a different path; `opencode session list` showed it there and
  `opencode run -s` recalled the conversation.
- **jcode 0.83.0** (a session jcode wrote) and **Codex 0.151.0** (a generated
  rollout in Codex's format): the agent's own resume by id loaded the pulled
  file and appended the next turn to it, with no second copy. Neither got as
  far as a model reply, because neither was logged in where it was tested, so
  that last step is unverified.
- **Antigravity**: not attempted. `agy` had no store on the test machine.

## Replying to a session

`asm send <ref> <message>` sends a message into an existing session and
streams the reply; the TUI binds it to `c` and the web transcript has a
composer at the bottom. It goes through each agent's own headless resume —
`claude --resume … -p`, `opencode run -s`, `codex exec resume` — so the turn
is the agent's, with the agent's tools, permissions and model.

Three things worth knowing before you use it:

- **It appends to the session, it does not fork.** Verified against each
  agent: same native id, same transcript, no new row.
- **The agent can edit files in that project.** This is a real turn, not a
  read-only query, and it spends tokens.
- **Live sessions are refused.** If another terminal is driving the session
  right now, asm will not send into it — none of these CLIs promise to
  handle two writers.

jcode is missing here for a different reason than below: its stream format
has not been captured from a real run, and a normalizer written from a flag
name is a guess, not support.

Codex is read-only. Its metadata lives in `state_5.sqlite`, a schema 48 sqlx
migrations deep and still gaining columns, and codex ships no `rename`,
`archive` or `import` command to write through instead — so every mutation
would be a raw write to a moving target that its own picker might then
disagree with. Reading is safe and complete: the `threads` table drives the
listing, and asm additionally sweeps the `sessions/` tree for rollouts that
have no `threads` row, which codex hides until you resume them by id.

Antigravity is what Gemini CLI became — Google now refuses Gemini Code
Assist for individuals outright, telling you to migrate — and its store is
different in kind: one SQLite database per conversation, with every step
held as a protobuf blob against a schema Google does not publish. asm reads
the conversation instead from the JSONL rendering antigravity writes
alongside it, under `brain/<id>/.system_generated/logs/`. Same steps, same
author, no guessed field numbers.

One real gap: **an Antigravity conversation does not record its project
directory.** Neither the conversation database nor the summaries index keeps
one (`workspace_uris` is empty), and the only mapping on disk,
`cache/last_conversations.json`, remembers just the most recent conversation
per directory. Sessions it does not cover are listed with no project rather
than a guessed one.

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

## Multiple machines

One machine runs a hub; the others join it and push their sessions to it, and
any of them can pull a session another machine pushed. The hub is an archive,
not a peer: it stores what machines upload and never runs an agent, so a
headless server can be one. Machines see each other through it — nothing needs
to reach a laptop directly.

```sh
# on the hub
asm hub serve --port 7434
#   join with  ASM_JOIN_TOKEN=asmj_… asm join http://hub-host:7434

# on each machine
ASM_JOIN_TOKEN=asmj_… asm join https://hub.example.ts.net
asm push --all
asm remote list
asm pull 7f3a1c88 [--project-dir ~/src/elsewhere]
```

What crosses is each session's **own files under its original id**, so a
pulled session is the same session — `claude --resume <id>` works on the other
machine, not a copy with a new name. A pull lands at the same place relative to
your home directory, or wherever `--project-dir` says; a session that lives
somewhere else on the second machine is told so rather than duplicated.

The TUI pushes the selected (or ticked) sessions with `p`. The web UI marks
each session as synced, ahead or behind the hub, pushes and pulls from the
row, and lists what is only on the hub in the sidebar, one click to pull.

**Moving** a session is `asm push <id> --move` on the first machine — it is
archived there once the hub holds it (`asm unarchive` undoes that) — then
`asm pull <id>` on the second.

**Keeping the hub current** is `asm daemon`. Every 30 seconds (`--interval`) it
pushes each session that changed and then held still for a whole interval, so
a turn being streamed goes up when it ends. It only ever pushes: it never
writes into an agent's store, so it cannot disturb a session in use, and two
machines running it do not echo each other's work back. Run it as a service:

```ini
# ~/.config/systemd/user/asm-daemon.service
#   systemctl --user enable --now asm-daemon
[Unit]
Description=Push coding-agent sessions to the asm hub

[Service]
ExecStart=%h/.local/bin/asm daemon
Restart=on-failure
RestartSec=30

[Install]
WantedBy=default.target
```

On macOS, as `~/Library/LaunchAgents/dev.asm.daemon.plist`, then
`launchctl load` it:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>dev.asm.daemon</string>
  <key>ProgramArguments</key>
  <array><string>/Users/you/.local/bin/asm</string><string>daemon</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardErrorPath</key><string>/tmp/asm-daemon.log</string>
</dict></plist>
```

Closing the lid loses at most one interval. To lose nothing on Linux, push
once more on the way to sleep (a system unit, since `sleep.target` is one):

```ini
# /etc/systemd/system/asm-push-before-sleep.service
#   systemctl enable asm-push-before-sleep
[Unit]
Description=Push asm sessions before sleeping
Before=sleep.target

[Service]
Type=oneshot
User=you
ExecStart=/home/you/.local/bin/asm push --all
TimeoutStartSec=60

[Install]
WantedBy=sleep.target
```

**Nothing is merged, and nothing is guessed.** Every push names the hub
revision it started from, and the hub refuses one that would replace another
machine's newer copy. When both machines continued the same session, both are
told it diverged; `asm push --force` makes one copy the head, and the other
stays on the hub as a revision. Timestamps are shown in `remote list` but never
decide anything — clocks differ between machines, and a rename does not move a
session's timestamp.

Every agent's sessions are **backed up**; restore works for **Claude Code**,
**OpenCode**, **jcode** and **Codex**. A Claude transcript is append-only, so a
newer copy is appended to an older one without rewriting a byte. An OpenCode session is rows, with
its subagent sessions, todos and queued inputs: an older copy here is replaced
by the hub's only when every row here is also in the hub's copy, and it is
backed up first (OpenCode itself files it under this machine's project). A
jcode snapshot is rewritten whole every turn, so an older copy here is replaced
when its messages are a prefix of the hub's, again after a backup; it is filed
under this machine's directory and resumed by id. A Codex rollout is
append-only like a Claude transcript, but Codex writes the absolute working
directory into the conversation itself, so it is restored only where that
directory exists — the same path on both machines. Antigravity sessions are
safe on the hub, and become restorable once `agy` has been seen resuming a
restored copy.

Security, deliberately plain:

- The hub speaks HTTP and **you** provide TLS: run it on a tailnet or LAN you
  trust, or behind a reverse proxy (`tailscale serve`, Caddy). `asm join`
  refuses plain HTTP to anything outside loopback, private ranges and VPN
  ranges unless you pass `--insecure-http`.
- The join token buys a machine its **own** credential. The hub keeps only its
  hash; `asm hub revoke <machine>` shuts one laptop out without touching the
  rest, and `asm hub token --rotate` retires the join token.
- Secrets stay out of the process list: the join token comes from
  `$ASM_JOIN_TOKEN` (or `--token -`, on stdin), and asm hands every
  credential to `curl` on stdin. `curl` is run with `-q` and `--noproxy '*'`,
  so neither a `~/.curlrc` nor an `http_proxy` sees them. Its request and
  reply files live in a 0700 directory.
- Every joined machine can read every session on the hub. It is one person's
  hub, not a shared one.

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
- **A pull obeys all of the above.** It refuses a live session, never makes a
  second copy of an id, and only ever appends to a transcript that is a prefix
  of the hub's. Pushing is a read, so running sessions are backed up too.

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
be versioned and pushed to a remote of your choosing. That path leaves the
transport to you — `sync status` prints the git command to run; the hub above
is the one asm runs itself.

Joined to a hub, a machine also keeps `hub-client.json` (its credential, mode
`0600`) and `hub-state.json` (what it last agreed with the hub about). A hub
keeps its store in `hub/`: content-addressed `blobs/`, every revision of every
session under `sessions/`, and the joined machines' credential hashes.

## License

MIT or Apache-2.0, at your option — [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE).
