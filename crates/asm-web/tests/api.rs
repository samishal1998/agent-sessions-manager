//! The web API as a program calls it: single sessions, messages, filters,
//! typed errors with their statuses, the bearer token and the event stream.
//! The stores are process-wide, so they are set up once in a temp dir; the
//! tests only ever read them, except the stream test, which adds one Claude
//! session and so asserts on what it finds rather than on totals.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tokio_stream::StreamExt;
use tower::ServiceExt;

const A: &str = "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43";
const B: &str = "7f3a1c99-2d4e-4b91-9a05-6c7e8f201b44";
const LIVE: &str = "aaaa0000-2d4e-4b91-9a05-6c7e8f201b45";
const TOKEN: &str = "s3cret-token-for-tests";

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
            // Safety: set once per test process, before any test reads the stores.
            unsafe { std::env::set_var(var, root.join(sub)) };
        }
        let project = root.join("claude/projects/-w-app");
        fs::create_dir_all(&project).unwrap();
        let rec = |id: &str, uuid: &str, parent: Value, kind: &str, message: Value| {
            json!({"type": kind, "uuid": uuid, "parentUuid": parent, "isSidechain": false, "timestamp": "2026-10-09T10:00:00.000Z",
                   "cwd": "/w/app", "sessionId": id, "message": message}).to_string()
        };
        let a = [
            rec(A, "u1", Value::Null, "user", json!({"role": "user", "content": "walrus question"})),
            rec(A, "a1", json!("u1"), "assistant", json!({"id": "m1", "type": "message", "role": "assistant", "content": [
                {"type": "text", "text": "Running."}, {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls"}}]})),
            rec(A, "u2", json!("a1"), "user", json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "x"}]})),
            rec(A, "a2", json!("u2"), "assistant", json!({"id": "m2", "type": "message", "role": "assistant", "content": [{"type": "text", "text": "Done."}]})),
        ];
        fs::write(project.join(format!("{A}.jsonl")), a.join("\n")).unwrap();
        fs::write(project.join(format!("{B}.jsonl")), rec(B, "u1", Value::Null, "user", json!({"role": "user", "content": "second"}))).unwrap();
        fs::write(project.join(format!("{LIVE}.jsonl")), rec(LIVE, "u1", Value::Null, "user", json!({"role": "user", "content": "in use"}))).unwrap();
        let sessions = root.join("claude/sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(sessions.join("1.json"), json!({"pid": std::process::id(), "sessionId": LIVE, "cwd": "/w/app", "status": "busy"}).to_string()).unwrap();

        // OpenCode 1.x, with a subagent under the root.
        fs::create_dir_all(root.join("xdg-data/opencode")).unwrap();
        let conn = rusqlite::Connection::open(root.join("xdg-data/opencode/opencode.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT, vcs TEXT, time_created INTEGER, time_updated INTEGER);
             CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT, slug TEXT, directory TEXT,
                 title TEXT, version TEXT, agent TEXT, model TEXT, cost REAL, tokens_input INTEGER,
                 tokens_output INTEGER, tokens_cache_read INTEGER, tokens_cache_write INTEGER,
                 time_created INTEGER, time_updated INTEGER, time_archived INTEGER);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
             INSERT INTO project VALUES ('p1', '/w/oc', 'git', 1, 1);
             INSERT INTO session VALUES ('ses_root', 'p1', NULL, 's1', '/w/oc', 'Octopus plan', '1.17.18', 'build', NULL, NULL, NULL, NULL, NULL, NULL, 1800000000000, 1800000000000, NULL);
             INSERT INTO session VALUES ('ses_kid', 'p1', 'ses_root', 's2', '/w/oc', 'Octopus kid (@general subagent)', '1.17.18', 'general', NULL, NULL, NULL, NULL, NULL, NULL, 1799999990000, 1799999990000, NULL);
             INSERT INTO message VALUES ('m1', 'ses_root', 1, 1, '{\"role\":\"user\"}');
             INSERT INTO part VALUES ('p1', 'm1', 'ses_root', 1, 1, '{\"type\":\"text\",\"text\":\"hello octopus\"}');",
        )
        .unwrap();
        dir
    })
    .path()
}

