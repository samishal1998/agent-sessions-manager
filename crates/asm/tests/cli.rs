//! The machine interface, through the built binary: `--json` on every
//! command, structured errors and exit statuses, the listing flags, messages,
//! children and the change stream. Every run is `env -i` with HOME, asm's data
//! directory, the XDG directories and every agent store pointed into a
//! temporary home; nothing of the real environment is read or written.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

const CLAUDE_A: &str = "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43";
const CLAUDE_B: &str = "7f3a1c99-2d4e-4b91-9a05-6c7e8f201b44";
const OC_ROOT: &str = "ses_r001aaaaaaaaaaaa";
const OC_KID: &str = "ses_k002bbbbbbbbbbbb";
const OC_GRAND: &str = "ses_g003cccccccccccc";
const OC_OTHER: &str = "ses_o004dddddddddddd";

struct Home {
    dir: tempfile::TempDir,
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Out {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {:?}\nstderr: {}", self.stdout, self.stderr))
    }

    fn lines(&self) -> Vec<Value> {
        self.stdout.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l:?}"))).collect()
    }

    /// The `error` object, when this is a structured failure: exactly one
    /// JSON object on stderr and nothing on stdout.
    fn error(&self) -> Value {
        assert!(self.stdout.is_empty(), "a failure prints nothing on stdout: {:?}", self.stdout);
        let doc: Value = serde_json::from_str(self.stderr.trim())
            .unwrap_or_else(|e| panic!("stderr is not one JSON object ({e}): {:?}", self.stderr));
        assert_eq!(doc.as_object().unwrap().len(), 1, "{doc}");
        doc["error"].clone()
    }
}

fn ids(value: &Value) -> Vec<String> {
    value.as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap().to_string()).collect()
}

