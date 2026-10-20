//! Child sessions end to end through `ops`, over fixture stores of every
//! agent that has them: OpenCode 1.x (three levels deep), jcode (a child,
//! and a debug session that is not one), Codex (a spawned thread, and one
//! whose parent is gone). The stores are process-wide, so they are set up
//! once, in a temp dir, before anything reads them. OpenCode 2.x has its
//! own tests beside its fixture (`adapter/opencode/v2.rs`), because a
//! process has one OpenCode store.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use asm_core::adapter::SessionFilter;
use asm_core::model::{AgentKind, Session};
use asm_core::ops::{self, ResolveError};

const OC_ROOT: &str = "ses_tree00000000root";
const OC_KID: &str = "ses_tree00000000kid1";
const OC_GRAND: &str = "ses_tree00000000kid2";
const OC_OTHER: &str = "ses_other0000000000";

const JC_MAIN: &str = "session_main_1790000000000_aaaaaaaaaaaaaaaa";
const JC_KID: &str = "session_kid_1790000000001_bbbbbbbbbbbbbbbb";
const JC_DEBUG: &str = "session_dbg_1790000000002_cccccccccccccccc";

const CX_MAIN: &str = "01a020ab-9a2c-71a1-8384-1e1058de5553";
const CX_KID: &str = "01a020ab-1f35-7763-8138-231f59ac2e99";
const CX_ORPHAN: &str = "01a020ac-0000-7000-8000-000000000001";
const CX_GHOST: &str = "01a020aa-0000-7000-8000-00000000dead";

const CLAUDE: &str = "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43";

fn isolate() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (var, sub) in [
            ("HOME", "home"),
            ("ASM_DATA_DIR", "asm"),
            ("XDG_DATA_HOME", "xdg-data"),
            ("XDG_CONFIG_HOME", "xdg-config"),
            ("XDG_STATE_HOME", "xdg-state"),
            ("XDG_CACHE_HOME", "xdg-cache"),
            ("CLAUDE_CONFIG_DIR", "claude"),
            ("CODEX_HOME", "codex"),
            ("JCODE_HOME", "jcode"),
            ("ASM_ANTIGRAVITY_ROOT", "antigravity"),
        ] {
            fs::create_dir_all(root.join(sub)).unwrap();
            // Safety: set once per test process, before concurrent readers.
            unsafe { std::env::set_var(var, root.join(sub)) };
        }
        claude(root);
        opencode(root);
        jcode(root);
        codex(root);
        dir
    })
    .path()
}

fn claude(root: &Path) {
    let project = root.join("claude/projects/-w-app");
    fs::create_dir_all(&project).unwrap();
    let record = r#"{"type":"user","uuid":"u1","parentUuid":null,"isSidechain":false,"timestamp":"2026-10-09T10:00:00.000Z","cwd":"/w/app","sessionId":"7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43","message":{"role":"user","content":"hello"}}"#;
    fs::write(project.join(format!("{CLAUDE}.jsonl")), record).unwrap();
}

fn opencode(root: &Path) {
    let dir = root.join("xdg-data/opencode");
    fs::create_dir_all(&dir).unwrap();
    let conn = rusqlite::Connection::open(dir.join("opencode.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT, vcs TEXT, time_created INTEGER, time_updated INTEGER);
         CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT, slug TEXT, directory TEXT,
             title TEXT, version TEXT, agent TEXT, model TEXT, cost REAL, tokens_input INTEGER,
             tokens_output INTEGER, tokens_cache_read INTEGER, tokens_cache_write INTEGER,
             time_created INTEGER, time_updated INTEGER, time_archived INTEGER);
         CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
         CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
         INSERT INTO project VALUES ('p1', '/w/app', 'git', 1, 1);",
    )
    .unwrap();
    // A root, its subagent, that subagent's own subagent, and another root.
    for (id, parent, title, updated) in [
        (OC_ROOT, None, "Tree root", 1_800_000_030_000_i64),
        (OC_KID, Some(OC_ROOT), "Tree kid (@general subagent)", 1_800_000_020_000),
        (OC_GRAND, Some(OC_KID), "Tree grandkid (@explore subagent)", 1_800_000_010_000),
        (OC_OTHER, None, "Another root", 1_800_000_000_000),
    ] {
        conn.execute(
            "INSERT INTO session VALUES (?1, 'p1', ?2, ?3, '/w/app', ?4, '1.17.18', 'build', NULL, NULL, NULL, NULL, NULL, NULL, ?5, ?5, NULL)",
            rusqlite::params![id, parent, format!("slug-{id}"), title, updated],
        )
        .unwrap();
    }
}