fn app(token: Option<&str>) -> axum::Router {
    isolate();
    asm_web::api::router(token.map(str::to_string))
}

async fn call(app: &axum::Router, uri: &str, headers: &[(&str, &str)]) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut request = Request::builder().uri(uri);
    for (k, v) in headers {
        request = request.header(*k, *v);
    }
    let response = app.clone().oneshot(request.body(Body::empty()).unwrap()).await.unwrap();
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    (parts.status, parts.headers, String::from_utf8_lossy(&bytes).into_owned())
}

async fn get(uri: &str) -> (StatusCode, Value) {
    let (status, _, body) = call(&app(None), uri, &[]).await;
    (status, serde_json::from_str(&body).unwrap_or_else(|e| panic!("{uri}: {e}: {body:?}")))
}

fn ids(value: &Value) -> Vec<&str> {
    value.as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect()
}

#[tokio::test]
async fn meta_says_which_asm_and_what_it_can_do() {
    let (status, meta) = get("/api/meta").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(meta["asm_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(meta["json_contract_version"], 1);
    assert!(meta["home"].is_string(), "what it always had: {meta}");
    assert!(meta.get("hostname").is_some());
    let agents = meta["capabilities"].as_array().unwrap();
    assert_eq!(agents.len(), 5);
    let oc = agents.iter().find(|a| a["agent"] == "opencode").unwrap();
    assert_eq!((oc["installed"].clone(), oc["schema"].clone()), (true.into(), "v1".into()));
    assert_eq!(oc["capabilities"]["archive"], true);
}

#[tokio::test]
async fn the_session_list_takes_filters() {
    let (_, all) = get("/api/sessions").await;
    let all = ids(&all).into_iter().map(String::from).collect::<Vec<_>>();
    assert!(all.contains(&A.to_string()) && all.contains(&"ses_root".to_string()));
    assert!(!all.contains(&"ses_kid".to_string()), "children are hidden unless all=true");

    let (_, found) = get("/api/sessions?all=true").await;
    assert!(ids(&found).contains(&"ses_kid"));
    let (_, found) = get("/api/sessions?agent=opencode").await;
    assert_eq!(ids(&found), ["ses_root"]);
    let (_, found) = get("/api/sessions?agent=oc&all=true").await;
    assert_eq!(ids(&found), ["ses_root", "ses_kid"], "newest first, and a short name for the agent will do");
    let (_, found) = get("/api/sessions?status=live").await;
    assert_eq!(ids(&found), [LIVE]);
    assert_eq!(found[0]["state"], "live");
    let (_, found) = get("/api/sessions?status=archived").await;
    assert_eq!(found, json!([]));
    let (_, found) = get("/api/sessions?q=OCTOPUS").await;
    assert_eq!(ids(&found), ["ses_root"], "case-insensitive, over the title");
    let (_, found) = get("/api/sessions?q=7f3a1c88").await;
    assert_eq!(ids(&found), [A], "over the id");
    let (_, found) = get("/api/sessions?project=/w/oc").await;
    assert_eq!(ids(&found), ["ses_root"]);
    let (_, found) = get("/api/sessions?agent=claude-code&status=idle&q=7f3a").await;
    let mut found = ids(&found);
    found.sort();
    assert_eq!(found, [A, B]);

    // A bad value is the caller's mistake: 400, in the shape this route always answered.
    let (status, error) = get("/api/sessions?agent=nope").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["code"], "usage");
    assert!(error["error"].as_str().unwrap().contains("unknown agent"), "{error}");
    assert_eq!(get("/api/sessions?status=bogus").await.0, StatusCode::BAD_REQUEST);

    // The streamed form is filtered the same way.
    let (status, headers, body) = call(&app(None), "/api/sessions?stream=1&agent=opencode", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/x-ndjson");
    let lines: Vec<Value> = body.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.last().unwrap()["done"], true);
    let streamed: Vec<&str> = lines.iter().filter_map(|l| l["sessions"].as_array()).flatten().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(streamed, ["ses_root"]);
}

#[tokio::test]
async fn one_session_and_its_typed_errors() {
    let (status, session) = get(&format!("/api/sessions/claude-code/{A}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((session["id"].as_str(), session["ref"]["agent"].as_str(), session["state"].as_str()), (Some(A), Some("claude-code"), Some("idle")));
    assert!(session["status"]["state"].is_string());
    // A prefix is fine when it is unique, and `opencode` has a root with that id.
    assert_eq!(get("/api/sessions/claude-code/7f3a1c88").await.1["id"], A);
    assert_eq!(get("/api/sessions/opencode/ses_kid").await.1["parent"], "ses_root", "a child is addressable");

    let (status, error) = get("/api/sessions/claude-code/zzzz").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], "not_found");
    assert!(error["error"]["message"].is_string() && error.as_object().unwrap().len() == 1);

    let (status, error) = get("/api/sessions/claude-code/7f3a1c").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "ambiguous");
    let mut candidates: Vec<&str> = error["error"]["candidates"].as_array().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
    candidates.sort();
    assert_eq!(candidates, [format!("claude-code:{A}"), format!("claude-code:{B}")]);
}