impl Home {
    fn new(v2: bool) -> Home {
        let home = Home { dir: tempfile::tempdir().unwrap() };
        home.fixtures(v2);
        home
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.dir.path().join(sub)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_asm"));
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.path("home"))
            .env("ASM_DATA_DIR", self.path("data"))
            .env("XDG_CONFIG_HOME", self.path("cfg"))
            .env("XDG_DATA_HOME", self.path("xdg"))
            .env("XDG_STATE_HOME", self.path("xst"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("CLAUDE_CONFIG_DIR", self.path("claude"))
            .env("CODEX_HOME", self.path("codex"))
            .env("JCODE_HOME", self.path("jcode"))
            .env("ASM_ANTIGRAVITY_ROOT", self.path("ag"))
            .env("ASM_MACHINE_UID", "cli-test-machine")
            .args(args)
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str]) -> Out {
        let out = self.command(args).output().unwrap();
        Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn fixtures(&self, v2: bool) {
        let project = self.path("home/code/app");
        fs::create_dir_all(&project).unwrap();
        let cwd = project.display().to_string();
        let dir = self.path("claude/projects").join(cwd.replace('/', "-"));
        fs::create_dir_all(&dir).unwrap();
        let rec = |id: &str, uuid: &str, parent: Option<&str>, ts: &str, kind: &str, message: Value| {
            serde_json::json!({"type": kind, "uuid": uuid, "parentUuid": parent, "isSidechain": false, "timestamp": ts,
                "cwd": cwd, "sessionId": id, "version": "2.1.230", "message": message})
            .to_string()
        };
        let a = [
            rec(CLAUDE_A, "u1", None, "2026-10-09T10:00:00.000Z", "user", serde_json::json!({"role": "user", "content": "list the files please"})),
            rec(CLAUDE_A, "a1", Some("u1"), "2026-10-09T10:00:05.000Z", "assistant", serde_json::json!({"id": "m1", "type": "message", "role": "assistant", "model": "claude-fable-5",
                "content": [{"type": "text", "text": "Running ls."}, {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls"}}]})),
            rec(CLAUDE_A, "u2", Some("a1"), "2026-10-09T10:00:06.000Z", "user", serde_json::json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "a.txt b.txt"}]})),
            rec(CLAUDE_A, "a2", Some("u2"), "2026-10-09T10:00:09.000Z", "assistant", serde_json::json!({"id": "m2", "type": "message", "role": "assistant", "model": "claude-fable-5", "content": [{"type": "text", "text": "Two files."}]})),
        ];
        fs::write(dir.join(format!("{CLAUDE_A}.jsonl")), a.join("\n") + "\n").unwrap();
        // Shares its first six characters with the first: an ambiguous prefix.
        let b = rec(CLAUDE_B, "u1", None, "2026-10-09T11:00:00.000Z", "user", serde_json::json!({"role": "user", "content": "second claude session"}));
        fs::write(dir.join(format!("{CLAUDE_B}.jsonl")), b + "\n").unwrap();

        let oc = self.path("xdg/opencode");
        fs::create_dir_all(&oc).unwrap();
        let conn = rusqlite::Connection::open(oc.join("opencode.db")).unwrap();
        let rows = [(OC_ROOT, None, "Tree root"), (OC_KID, Some(OC_ROOT), "Review docs (@general subagent)"),
            (OC_GRAND, Some(OC_KID), "Explore queue (@explore subagent)"), (OC_OTHER, None, "Another root")];
        let t = 1_800_000_000_000_i64;
        if v2 {
            conn.execute_batch(include_str!("fixtures/opencode_v2.sql")).unwrap();
            conn.execute("INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) VALUES ('prj_1', ?1, '[]', 1, 1)", [&cwd]).unwrap();
            for (i, (id, parent, title)) in rows.iter().enumerate() {
                let at = t - i as i64 * 10_000;
                conn.execute(
                    "INSERT INTO session_v2 (id, project_id, parent_id, slug, directory, title, version, agent, model, cost, tokens_input, tokens_output,
                        tokens_cache_read, tokens_cache_write, time_created, time_updated)
                     VALUES (?1, 'prj_1', ?2, ?3, ?4, ?5, '2.0.25', 'build', '{\"id\":\"m\",\"providerID\":\"p\",\"variant\":\"default\"}', 0, 0, 0, 0, 0, ?6, ?6 + 500)",
                    rusqlite::params![id, parent, format!("slug-{id}"), cwd, title, at],
                ).unwrap();
                for (seq, kind, data) in [
                    (1, "user", serde_json::json!({"text": "kiwi-needle question", "time": {"created": 1}})),
                    (2, "assistant", serde_json::json!({"agent": "build", "model": {"id": "m", "providerID": "p"}, "content": [
                        {"type": "text", "id": "t1", "text": "an answer"},
                        {"type": "tool", "id": "c1", "name": "bash", "state": {"status": "completed", "input": {"command": "ls"}, "content": [{"type": "text", "text": "x"}]}, "time": {"created": 1}}],
                        "time": {"created": 2, "completed": 3}})),
                ] {
                    conn.execute(
                        "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
                        rusqlite::params![format!("msg_{seq}_{id}"), id, kind, seq, at + seq, data.to_string()],
                    ).unwrap();
                }
            }
        } else {
            conn.execute_batch(
                "CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT, vcs TEXT, time_created INTEGER, time_updated INTEGER);
                 CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT, slug TEXT, directory TEXT,
                     title TEXT, version TEXT, agent TEXT, model TEXT, cost REAL, tokens_input INTEGER,
                     tokens_output INTEGER, tokens_cache_read INTEGER, tokens_cache_write INTEGER,
                     time_created INTEGER, time_updated INTEGER, time_archived INTEGER);
                 CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
                 CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);",
            ).unwrap();
            conn.execute("INSERT INTO project VALUES ('p1', ?1, 'git', 1, 1)", [&cwd]).unwrap();
            for (i, (id, parent, title)) in rows.iter().enumerate() {
                let at = t - i as i64 * 10_000;
                conn.execute(
                    "INSERT INTO session VALUES (?1, 'p1', ?2, ?3, ?4, ?5, '1.17.18', 'build', NULL, NULL, NULL, NULL, NULL, NULL, ?6, ?6, NULL)",
                    rusqlite::params![id, parent, format!("slug-{id}"), cwd, title, at],
                ).unwrap();
                let say = if parent.is_some() { "kiwi-needle question" } else { "plain talk" };
                for (n, role, part) in [
                    (1, "user", serde_json::json!({"type": "text", "text": say})),
                    (2, "assistant", serde_json::json!({"type": "text", "text": "an answer"})),
                ] {
                    conn.execute("INSERT INTO message VALUES (?1, ?2, ?3, ?3, ?4)", rusqlite::params![format!("m{n}_{id}"), id, at + n, serde_json::json!({"role": role}).to_string()]).unwrap();
                    conn.execute("INSERT INTO part VALUES (?1, ?2, ?3, ?4, ?4, ?5)", rusqlite::params![format!("p{n}_{id}"), format!("m{n}_{id}"), id, at + n, part.to_string()]).unwrap();
                }
                let tool = serde_json::json!({"type": "tool", "callID": "c1", "tool": "bash", "state": {"status": "completed", "input": {"command": "ls"}, "output": "x"}});
                conn.execute("INSERT INTO part VALUES (?1, ?2, ?3, 9, 9, ?4)", rusqlite::params![format!("p9_{id}"), format!("m2_{id}"), id, tool.to_string()]).unwrap();
            }
        }
    }
}

/// Every command that exists, so a new one cannot ship without a decision
/// here: either a case in `cases()` or a reason it has no JSON.
fn leaves(command: &clap::Command, prefix: &str, out: &mut BTreeSet<String>) {
    for sub in command.get_subcommands().filter(|c| c.get_name() != "help") {
        let path = if prefix.is_empty() { sub.get_name().to_string() } else { format!("{prefix} {}", sub.get_name()) };
        if sub.has_subcommands() {
            leaves(sub, &path, out);
        } else {
            out.insert(path);
        }
    }
}

/// Long-running or interactive commands, and why they have no JSON document.
const EXEMPT: &[(&str, &str)] = &[
    ("tui", "interactive terminal UI"),
    ("serve", "runs until stopped; the API it serves is JSON (see the web tests)"),
    ("hub serve", "runs until stopped"),
    ("hub receive", "the ssh end of a transfer: its stdout is the protocol"),
    ("hub send", "the ssh end of a transfer: its stdout is a tar"),
    ("resume", "hands the terminal to the agent; only its failures are asm's"),
    ("events", "an unending stream; covered by `events_stream_follows_the_stores`"),
];