fn jcode(root: &Path) {
    let sessions = root.join("jcode/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let snapshot = |id: &str, name: &str, extra: &str| {
        format!(
            r#"{{"id":"{id}","title":"{name}",{extra}"created_at":"2026-08-01T10:00:00Z","updated_at":"2026-08-01T11:00:00Z","messages":[],"model":"m","working_dir":"/w/app","short_name":"{name}"}}"#
        )
    };
    fs::write(sessions.join(format!("{JC_MAIN}.json")), snapshot(JC_MAIN, "main", "")).unwrap();
    fs::write(
        sessions.join(format!("{JC_KID}.json")),
        snapshot(JC_KID, "kid", &format!(r#""parent_id":"{JC_MAIN}","#)),
    )
    .unwrap();
    fs::write(sessions.join(format!("{JC_DEBUG}.json")), snapshot(JC_DEBUG, "dbg", r#""is_debug":true,"#)).unwrap();
}

fn codex(root: &Path) {
    let day = root.join("codex/sessions/2026/08/20");
    fs::create_dir_all(&day).unwrap();
    let conn = rusqlite::Connection::open(root.join("codex/state_5.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE threads (
            id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL,
            created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            source TEXT NOT NULL, model_provider TEXT NOT NULL, cwd TEXT NOT NULL,
            title TEXT NOT NULL, sandbox_policy TEXT NOT NULL, approval_mode TEXT NOT NULL,
            tokens_used INTEGER NOT NULL DEFAULT 0, archived INTEGER NOT NULL DEFAULT 0, archived_at INTEGER,
            git_branch TEXT, cli_version TEXT NOT NULL DEFAULT '', model TEXT);
         CREATE TABLE thread_spawn_edges (
            parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL PRIMARY KEY, status TEXT NOT NULL);",
    )
    .unwrap();
    for (id, title, updated) in [(CX_MAIN, "Codex main", 1_787_253_341_i64), (CX_KID, "Codex kid", 1_787_253_300), (CX_ORPHAN, "Codex orphan", 1_787_253_200)] {
        let path = day.join(format!("rollout-2026-08-20T19-35-12-{id}.jsonl"));
        fs::write(
            &path,
            format!(r#"{{"timestamp":"2026-08-20T19:35:12.500Z","type":"session_meta","payload":{{"id":"{id}","cwd":"/w/app"}}}}"#),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO threads (id, rollout_path, created_at, updated_at, source, model_provider, cwd, title, sandbox_policy, approval_mode)
             VALUES (?1, ?2, ?3, ?3, 'exec', 'openai', '/w/app', ?4, '{}', 'never')",
            rusqlite::params![id, path.display().to_string(), updated, title],
        )
        .unwrap();
    }
    // One thread spawned from another; one spawned from a thread that is
    // not anywhere.
    for (parent, child) in [(CX_MAIN, CX_KID), (CX_GHOST, CX_ORPHAN)] {
        conn.execute(
            "INSERT INTO thread_spawn_edges (parent_thread_id, child_thread_id, status) VALUES (?1, ?2, 'done')",
            [parent, child],
        )
        .unwrap();
    }
}

fn everything() -> Vec<Session> {
    isolate();
    ops::list_sessions(&SessionFilter { include_children: true, ..SessionFilter::default() }).unwrap()
}

fn id_of(s: &Session) -> &str {
    &s.handle.native_id
}

/// The tree as nested ids, per the order the listing gave.
fn shape(all: &[Session], nodes: &[ops::Node]) -> String {
    nodes
        .iter()
        .map(|n| {
            let id = id_of(&all[n.index]);
            if n.children.is_empty() { id.to_string() } else { format!("{id}({})", shape(all, &n.children)) }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn show(id: &str) -> String {
    // Short enough to read, long enough to tell the fixtures apart.
    id.chars().take(16).collect()
}

#[test]
fn every_agent_that_has_children_groups_them_and_orphans_stand_alone() {
    let all = everything();
    let tree = ops::group_children(&all);
    let by_agent = |agent: AgentKind| -> Vec<&ops::Node> { tree.iter().filter(|n| all[n.index].handle.agent == agent).collect() };

    // OpenCode: three levels, arbitrary depth.
    let oc = by_agent(AgentKind::OpenCode);
    assert_eq!(oc.len(), 2, "two roots");
    let root = oc.iter().find(|n| id_of(&all[n.index]) == OC_ROOT).unwrap();
    assert_eq!(root.descendants(), 2);
    assert_eq!(id_of(&all[root.children[0].index]), OC_KID);
    assert_eq!(id_of(&all[root.children[0].children[0].index]), OC_GRAND);

    // jcode: the child under its parent; the debug session is nowhere.
    let jc = by_agent(AgentKind::JCode);
    assert_eq!(jc.len(), 1);
    assert_eq!(jc[0].descendants(), 1);
    assert!(all.iter().all(|s| id_of(s) != JC_DEBUG), "debug sessions stay out of a children listing");

    // Codex: a spawned thread under its parent, and a thread whose parent
    // is gone as a root of its own rather than lost.
    let cx = by_agent(AgentKind::Codex);
    let roots: Vec<&str> = cx.iter().map(|n| id_of(&all[n.index])).collect();
    assert_eq!(roots.len(), 2, "{roots:?}");
    assert!(roots.contains(&CX_MAIN) && roots.contains(&CX_ORPHAN));
    let main = cx.iter().find(|n| id_of(&all[n.index]) == CX_MAIN).unwrap();
    assert_eq!(id_of(&all[main.children[0].index]), CX_KID);
    let orphan = all.iter().find(|s| id_of(s) == CX_ORPHAN).unwrap();
    assert_eq!(orphan.parent.as_deref(), Some(CX_GHOST), "the edge is still there to see");

    // Claude Code has no child sessions: its subagents are inside its transcript.
    assert_eq!(by_agent(AgentKind::ClaudeCode).len(), 1);
    let rendered = shape(&all, &tree);
    assert!(rendered.contains(&format!("{OC_ROOT}({OC_KID}({OC_GRAND}))")), "{rendered}");
}

#[test]
fn the_default_listing_and_the_counts_are_what_they_were() {
    isolate();
    let default = ops::list_sessions(&SessionFilter::default()).unwrap();
    let ids: std::collections::HashSet<&str> = default.iter().map(id_of).collect();
    for hidden in [OC_KID, OC_GRAND, JC_KID, JC_DEBUG, CX_KID, CX_ORPHAN] {
        assert!(!ids.contains(hidden), "{} is not in the default listing", show(hidden));
    }
    for shown in [OC_ROOT, OC_OTHER, JC_MAIN, CX_MAIN, CLAUDE] {
        assert!(ids.contains(shown), "{} is", show(shown));
    }

    // Projects count the same from either listing: the TUI and the web UI
    // list children to group them, and their project counts must not move.
    let count = |sessions: &[Session]| -> usize {
        ops::group_projects(sessions, |_| None, |_| Vec::new()).iter().map(|p| p.session_count).sum()
    };
    assert_eq!(count(&default), 5);
    assert_eq!(count(&everything()), 5);
}

/// A child is addressed like any session: by its id, a prefix, or
/// `agent:prefix`, from a plain filter, so every verb in every frontend can
/// reach it.
#[test]
fn a_child_resolves_by_id_and_prefix_without_asking_for_children() {
    isolate();
    let plain = SessionFilter::default();
    let resolve = |q: &str| ops::resolve_ref(q, &plain).map(|s| (s.handle.native_id, s.parent));

    let (id, parent) = resolve(OC_GRAND).unwrap();
    assert_eq!((id.as_str(), parent.as_deref()), (OC_GRAND, Some(OC_KID)));
    assert_eq!(resolve("opencode:ses_tree00000000kid1").unwrap().0, OC_KID);
    assert_eq!(resolve("ses_tree00000000kid2").unwrap().0, OC_GRAND);
    assert_eq!(resolve(&format!("jcode:{JC_KID}")).unwrap().1.as_deref(), Some(JC_MAIN));
    assert_eq!(resolve("codex:01a020ab-1f35").unwrap().0, CX_KID);
    assert_eq!(resolve(CX_ORPHAN).unwrap().1.as_deref(), Some(CX_GHOST));

    // The parent's short id still names the parent, though children share it.
    assert_eq!(resolve("ses_tree00000000root").unwrap().0, OC_ROOT);
    assert_eq!(resolve("ses_tree0").unwrap().0, OC_ROOT, "roots win a prefix the children share");
    // Two children and no root to prefer.
    assert!(matches!(resolve("ses_tree00000000kid"), Err(ResolveError::Ambiguous { .. })));
    assert!(matches!(resolve("ses_nothing"), Err(ResolveError::NotFound { .. })));
}

/// jcode's debug sessions are not children: asking for children does not
/// list them, but their id still addresses them, as it did before.
#[test]
fn a_debug_session_is_addressable_but_never_listed() {
    isolate();
    for filter in [SessionFilter::default(), SessionFilter { include_children: true, ..SessionFilter::default() }] {
        assert_eq!(id_of(&ops::resolve_ref(JC_DEBUG, &filter).unwrap()), JC_DEBUG);
        let listed = ops::list_sessions(&filter).unwrap();
        assert!(listed.iter().all(|s| id_of(s) != JC_DEBUG), "listings never show it");
    }
    let all = ops::list_sessions(&SessionFilter { include_children: true, include_debug: true, ..SessionFilter::default() }).unwrap();
    assert!(all.iter().any(|s| id_of(s) == JC_DEBUG));
}

#[test]
fn what_hangs_under_a_session_is_what_deleting_it_would_take() {
    isolate();
    let root = ops::resolve_ref(OC_ROOT, &SessionFilter::default()).unwrap();
    let below: Vec<(usize, String)> =
        ops::descendants(&root).unwrap().iter().map(|(d, s)| (*d, id_of(s).to_string())).collect();
    assert_eq!(below, [(1, OC_KID.to_string()), (2, OC_GRAND.to_string())]);
    assert!(ops::deletes_descendants(root.handle.agent));
    let kid = ops::resolve_ref(OC_KID, &SessionFilter::default()).unwrap();
    assert_eq!(ops::descendants(&kid).unwrap().len(), 1);
    let other = ops::resolve_ref(OC_OTHER, &SessionFilter::default()).unwrap();
    assert!(ops::descendants(&other).unwrap().is_empty());
}
