#!/usr/bin/env python3
"""Build a synthetic agent-session store so the landing page can show the
real UI without putting anybody's actual transcripts on the internet."""
import json, os, shutil, sqlite3, subprocess, sys, uuid
from pathlib import Path

DEMO = Path(sys.argv[1]).resolve()
SCHEMA = Path(sys.argv[2])           # schema-only dump of a real opencode.db
if DEMO.exists(): shutil.rmtree(DEMO)

HOME = DEMO
CODE = HOME / "code"
CLAUDE = HOME / ".claude"
JCODE = HOME / ".jcode"
OCDATA = HOME / ".local/share/opencode"
CODEX = HOME / ".codex"
AGY = HOME / ".gemini/antigravity-cli"
for p in (CODE, CLAUDE / "projects", JCODE / "sessions", OCDATA,
          CODEX / "sessions", AGY / "conversations", AGY / "cache"):
    p.mkdir(parents=True, exist_ok=True)

def git(*a, cwd):
    subprocess.run(["git", *a], cwd=cwd, check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

def repo(name, files):
    d = CODE / name
    d.mkdir(parents=True, exist_ok=True)
    for rel, body in files.items():
        f = d / rel; f.parent.mkdir(parents=True, exist_ok=True); f.write_text(body)
    git("init", "-q", "-b", "main", cwd=d)
    git("config", "user.email", "dev@example.com", cwd=d)
    git("config", "user.name", "Dev", cwd=d)
    git("add", "-A", cwd=d)
    git("commit", "-qm", "initial", cwd=d)
    return d

mercury = repo("mercury", {
    "Cargo.toml": '[package]\nname = "mercury"\nversion = "0.4.1"\n',
    "src/main.rs": "fn main() { println!(\"mercury\"); }\n",
    "src/pool.rs": "pub struct Pool;\n",
})
atlas = repo("atlas-web", {
    "package.json": '{"name":"atlas-web","version":"2.3.0"}\n',
    "src/app.ts": "export const app = {};\n",
})
# a linked worktree — a project is the repository, not the directory
wt = CODE / "mercury-tls"
git("worktree", "add", "-q", "-b", "tls-rewrite", str(wt), cwd=mercury)
notes = CODE / "notes"; notes.mkdir(exist_ok=True)   # not a repo at all
(notes / "postmortem.md").write_text("# Incident 2026-08-04\n")

MS = 1_000
def ts(day, hour=10, minute=0):
    import datetime
    return datetime.datetime(2026, 8, day, hour, minute, tzinfo=datetime.timezone.utc)
def iso(d): return d.strftime("%Y-%m-%dT%H:%M:%S.000Z")
def epoch_ms(d): return int(d.timestamp() * 1000)

# ---------------------------------------------------------------- claude code
def claude_session(sid, cwd, branch, title, model, day, turns):
    enc = str(cwd).replace("/", "-")
    d = CLAUDE / "projects" / enc; d.mkdir(parents=True, exist_ok=True)
    lines = []
    t = ts(day)
    def rec(**kw):
        nonlocal t
        import datetime
        t = t + datetime.timedelta(minutes=3)
        base = {"cwd": str(cwd), "gitBranch": branch, "version": "2.1.234",
                "sessionId": sid, "timestamp": iso(t), "uuid": str(uuid.uuid4())}
        base.update(kw); lines.append(json.dumps(base))
    for turn in turns:
        if turn[0] == "user":
            rec(type="user", message={"role": "user", "content": turn[1]})
        else:
            content = []
            for part in turn[1]:
                content.append(part)
            rec(type="assistant", message={"role": "assistant", "model": model,
                                           "content": content})
    lines.append(json.dumps({"type": "ai-title", "aiTitle": title,
                             "sessionId": sid, "timestamp": iso(t)}))
    (d / f"{sid}.jsonl").write_text("\n".join(lines) + "\n")

claude_session(
    "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43", mercury, "main",
    "Trace the connection-pool leak under sustained load", "claude-opus-4-6", 12,
    [("user", "Under sustained load mercury's connection pool grows without bound and never returns to baseline. Find out why."),
     ("assistant", [
        {"type": "thinking", "thinking": "A pool that grows but never shrinks usually means checked-out connections are not being returned on every path — most often an early return or a `?` that skips the guard's drop."},
        {"type": "text", "text": "Let me look at how connections are checked back in."},
        {"type": "tool_use", "id": "t1", "name": "Grep", "input": {"pattern": "fn checkout|fn release|impl Drop", "path": "src/pool.rs"}},
     ]),
     ("user", [{"type": "tool_result", "tool_use_id": "t1", "content": "src/pool.rs:41: pub fn checkout(&self) -> Result<Conn, PoolError>\nsrc/pool.rs:77: fn release(&self, c: Conn)\n"}]),
     ("assistant", [
        {"type": "text", "text": "There's no `impl Drop` for `Conn` — release is only ever called explicitly. Any error path between checkout and release leaks the connection permanently."},
        {"type": "tool_use", "id": "t2", "name": "Read", "input": {"file_path": "src/pool.rs"}},
     ]),
     ("user", [{"type": "tool_result", "tool_use_id": "t2", "content": "…\n    let conn = pool.checkout()?;\n    let resp = upstream.send(req).await?;   // <- early return leaks `conn`\n    pool.release(conn);\n…"}]),
     ("assistant", [
        {"type": "text", "text": "Confirmed. `upstream.send(req).await?` returns early on any upstream timeout, and `pool.release(conn)` below it never runs. Under load, timeouts are exactly what you get — so the pool bleeds one connection per timed-out request and never recovers.\n\nThe fix is to make the return automatic rather than manual: give `Conn` a `Drop` that hands the connection back, so no error path can skip it."},
     ]),
    ])

claude_session(
    "b21e6f04-93cc-4d17-8e52-11aa7d3f9c60", wt, "tls-rewrite",
    "Port the retry middleware onto tower::Layer", "claude-opus-4-6", 15,
    [("user", "Port the retry middleware to a tower Layer so it composes with the rest of the stack."),
     ("assistant", [
        {"type": "text", "text": "Starting with the current middleware so the retry semantics survive the port."},
        {"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "src/retry.rs"}},
     ]),
     ("user", [{"type": "tool_result", "tool_use_id": "t1", "content": "pub async fn with_retry(req: Request) -> Result<Response> { … }\n"}]),
     ("assistant", [{"type": "text", "text": "This retries on connect errors but not on 503s, and the backoff is fixed rather than exponential. I'll preserve both behaviours exactly in the Layer and change them separately, so the port stays reviewable."}]),
    ])

claude_session(
    "4c0d9a71-5b83-42fe-a6d1-0e39b8c47f25", atlas, "main",
    "Rewrite the auth guard tests around the new session cookie",
    "claude-sonnet-4-6", 16,
    [("user", "The auth guard tests still assume the old bearer header. Rewrite them for the session cookie."),
     ("assistant", [{"type": "text", "text": "I'll rewrite them against the cookie, keeping one test on the bearer path since the mobile client still sends it."}]),
    ])

# ------------------------------------------------------------------ open code
db = OCDATA / "opencode.db"
conn = sqlite3.connect(db)
conn.executescript(SCHEMA.read_text())
def oc_project(pid, worktree, day):
    conn.execute("INSERT INTO project (id, worktree, vcs, time_created, time_updated, sandboxes) VALUES (?,?,?,?,?,?)",
                 (pid, str(worktree), "git", epoch_ms(ts(day)), epoch_ms(ts(day)), "[]"))
def oc_session(sid, pid, slug, directory, title, day, model, msgs, archived=False):
    conn.execute("""INSERT INTO session (id, project_id, slug, directory, title, version,
                    time_created, time_updated, time_archived, agent, model,
                    cost, tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write)
                    VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)""",
                 (sid, pid, slug, str(directory), title, "1.17.18",
                  epoch_ms(ts(day)), epoch_ms(ts(day, 17)),
                  epoch_ms(ts(day, 18)) if archived else None,
                  "build", json.dumps({"id": model, "providerID": "openai"}),
                  0.41, 184320, 21044, 5120, 942100, 61230))
    for i, (role, parts) in enumerate(msgs):
        mid = f"msg_{sid[4:12]}{i:02d}"
        conn.execute("INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?,?,?,?,?)",
                     (mid, sid, epoch_ms(ts(day, 10 + i)), epoch_ms(ts(day, 10 + i)),
                      json.dumps({"id": mid, "role": role, "sessionID": sid})))
        for j, part in enumerate(parts):
            pidp = f"prt_{sid[4:12]}{i:02d}{j:02d}"
            part = {"id": pidp, "messageID": mid, "sessionID": sid, **part}
            conn.execute("INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?,?,?,?,?,?)",
                         (pidp, mid, sid, epoch_ms(ts(day, 10 + i)), epoch_ms(ts(day, 10 + i)),
                          json.dumps(part)))
oc_project("a41f9c22bd7e4508aa19cf3d6b2e75810cc4de13", atlas, 10)
oc_session("ses_71bc0e4faa2196KmTqRvBnLd2", "a41f9c22bd7e4508aa19cf3d6b2e75810cc4de13",
           "amber-heron", atlas, "Design the billing webhook retry schema", 14, "gpt-5.6-sol",
           [("user", [{"type": "text", "text": "Design the schema for billing webhook retries. Stripe can deliver the same event more than once and out of order."}]),
            ("assistant", [{"type": "text", "text": "Out-of-order plus duplicate delivery means the table has to be idempotent on the event id and monotonic on the event's own timestamp — not on arrival time."},
                           {"type": "tool", "tool": "write", "state": {"status": "completed", "title": "migrations/007_webhook_events.sql", "input": {"filePath": "migrations/007_webhook_events.sql"}, "output": "created 24 lines"}},
                           {"type": "text", "text": "`event_id` is the primary key, so a duplicate delivery is a no-op insert rather than a second charge. `occurred_at` comes from the payload, and the handler ignores any event older than the last one applied for that subscription."}]),
           ])
oc_session("ses_9d2a55e1cc03b8XyWfNmQpEr7", "a41f9c22bd7e4508aa19cf3d6b2e75810cc4de13",
           "quiet-lantern", atlas, "Migrate the dashboard build to Vite 7", 11, "gpt-5.6-sol",
           [("user", [{"type": "text", "text": "Migrate the dashboard off webpack onto Vite 7."}]),
            ("assistant", [{"type": "text", "text": "The blocker will be the two webpack-specific loaders. Checking what actually depends on them first."},
                           {"type": "tool", "tool": "bash", "state": {"status": "completed", "title": "rg -l 'raw-loader|svg-inline-loader' src", "input": {"command": "rg -l 'raw-loader|svg-inline-loader' src"}, "output": "src/icons/index.ts\nsrc/templates/mail.ts"}}]),
           ], archived=True)
conn.commit(); conn.close()

# ---------------------------------------------------------------------- jcode
def jcode_session(slug, cwd, title, day, model, messages):
    snap = {"id": f"session_{slug}", "title": title, "created_at": iso(ts(day)),
            "updated_at": iso(ts(day, 16)), "messages": messages, "model": model,
            "working_dir": str(cwd), "short_name": slug, "is_debug": False}
    (JCODE / "sessions" / f"session_{slug}.json").write_text(json.dumps(snap, indent=1))
    (JCODE / "sessions" / f"session_{slug}.journal.jsonl").write_text(
        "\n".join(json.dumps({"op": "append", "seq": i}) for i in range(len(messages))) + "\n")

jcode_session("bright-canyon", notes, "Draft the incident postmortem for the 04 Aug outage",
              13, "claude-opus-4-6",
              [{"role": "user", "content": "Draft a postmortem for the Aug 4 outage. Blameless, and lead with the timeline."},
               {"role": "assistant", "content": "Leading with the timeline means the reader sees the sequence before any interpretation of it, which is what keeps a postmortem blameless in practice rather than just in tone."}])
jcode_session("calm-meridian", mercury, "Benchmark the new pool against the old one",
              16, "claude-opus-4-6",
              [{"role": "user", "content": "Benchmark the Drop-based pool against the old explicit-release one under 5k concurrent connections."},
               {"role": "assistant", "content": "Running both for ten minutes at 5k concurrent so the leak has time to show up in the old one."}])

# ---------------------------------------------------------------------- codex
# Two halves, exactly as codex keeps them: a rollout JSONL per session, and a
# `threads` row in state_5.sqlite — the row its own picker reads.
cx = sqlite3.connect(CODEX / "state_5.sqlite")
cx.executescript("""
CREATE TABLE threads (
    id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL,
    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
    source TEXT NOT NULL, model_provider TEXT NOT NULL, cwd TEXT NOT NULL,
    title TEXT NOT NULL, sandbox_policy TEXT NOT NULL, approval_mode TEXT NOT NULL,
    tokens_used INTEGER NOT NULL DEFAULT 0, has_user_event INTEGER NOT NULL DEFAULT 0,
    archived INTEGER NOT NULL DEFAULT 0, archived_at INTEGER,
    git_sha TEXT, git_branch TEXT, git_origin_url TEXT,
    cli_version TEXT NOT NULL DEFAULT '', model TEXT, preview TEXT NOT NULL DEFAULT '');
CREATE TABLE thread_spawn_edges (
    parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL PRIMARY KEY,
    status TEXT NOT NULL);
""")


def codex_session(tid, cwd, branch, title, day, model, items, tokens=0):
    import datetime
    start = ts(day, 14)
    day_dir = CODEX / "sessions" / f"{start:%Y/%m/%d}"
    day_dir.mkdir(parents=True, exist_ok=True)
    # The filename stamp is local time with no offset, which is exactly why
    # asm reads times from the record and the database instead of the name.
    path = day_dir / f"rollout-{start:%Y-%m-%dT%H-%M-%S}-{tid}.jsonl"

    lines, ordinal, t = [], 0, start

    def rec(kind, payload):
        nonlocal ordinal, t
        t = t + datetime.timedelta(seconds=20)
        lines.append(json.dumps({"timestamp": iso(t), "ordinal": ordinal,
                                 "type": kind, "payload": payload}))
        ordinal += 1

    rec("session_meta", {"session_id": tid, "id": tid, "timestamp": iso(start),
                         "cwd": str(cwd), "originator": "codex_cli_rs",
                         "cli_version": "0.148.0", "source": "cli",
                         "model_provider": "openai"})
    rec("event_msg", {"type": "task_started", "turn_id": "turn-1"})
    for item in items:
        rec("response_item", item)
    rec("event_msg", {"type": "token_count",
                      "info": {"total_token_usage": {
                          "input_tokens": tokens,
                          "cached_input_tokens": int(tokens * 0.8),
                          "output_tokens": 480}}})
    path.write_text("\n".join(lines) + "\n")

    cx.execute(
        "INSERT INTO threads (id, rollout_path, created_at, updated_at, source,"
        " model_provider, cwd, title, sandbox_policy, approval_mode, tokens_used,"
        " archived, git_branch, cli_version, model)"
        " VALUES (?,?,?,?,'cli','openai',?,?,'{}','on-request',?,0,?,'0.148.0',?)",
        (tid, str(path), int(start.timestamp()), int(t.timestamp()),
         str(cwd), title, tokens, branch, model))


codex_session(
    "01a020ab-9a2c-71a1-8384-1e1058de5553", mercury, "main",
    "Add a regression test for the pool leak", 17, "gpt-5.6-sol",
    [{"type": "message", "id": "msg_u1", "role": "user",
      "content": [{"type": "input_text",
                   "text": "Add a regression test that fails on the old pool "
                           "and passes on the new one."}]},
     {"type": "reasoning", "id": "rs_1", "summary": [],
      "encrypted_content": "gAAAAAB-provider-bound-and-unreadable"},
     {"type": "custom_tool_call", "id": "ctc_1", "call_id": "call_1", "name": "exec",
      "input": "cargo test --test pool -- --nocapture"},
     {"type": "custom_tool_call_output", "id": "ctco_1", "call_id": "call_1",
      "output": [{"type": "input_text",
                  "text": "running 1 test\ntest pool::leaks_on_timeout ... FAILED\n"}]},
     {"type": "message", "id": "msg_a1", "role": "assistant",
      "content": [{"type": "output_text",
                   "text": "The test drives 200 requests through an upstream that "
                           "times out half of them, then asserts the pool is back at "
                           "its idle size. It fails on the explicit-release pool with "
                           "100 connections still checked out, and passes once `Conn` "
                           "has a `Drop` that returns them."}]}],
    tokens=44773)

codex_session(
    "01a020c4-71bd-7440-b2f1-8ce905d6a3f7", atlas, "main",
    "Audit the bundle for duplicate dependencies", 18, "gpt-5.6-sol",
    [{"type": "message", "id": "msg_u1", "role": "user",
      "content": [{"type": "input_text",
                   "text": "The bundle is 400kB heavier than last release. Find out "
                           "what got pulled in twice."}]},
     {"type": "message", "id": "msg_a1", "role": "assistant",
      "content": [{"type": "output_text",
                   "text": "Two copies of `date-fns` — 3.x hoisted at the root and "
                           "2.x nested under the charting library, which pins it. "
                           "That accounts for 310kB of the increase on its own."}]}],
    tokens=18240)
cx.commit()
cx.close()

# ---------------------------------------------------------------- antigravity
# One SQLite database per conversation, with the steps also written out as
# JSONL — the copy asm reads, rather than decoding the protobuf blobs.
def agy_conversation(cid, cwd, title, day, steps, workspace=True):
    import datetime
    # asm only ever sizes the database; the WAL is where most of a fresh
    # conversation actually lives, so a size that ignored it would mislead.
    (AGY / "conversations" / f"{cid}.db").write_bytes(
        b"SQLite format 3\x00" + bytes(49_000))
    (AGY / "conversations" / f"{cid}.db-wal").write_bytes(bytes(210_000))

    logs = AGY / "brain" / cid / ".system_generated/logs"
    logs.mkdir(parents=True, exist_ok=True)
    lines, t = [], ts(day, 15)
    for i, (kind, content) in enumerate(steps):
        t = t + datetime.timedelta(minutes=2)
        # Antigravity wraps what the user typed in envelopes meant for the
        # model; asm keeps only the request itself.
        if kind == "USER_INPUT":
            body = (f"<USER_REQUEST>\n{content}\n</USER_REQUEST>\n"
                    f"<ADDITIONAL_METADATA>\nThe current local time is: {iso(t)}.\n"
                    "</ADDITIONAL_METADATA>")
        else:
            body = content
        lines.append(json.dumps({
            "step_index": i,
            "source": {"USER_INPUT": "USER_EXPLICIT",
                       "PLANNER_RESPONSE": "MODEL"}.get(kind, "SYSTEM"),
            "type": kind, "status": "DONE", "created_at": iso(t), "content": body,
        }))
    (logs / "transcript.jsonl").write_text("\n".join(lines) + "\n")
    return {"cid": cid, "cwd": cwd if workspace else None, "title": title,
            "steps": len(steps), "updated": iso(t)}


agy = [
    agy_conversation(
        "3f6bfd66-67b6-4c35-9065-5316a755e9af", atlas,
        "Dashboard Chart Accessibility Pass", 19,
        [("USER_INPUT",
          "The dashboard charts fail contrast checks in light mode. Fix the "
          "palette without changing the series order."),
         ("PLANNER_RESPONSE",
          "Series order is load-bearing here — the legend and the CSV export both "
          "depend on it — so I am changing only the colours assigned to each "
          "position, not the positions themselves."),
         ("SYSTEM_MESSAGE",
          "The following is a <SYSTEM_MESSAGE> not actually sent by the user."),
         ("PLANNER_RESPONSE",
          "Four of the seven series were below 3:1 against the light background. "
          "The replacements hold 4.5:1 in both themes and stay distinguishable "
          "under deuteranopia.")]),
    # No workspace recorded. last_conversations.json keeps only the newest
    # conversation per directory, so older ones have no project anywhere on
    # disk — and asm says so rather than guessing one.
    agy_conversation(
        "5c81b0e2-3a47-4c19-9f6d-2b7188ea4013", notes, "Release Notes Draft", 14,
        [("USER_INPUT",
          "Turn the merged PR titles for 0.4.1 into release notes a user would read."),
         ("PLANNER_RESPONSE",
          "Grouping by what changed for the reader rather than by subsystem, and "
          "dropping the six PRs that are pure refactors with no visible effect.")],
        workspace=False),
]
(AGY / "cache/conversation_metadata.json").write_text(json.dumps({
    "conversations": {
        c["cid"]: {
            "summary": {"ID": c["cid"], "Title": "", "Preview": c["title"],
                        "NumSteps": c["steps"], "Loaded": True,
                        "UpdatedAt": c["updated"], "WorkspaceURIs": None,
                        "AppDataDir": "antigravity-cli",
                        "ProjectID": "default-cli-project", "AgentName": ""},
            "is_internal": False, "last_modified_time": c["updated"]}
        for c in agy
    }
}, indent=2))
(AGY / "cache/last_conversations.json").write_text(json.dumps(
    {str(c["cwd"]): c["cid"] for c in agy if c["cwd"]}, indent=2))

print(f"demo home: {HOME}")