/// `(command path, arguments)`. `{HOME}` is the test home. Each runs on a
/// fresh home, so a command that changes things cannot disturb the next.
fn cases() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("list", vec!["list"]),
        ("list", vec!["list", "--include-children", "--status", "idle"]),
        ("children", vec!["children", OC_ROOT]),
        ("messages", vec!["messages", CLAUDE_A]),
        ("messages", vec!["messages", CLAUDE_A, "--format", "json", "--tail", "2"]),
        ("capabilities", vec!["capabilities"]),
        ("projects", vec!["projects", "--worktrees"]),
        ("show", vec!["show", CLAUDE_A]),
        ("rename", vec!["rename", CLAUDE_A, "A new title"]),
        ("move", vec!["move", CLAUDE_B, "{HOME}/code/other"]),
        ("archive", vec!["archive", CLAUDE_B]),
        ("unarchive", vec!["unarchive", CLAUDE_B]),
        ("delete", vec!["delete", "--yes", CLAUDE_B]),
        ("delete", vec!["delete", CLAUDE_B]),
        ("send", vec!["send", "no-such-session", "hello"]),
        ("import", vec!["import", CLAUDE_A, "--to", "opencode", "--dry-run"]),
        ("import", vec!["import", CLAUDE_A, "--to", "opencode", "--show-toolmap"]),
        ("export", vec!["export", CLAUDE_A]),
        ("export", vec!["export", CLAUDE_A, "-o", "{HOME}/ir.json"]),
        ("worktrees", vec!["worktrees", "{HOME}/code/app"]),
        ("search", vec!["search", "kiwi-needle"]),
        ("index", vec!["index", "--stats"]),
        ("doctor", vec!["doctor"]),
        ("sync init", vec!["sync", "init"]),
        ("sync status", vec!["sync", "status"]),
        ("sync commit", vec!["sync", "commit"]),
        ("update", vec!["update", "--check", "--version", "9.9.9"]),
        ("update", vec!["update", "--version", "0.0.1"]),
        ("hub token", vec!["hub", "token"]),
        ("hub admin-token", vec!["hub", "admin-token"]),
        ("hub commands-token", vec!["hub", "commands-token"]),
        ("hub machines", vec!["hub", "machines"]),
        ("hub revoke", vec!["hub", "revoke", "nobody"]),
        ("join", vec!["join", "http://127.0.0.1:1", "--token", "t", "--name", "m"]),
        ("push", vec!["push", "--all"]),
        ("pull", vec!["pull", "--all"]),
        ("remote list", vec!["remote", "list"]),
        ("remote machines", vec!["remote", "machines"]),
        ("peer add", vec!["peer", "add", "p", "http://127.0.0.1:1", "--token", "t", "--as", "m"]),
        ("peer remove", vec!["peer", "remove", "p"]),
        ("peer list", vec!["peer", "list"]),
        ("peer sessions", vec!["peer", "sessions", "p"]),
        ("daemon start", vec!["daemon", "start"]),
        ("daemon stop", vec!["daemon", "stop"]),
        ("daemon install", vec!["daemon", "install", "--no-start"]),
        ("daemon uninstall", vec!["daemon", "uninstall"]),
        ("daemon status", vec!["daemon", "status"]),
        ("control enable", vec!["control", "enable"]),
        ("control disable", vec!["control", "disable"]),
        ("control status", vec!["control", "status"]),
        ("control push", vec!["control", "push", "m", "codex:x"]),
        ("control pull", vec!["control", "pull", "m", "codex:x"]),
        ("control send", vec!["control", "send", "codex:x", "--to", "m"]),
        ("control move", vec!["control", "move", "codex:x", "--from", "a", "--to", "b"]),
        ("control jobs", vec!["control", "jobs"]),
        ("control show", vec!["control", "show", "x"]),
        ("control cancel", vec!["control", "cancel", "x"]),
        ("control retry", vec!["control", "retry", "x"]),
    ]
}