#[tokio::test]
async fn messages_are_json_lines_or_an_array() {
    let (status, headers, body) = call(&app(None), &format!("/api/sessions/claude-code/{A}/messages"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/x-ndjson");
    let rows: Vec<Value> = body.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(rows.iter().map(|r| r["index"].as_u64().unwrap()).collect::<Vec<_>>(), [0, 1, 2, 3]);
    assert_eq!(rows[0]["parts"][0]["text"], "walrus question");

    let q = |query: &str| format!("/api/sessions/claude-code/{A}/messages?{query}");
    let (_, array) = get(&q("format=json&role=assistant")).await;
    assert_eq!(array.as_array().unwrap().iter().map(|r| r["index"].as_u64().unwrap()).collect::<Vec<_>>(), [1, 3]);
    let (_, array) = get(&q("format=json&tail=1&text=1")).await;
    assert_eq!(array[0]["parts"], json!([{"type": "text", "text": "Done."}]));
    let (_, array) = get(&q("format=json&range=1..3&role=tool")).await;
    assert_eq!(array.as_array().unwrap().len(), 2);

    let (status, error) = get(&q("role=wizard")).await;
    assert_eq!((status, error["error"]["code"].as_str()), (StatusCode::BAD_REQUEST, Some("usage")));
    assert_eq!(get(&q("range=bad")).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(get(&q("format=xml")).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(get("/api/sessions/claude-code/zzzz/messages").await.0, StatusCode::NOT_FOUND);
    assert_eq!(get("/api/sessions/claude-code/7f3a1c/messages").await.1["error"]["code"], "ambiguous");

    // OpenCode 1.x reads the same way.
    let (_, rows) = get("/api/sessions/opencode/ses_root/messages?format=json").await;
    assert_eq!(rows[0]["role"], "user");
    assert_eq!(rows[0]["parts"][0]["text"], "hello octopus");
}

#[tokio::test]
async fn the_routes_that_predate_typed_errors_keep_their_shape_and_gain_a_code() {
    let (status, _, body) = call(&app(None), "/api/session/claude-code/zzzz/ir", &[]).await;
    let error: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(error["error"].is_string(), "still the string the UI shows: {error}");
    assert_eq!(error["code"], "not_found");

    let (status, _, body) = call(&app(None), "/api/session/claude-code/7f3a1c/ir", &[]).await;
    let error: Value = serde_json::from_str(&body).unwrap();
    assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("ambiguous")));
    assert_eq!(error["candidates"].as_array().unwrap().len(), 2);

    // A session in use is refused with 423, and nothing is touched.
    let request = Request::builder().method("POST").uri(format!("/api/session/claude-code/{LIVE}/delete")).header("x-asm-request", "1").body(Body::empty()).unwrap();
    let response = app(None).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::LOCKED);
    let error: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(error["error"].is_string());
    assert_eq!(error["code"], "live_session");
    assert!(isolate().join(format!("claude/projects/-w-app/{LIVE}.jsonl")).exists());

    // The guard against cross-site forms is unchanged.
    let request = Request::builder().method("POST").uri(format!("/api/session/claude-code/{A}/archive")).body(Body::empty()).unwrap();
    assert_eq!(app(None).oneshot(request).await.unwrap().status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn with_a_token_every_api_request_needs_it() {
    let app = app(Some(TOKEN));
    let bearer = format!("Bearer {TOKEN}");
    for uri in ["/api/meta", "/api/sessions", &format!("/api/sessions/claude-code/{A}"), "/api/doctor"] {
        let (status, headers, body) = call(&app, uri, &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(headers["www-authenticate"], "Bearer");
        let error: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(error["error"]["code"], "unauthorized");
        assert!(!body.contains(TOKEN), "the token is never echoed");
        for wrong in ["Bearer nope", "Bearer ", &format!("Bearer {TOKEN}x"), &format!("Bearer {}", &TOKEN[..5]), TOKEN, &format!("Basic {TOKEN}")] {
            assert_eq!(call(&app, uri, &[("authorization", wrong)]).await.0, StatusCode::UNAUTHORIZED, "{uri} with {wrong:?}");
        }
        assert_eq!(call(&app, uri, &[("authorization", &bearer)]).await.0, StatusCode::OK, "{uri}");
    }
    // A mutation needs both the token and the marker header.
    let request = Request::builder().method("POST").uri(format!("/api/session/claude-code/{A}/archive")).header("x-asm-request", "1").body(Body::empty()).unwrap();
    assert_eq!(app.clone().oneshot(request).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    // Without one set, nothing asks.
    assert_eq!(call(&self::app(None), "/api/meta", &[]).await.0, StatusCode::OK);
}

#[tokio::test]
async fn the_event_stream_says_ready_then_what_changed() {
    let app = app(None);
    assert_eq!(call(&app, "/api/events?interval=0.1", &[]).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(call(&app, "/api/events?agent=nope", &[]).await.0, StatusCode::BAD_REQUEST);

    let response = app.oneshot(Request::builder().uri("/api/events?interval=0.5&agent=claude-code").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut stream = response.into_body().into_data_stream();
    let mut seen = String::new();
    let mut next = async |want: &str| {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !seen.contains(want) {
            let chunk = tokio::time::timeout(end.saturating_duration_since(std::time::Instant::now()), stream.next())
                .await
                .unwrap_or_else(|_| panic!("no {want:?} in {seen:?}"))
                .expect("the stream ended")
                .unwrap();
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
    };
    next("event: ready\ndata: {\"event\":\"ready\"}\n\n").await;

    let id = "bbbb1111-2d4e-4b91-9a05-6c7e8f201b46";
    let line = json!({"type": "user", "uuid": "u1", "parentUuid": null, "isSidechain": false, "timestamp": "2026-10-10T10:00:00.000Z", "cwd": "/w/app", "sessionId": id, "message": {"role": "user", "content": "new"}});
    fs::write(isolate().join(format!("claude/projects/-w-app/{id}.jsonl")), line.to_string()).unwrap();
    next("event: added\n").await;
    let data = seen.lines().find(|l| l.starts_with("data: {\"event\":\"added\"")).expect("an added event");
    let event: Value = serde_json::from_str(data.trim_start_matches("data: ")).unwrap();
    assert_eq!((event["event"].as_str(), event["session"]["id"].as_str()), (Some("added"), Some(id)));
    assert!(!seen.contains("ses_root"), "the filter holds: only Claude sessions");
}