/// `--json` is accepted by every command, and what comes back is JSON:
/// a document (or lines of them) on stdout, or one `{"error": ...}` object on
/// stderr with nothing on stdout. If a command's help promises JSON and it
/// prints a table or text instead, this fails.
#[test]
fn every_command_answers_json_or_a_structured_error() {
    let mut tree = BTreeSet::new();
    leaves(&asm_cli::command(), "", &mut tree);
    // `daemon` itself runs a daemon in the foreground when given no verb.
    tree.insert("daemon".into());
    let covered: BTreeSet<String> = cases().iter().map(|c| c.0.to_string()).chain(EXEMPT.iter().map(|e| e.0.to_string())).chain(["daemon".to_string()]).collect();
    assert_eq!(tree.difference(&covered).collect::<Vec<_>>(), Vec::<&String>::new(), "commands with no JSON case or exemption");
    assert_eq!(covered.difference(&tree).collect::<Vec<_>>(), Vec::<&String>::new(), "cases for commands that no longer exist");

    let codes = ["not_found", "ambiguous", "live_session", "unsupported", "io", "usage", "conflict", "unreachable", "unauthorized", "internal"];
    for (name, args) in cases() {
        let home = Home::new(false);
        let root = home.path("home").display().to_string();
        let args: Vec<String> = args.iter().map(|a| a.replace("{HOME}", &root)).collect();
        let mut with_json = vec!["--json"];
        with_json.extend(args.iter().map(String::as_str));
        let out = home.run(&with_json);
        let what = format!("`asm {}` (exit {})\nstdout: {}\nstderr: {}", with_json.join(" "), out.code, out.stdout, out.stderr);
        if out.code == 0 {
            assert!(out.stderr.trim().is_empty() || !out.stderr.trim().starts_with('{'), "a success has no error object: {what}");
            let whole = serde_json::from_str::<Value>(&out.stdout).is_ok();
            let lines = !out.stdout.trim().is_empty() && out.stdout.lines().all(|l| serde_json::from_str::<Value>(l).is_ok());
            assert!(whole || lines, "{name}: stdout is not JSON: {what}");
        } else if out.stdout.trim().is_empty() {
            let error = out.error();
            assert!(codes.contains(&error["code"].as_str().unwrap_or("")), "{name}: unknown code: {what}");
            assert!(error["message"].as_str().is_some_and(|m| !m.is_empty()), "{name}: {what}");
        } else {
            // A report that also says some item failed: a document, not text.
            out.json();
        }
    }
}

#[test]
fn json_failures_are_one_object_with_the_exit_status_of_their_code() {
    let home = Home::new(false);
    let status = |args: &[&str]| {
        let out = home.run(args);
        (out.code, out.error())
    };

    // A prefix both Claude sessions share: ambiguous, with its candidates.
    let (code, error) = status(&["--json", "show", "7f3a1c"]);
    assert_eq!((code, error["code"].as_str()), (11, Some("ambiguous")));
    let mut candidates: Vec<&str> = error["candidates"].as_array().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
    candidates.sort();
    assert_eq!(candidates, [format!("claude-code:{CLAUDE_A}"), format!("claude-code:{CLAUDE_B}")]);

    let (code, error) = status(&["--json", "show", "no-such-thing"]);
    assert_eq!((code, error["code"].as_str()), (10, Some("not_found")));
    assert!(error.get("candidates").is_none());

    // The same through a command that resolves for itself.
    assert_eq!(status(&["--json", "unarchive", "7f3a1c"]).0, 11);
    assert_eq!(status(&["--json", "messages", "no-such-thing"]).0, 10);

    // Usage: clap's own complaint, a bad value, a bad combination.
    let (code, error) = status(&["--json", "list", "--status", "bogus"]);
    assert_eq!((code, error["code"].as_str()), (2, Some("usage")));
    assert_eq!(status(&["--json", "--agent", "nope", "list"]).0, 2);
    assert_eq!(status(&["--json", "messages", CLAUDE_A, "--range", "x"]).0, 2);
    assert_eq!(status(&["--json", "list", "--follow", "--ids-only"]).0, 2);
    assert_eq!(status(&["--json", "events", "--interval", "0.1"]).0, 2);

    // Unreachable: nothing listens on port 1.
    let (code, error) = status(&["--json", "join", "http://127.0.0.1:1", "--token", "t", "--name", "m"]);
    assert_eq!((code, error["code"].as_str()), (15, Some("unreachable")));

    // io: the file cannot be written.
    let (code, error) = status(&["--json", "export", CLAUDE_A, "-o", "/no/such/dir/ir.json"]);
    assert_eq!((code, error["code"].as_str()), (16, Some("io")));

    // Without --json the text is what it always was and the status is the same.
    let out = home.run(&["show", "7f3a1c"]);
    assert_eq!(out.code, 11);
    assert!(out.stdout.is_empty() && out.stderr.starts_with("Error: '7f3a1c' is ambiguous; matches: "), "{}", out.stderr);
    let out = home.run(&["list", "--status", "bogus"]);
    assert_eq!(out.code, 2, "clap's usage error, as before");
    assert!(out.stderr.starts_with("error: invalid value"), "{}", out.stderr);

    // ASM_JSON=1 is --json.
    let out = home.command(&["show", "no-such-thing"]).env("ASM_JSON", "1").output().unwrap();
    assert_eq!(out.status.code(), Some(10));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("{\"error\":"));
}

#[test]
fn a_session_in_use_is_live_session_and_untouched() {
    let home = Home::new(false);
    let sessions = home.path("claude/sessions");
    fs::create_dir_all(&sessions).unwrap();
    // A record naming a process that is alive: this test.
    let record = serde_json::json!({"pid": std::process::id(), "sessionId": CLAUDE_A, "cwd": "/w", "status": "busy"});
    fs::write(sessions.join(format!("{}.json", std::process::id())), record.to_string()).unwrap();

    let out = home.run(&["--json", "delete", "--yes", CLAUDE_A]);
    assert_eq!(out.code, 12, "{}", out.stderr);
    assert_eq!(out.error()["code"], "live_session");
    let out = home.run(&["--json", "rename", CLAUDE_A, "nope"]);
    assert_eq!(out.code, 12);

    let live = home.run(&["--json", "list", "--status", "live"]);
    assert_eq!(ids(&live.json()), [CLAUDE_A]);
    assert!(home.path("claude/projects").read_dir().unwrap().next().is_some());
}

#[test]
fn unsupported_and_conflict_have_their_own_status() {
    // OpenCode 2.x has no archive.
    let v2 = Home::new(true);
    let out = v2.run(&["--json", "archive", OC_OTHER]);
    assert_eq!((out.code, out.error()["code"].as_str().map(String::from)), (13, Some("unsupported".into())), "{}", out.stderr);

    // An archived session whose place has been taken cannot come back over it.
    let home = Home::new(false);
    let path = home.path("claude/projects").join(home.path("home/code/app").display().to_string().replace('/', "-")).join(format!("{CLAUDE_B}.jsonl"));
    let original = fs::read_to_string(&path).unwrap();
    assert_eq!(home.run(&["--json", "archive", CLAUDE_B]).code, 0);
    fs::write(&path, "taken").unwrap();
    let out = home.run(&["--json", "unarchive", CLAUDE_B]);
    assert_eq!((out.code, out.error()["code"].as_str().map(String::from)), (14, Some("conflict".into())), "{}", out.stderr);
    assert_eq!(fs::read_to_string(&path).unwrap(), "taken", "refused, not overwritten");
    fs::remove_file(&path).unwrap();
    assert_eq!(home.run(&["--json", "unarchive", CLAUDE_B]).code, 0);
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn mutators_print_what_they_did() {
    let home = Home::new(false);
    let out = home.run(&["--json", "rename", CLAUDE_A, "A new title"]);
    let doc = out.json();
    assert_eq!((doc["ok"].clone(), doc["agent"].as_str(), doc["id"].as_str(), doc["title"].as_str()), (true.into(), Some("claude-code"), Some(CLAUDE_A), Some("A new title")));
    assert_eq!(home.run(&["--json", "show", CLAUDE_A]).json()["title"], "A new title");

    let other = home.path("home/code/other");
    fs::create_dir_all(&other).unwrap();
    let other = other.display().to_string();
    let doc = home.run(&["--json", "move", CLAUDE_B, &other]).json();
    assert_eq!((doc["ok"].clone(), doc["dir"].as_str()), (true.into(), Some(other.as_str())));
    assert!(doc["new_transcript"].is_string() && doc["warnings"].is_array());

    let doc = home.run(&["--json", "archive", CLAUDE_B]).json();
    assert!(doc["ok"] == true && doc["archived_to"].is_string(), "{doc}");
    let doc = home.run(&["--json", "unarchive", CLAUDE_B]).json();
    assert!(doc["ok"] == true && doc["restored_to"].is_string(), "{doc}");

    // OpenCode 1.x archives with a flag: there is no path.
    let doc = home.run(&["--json", "archive", OC_OTHER]).json();
    assert!(doc["ok"] == true && doc["archived_to"].is_null(), "{doc}");
    let doc = home.run(&["--json", "unarchive", OC_OTHER]).json();
    assert!(doc["ok"] == true && doc["restored_to"].is_null(), "{doc}");

    // A prompt cannot be answered by a program: it is refused, not waited on.
    let out = home.run(&["--json", "delete", CLAUDE_B]);
    assert_eq!(out.code, 2);
    assert_eq!(out.error()["message"], "confirmation required: pass --yes");
    assert_eq!(home.run(&["--json", "show", CLAUDE_B]).code, 0, "nothing was deleted");

    let doc = home.run(&["--json", "delete", "--yes", CLAUDE_B]).json();
    assert!(doc["ok"] == true && doc["backup_dir"].is_string() && doc["removed"].is_array() && doc["descendants"] == 0, "{doc}");
    assert_eq!(home.run(&["--json", "show", CLAUDE_B]).code, 10);

    // OpenCode's delete takes the tree with it, and says how many.
    let doc = home.run(&["--json", "delete", "--yes", OC_ROOT]).json();
    assert_eq!(doc["descendants"], 2, "{doc}");

    let doc = home.run(&["--json", "import", CLAUDE_B, "--to", "claude-code", "--show-toolmap"]).json();
    assert_eq!(doc["target"], "claude-code");
    assert!(doc["tools"].as_array().unwrap().iter().all(|t| t["from"].is_string() && t["to"].is_string()));

    let ir = home.path("home/ir.json");
    let doc = home.run(&["--json", "export", CLAUDE_A, "-o", ir.to_str().unwrap()]).json();
    assert_eq!((doc["ok"].clone(), doc["messages"].clone()), (true.into(), 4.into()));
    assert!(serde_json::from_str::<Value>(&fs::read_to_string(&ir).unwrap()).unwrap()["messages"].is_array());

    let doc = home.run(&["--json", "control", "disable"]).json();
    assert_eq!(doc, serde_json::json!({"enabled": false}));
    let doc = home.run(&["--json", "daemon", "stop"]).json();
    assert_eq!(doc, serde_json::json!({"stopped": false, "pid": null}));
    let doc = home.run(&["--json", "daemon", "uninstall"]).json();
    assert_eq!(doc["uninstalled"], false);
}

#[test]
fn worktrees_json_carries_the_sessions_of_each() {
    let home = Home::new(false);
    let repo = home.path("home/code/app");
    let git = |args: &[&str]| {
        let status = Command::new("git").args(args).current_dir(&repo).env_clear().env("PATH", "/usr/bin:/bin").env("HOME", home.path("home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null").stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"]);
    let doc = home.run(&["--json", "worktrees", repo.to_str().unwrap()]).json();
    let worktrees = doc.as_array().unwrap();
    assert_eq!(worktrees.len(), 1);
    let w = &worktrees[0];
    for key in ["path", "branch", "is_main", "detached", "locked", "prunable"] {
        assert!(w.get(key).is_some(), "{key} is still there: {w}");
    }
    // Three root sessions live in the directory itself: two Claude, two OpenCode roots.
    assert_eq!(w["session_count"], 4);
    assert_eq!(w["sessions"].as_array().unwrap().len(), 4);
    assert!(w["sessions"][0]["ref"]["agent"].is_string());
}

/// `projects` and `worktrees` list places, and `--here`/`-W` narrow them to
/// where asm runs: this worktree (not one nested in it), or all of them.
#[test]
fn projects_and_worktrees_honour_here() {
    let home = Home::new(false);
    let repo = home.path("home/code/app");
    let git = |args: &[&str]| {
        let status = Command::new("git").args(args).current_dir(&repo).env_clear().env("PATH", "/usr/bin:/bin").env("HOME", home.path("home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null").stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"]);
    git(&["worktree", "add", "-q", ".claude/worktrees/x"]);
    let elsewhere = home.path("home/elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let run = |cwd: &PathBuf, args: &[&str]| -> Value {
        let out = home.command(args).current_dir(cwd).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let paths = |doc: &Value, key: &str| -> Vec<String> {
        doc.as_array().unwrap().iter().map(|w| w[key].as_str().unwrap().rsplit('/').next().unwrap().to_string()).collect()
    };

    let here = run(&repo, &["--json", "projects", "--here"]);
    assert_eq!(paths(&here, "root"), vec!["app"]);
    assert_eq!(paths(&here[0]["worktrees"], "path"), vec!["app"], "not the worktree nested in it");
    assert_eq!(here[0]["session_count"], 4);
    let all = run(&repo, &["--json", "projects", "-W"]);
    assert_eq!(paths(&all[0]["worktrees"], "path"), vec!["app", "x"]);
    assert!(run(&elsewhere, &["--json", "projects", "--here"]).as_array().unwrap().is_empty(), "no project here");

    assert_eq!(paths(&run(&repo, &["--json", "worktrees", "--here"]), "path"), vec!["app"]);
    assert_eq!(paths(&run(&repo, &["--json", "worktrees", "-W"]), "path"), vec!["app", "x"]);
    assert_eq!(paths(&run(&repo, &["--json", "worktrees"]), "path"), vec!["app", "x"], "without either, every one");
}

#[test]
fn update_is_one_document() {
    let home = Home::new(false);
    let check = home.run(&["--json", "update", "--check", "--version", "9.9.9"]).json();
    assert_eq!((check["latest"].as_str(), check["update_available"].clone()), (Some("9.9.9"), true.into()));
    assert!(check["current"].is_string() && check["target"].is_string());
    // Nothing newer: still one document, now saying nothing was installed.
    let none = home.run(&["--json", "update", "--version", "0.0.1"]).json();
    assert_eq!(none["updated"], false);
    assert_eq!(none["current"], check["current"], "the same fields as --check");
}

#[test]
fn listing_flags_filter_and_feed_xargs() {
    let home = Home::new(false);
    let all = ids(&home.run(&["--json", "list"]).json());
    assert_eq!(all.len(), 4, "two Claude sessions and two OpenCode roots: {all:?}");

    // --json is unchanged: a bare array of sessions with the old fields, and the new flat ones.
    let first = &home.run(&["--json", "list"]).json()[0];
    assert!(first["status"]["state"].is_string(), "status is still the object");
    assert_eq!(first["state"], first["status"]["state"]);
    assert_eq!(first["id"], first["ref"]["native_id"]);

    assert_eq!(ids(&home.run(&["--json", "list", "--status", "idle"]).json()), all);
    assert_eq!(home.run(&["--json", "list", "--status", "archived"]).json(), serde_json::json!([]));
    assert_eq!(home.run(&["archive", OC_OTHER]).code, 0);
    assert_eq!(ids(&home.run(&["--json", "list", "--status", "archived"]).json()), [OC_OTHER]);
    assert!(!ids(&home.run(&["--json", "list", "--status", "idle"]).json()).contains(&OC_OTHER.to_string()));

    let out = home.run(&["list", "--ids-only"]);
    assert_eq!(out.stdout.lines().collect::<Vec<_>>(), all.iter().map(String::as_str).collect::<Vec<_>>());
    // -0: NUL after each id, no newline.
    let out = home.run(&["list", "-0"]);
    assert!(!out.stdout.contains('\n') && out.stdout.ends_with('\0'));
    assert_eq!(out.stdout.split_terminator('\0').count(), all.len());
    // Children are in neither unless asked for.
    assert!(!home.run(&["list", "--ids-only"]).stdout.contains(OC_KID));
    assert!(home.run(&["--include-children", "list", "--ids-only"]).stdout.contains(OC_KID));

    // search: one id per session, and --status narrows the index.
    let hits = home.run(&["search", "kiwi-needle", "--ids-only"]);
    assert_eq!(hits.stdout.lines().collect::<Vec<_>>().len(), 2, "the kid and the grandkid: {}", hits.stdout);
    assert!(home.run(&["search", "kiwi-needle", "--status", "archived", "--ids-only"]).stdout.is_empty());
    let hit = &home.run(&["--json", "search", "kiwi-needle"]).json()[0];
    assert_eq!(hit["status"], "idle");
    assert_eq!(hit["state"], "idle");
    assert_eq!(home.run(&["--json", "search", "kiwi-needle", "--status", "live"]).json(), serde_json::json!([]));
    // The archived one is found by asking for archived, and flagged so.
    let archived = home.run(&["--json", "search", "plain", "--status", "archived"]).json();
    assert_eq!(archived.as_array().unwrap().len(), 1);
    assert_eq!((archived[0]["status"].clone(), archived[0]["state"].clone()), ("archived".into(), "archived".into()));
}

#[test]
fn children_are_a_flat_array_with_depth_and_parent() {
    for v2 in [false, true] {
        let home = Home::new(v2);
        let out = home.run(&["--json", "children", OC_ROOT]);
        let kids = out.json();
        let rows: Vec<(String, u64, String)> = kids
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["id"].as_str().unwrap().into(), s["depth"].as_u64().unwrap(), s["parent"].as_str().unwrap().into()))
            .collect();
        assert_eq!(rows, [(OC_KID.into(), 1, OC_ROOT.into()), (OC_GRAND.into(), 2, OC_KID.into())], "v2={v2}");
        assert!(kids[0]["ref"]["agent"] == "opencode" && kids[0]["state"] == "idle");
        assert_eq!(home.run(&["children", OC_ROOT, "--ids-only"]).stdout, format!("{OC_KID}\n{OC_GRAND}\n"));
        assert_eq!(home.run(&["children", OC_ROOT, "-0"]).stdout, format!("{OC_KID}\0{OC_GRAND}\0"));
        assert_eq!(home.run(&["--json", "children", OC_GRAND]).json(), serde_json::json!([]), "a leaf has none");
        assert_eq!(home.run(&["--json", "children", OC_ROOT, "--status", "archived"]).json(), serde_json::json!([]));
        let table = home.run(&["children", OC_ROOT]);
        assert!(table.stdout.contains("Review docs") && table.stdout.contains("PARENT"), "{}", table.stdout);
    }
}

#[test]
fn messages_stream_the_transcript_as_json_lines() {
    let home = Home::new(false);
    let out = home.run(&["messages", CLAUDE_A]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let rows = out.lines();
    assert_eq!(rows.iter().map(|r| r["index"].as_u64().unwrap()).collect::<Vec<_>>(), [0, 1, 2, 3]);
    assert_eq!(rows[0]["role"], "user");
    assert_eq!(rows[0]["parts"][0], serde_json::json!({"type": "text", "text": "list the files please"}));
    assert!(rows[0]["ts"].is_string());
    assert_eq!(rows[1]["parts"][1]["type"], "tool_call");

    let roles = |args: &[&str]| -> Vec<u64> { home.run(args).lines().iter().map(|r| r["index"].as_u64().unwrap()).collect() };
    assert_eq!(roles(&["messages", CLAUDE_A, "--role", "assistant"]), [1, 3]);
    assert_eq!(roles(&["messages", CLAUDE_A, "--role", "tool"]), [1, 2]);
    assert_eq!(roles(&["messages", CLAUDE_A, "--tail", "1"]), [3]);
    assert_eq!(roles(&["messages", CLAUDE_A, "--range", "1..3"]), [1, 2]);
    assert_eq!(roles(&["messages", CLAUDE_A, "--range", "2.."]), [2, 3]);
    assert_eq!(roles(&["messages", CLAUDE_A, "--text"]), [0, 1, 3]);
    let text = home.run(&["messages", CLAUDE_A, "--text", "--role", "assistant", "--tail", "1"]).lines();
    assert_eq!(text[0]["parts"], serde_json::json!([{"type": "text", "text": "Two files."}]));

    let array = home.run(&["messages", CLAUDE_A, "--format", "json", "--range", "..2"]).json();
    assert_eq!(array.as_array().unwrap().len(), 2);
    assert_eq!(home.run(&["--json", "messages", CLAUDE_A, "--role", "system"]).stdout, "", "no lines, no error");
    assert_eq!(home.run(&["messages", CLAUDE_A, "--format", "json", "--role", "system"]).json(), serde_json::json!([]));
}

/// OpenCode keeps its transcript in different tables in 1.x and 2.x; the
/// rows `asm messages` gives are the same.
#[test]
fn opencode_messages_and_capabilities_in_both_generations() {
    for v2 in [false, true] {
        let home = Home::new(v2);
        let rows = home.run(&["messages", OC_KID]).lines();
        assert_eq!(rows.len(), 2, "v2={v2}");
        assert_eq!(rows[0]["role"], "user");
        assert_eq!(rows[0]["parts"][0]["text"], "kiwi-needle question", "v2={v2}");
        assert!(rows[1]["parts"].as_array().unwrap().iter().any(|p| p["type"] == "tool_call" && p["name"] == "bash"), "v2={v2}");
        let tool = home.run(&["messages", OC_KID, "--role", "tool"]).lines();
        assert_eq!(tool.len(), 1, "v2={v2}");
        assert_eq!(tool[0]["index"], 1);
        let text = home.run(&["messages", OC_KID, "--role", "tool", "--text"]).lines();
        assert_eq!(text[0]["parts"], serde_json::json!([{"type": "text", "text": "an answer"}]), "the tool call is dropped, the words stay");

        let caps = home.run(&["--json", "capabilities"]).json();
        assert_eq!(caps["json_contract_version"], 1);
        assert_eq!(caps["asm_version"], env!("CARGO_PKG_VERSION"));
        assert!(caps["ir_version"].is_u64() && caps["index_schema_version"].is_u64());
        let agents = caps["agents"].as_array().unwrap();
        assert_eq!(agents.iter().map(|a| a["agent"].as_str().unwrap()).collect::<Vec<_>>(), ["claude-code", "opencode", "jcode", "codex", "antigravity"]);
        let oc = agents.iter().find(|a| a["agent"] == "opencode").unwrap();
        assert_eq!(oc["schema"], if v2 { "v2" } else { "v1" });
        assert_eq!(oc["unmigrated"], if v2 { Value::Bool(false) } else { Value::Null });
        assert_eq!(oc["capabilities"]["archive"], !v2, "2.x has no archive");
        assert_eq!(oc["installed"], true);
        let claude = &agents[0];
        assert_eq!(claude["schema"], Value::Null);
        assert_eq!(agents[3]["installed"], false);
        assert_eq!(agents[3]["store_root"], Value::Null);

        let doctor = home.run(&["--json", "doctor"]).json();
        let oc = doctor["agents"].as_array().unwrap().iter().find(|a| a["agent"] == "opencode").unwrap();
        assert_eq!(oc["schema"], if v2 { "v2" } else { "v1" });
        assert!(doctor["warnings"].is_array());

        let human = home.run(&["capabilities"]);
        assert!(human.stdout.contains("json contract 1") && human.stdout.contains(if v2 { "schema v2" } else { "schema v1" }), "{}", human.stdout);
    }
}

#[test]
fn events_stream_follows_the_stores() {
    let home = Home::new(false);
    let mut child = home.command(&["events", "--interval", "0.5"]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line.unwrap()).is_err() {
                return;
            }
        }
    });
    let next = |what: &str| -> Value {
        let line = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap_or_else(|_| panic!("no event for: {what}"));
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"))
    };
    assert_eq!(next("ready"), serde_json::json!({"event": "ready"}));

    let dir = home.path("claude/projects").join(home.path("home/code/app").display().to_string().replace('/', "-"));
    let id = "cccccccc-1111-2222-3333-444444444444";
    fs::write(dir.join(format!("{id}.jsonl")), r#"{"type":"user","uuid":"u1","parentUuid":null,"isSidechain":false,"timestamp":"2026-10-10T10:00:00.000Z","cwd":"/w","sessionId":"cccccccc-1111-2222-3333-444444444444","message":{"role":"user","content":"hello"}}"#).unwrap();
    let added = next("added");
    assert_eq!((added["event"].as_str(), added["session"]["id"].as_str()), (Some("added"), Some(id)));
    assert_eq!(added["session"]["ref"]["agent"], "claude-code");

    let conn = rusqlite::Connection::open(home.path("xdg/opencode/opencode.db")).unwrap();
    conn.execute("UPDATE session SET title = 'Renamed live', time_updated = time_updated + 7 WHERE id = ?1", [OC_OTHER]).unwrap();
    let changed = next("changed");
    assert_eq!((changed["event"].as_str(), changed["session"]["title"].as_str()), (Some("changed"), Some("Renamed live")));

    fs::remove_file(dir.join(format!("{id}.jsonl"))).unwrap();
    let removed = next("removed");
    assert_eq!((removed["event"].as_str(), removed["session"]["id"].as_str()), (Some("removed"), Some(id)));

    // Killed, it leaves nothing behind; a closed pipe ends it the same way.
    child.kill().unwrap();
    child.wait().unwrap();

    // `list --follow` is the same stream, narrowed by what list narrows by.
    let mut child = home.command(&["list", "--follow", "--status", "live", "--interval", "0.5"]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut first = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut first).unwrap();
    assert_eq!(first.trim(), r#"{"event":"ready"}"#);
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn a_reader_that_goes_away_stops_the_stream_even_with_nothing_to_say() {
    let home = Home::new(false);
    let mut child = home.command(&["events", "--interval", "0.5"]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), r#"{"event":"ready"}"#);
    drop(reader);
    // No session changes, so there is never a line to write: it has to notice
    // the closed pipe by itself.
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        if std::time::Instant::now() > end {
            child.kill().unwrap();
            panic!("still running after its reader left");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
