//! `?async=1` and `/api/tasks`: a job answers at once, is followed to its
//! end, and a second identical request is the same job. A file of its own
//! (so a process of its own) because it changes the stores: `tests/api.rs`
//! only reads them. The harness is the same one.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use asm_web::tasks::{self, Lane};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

const A: &str = "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43";
const B: &str = "7f3a1c99-2d4e-4b91-9a05-6c7e8f201b44";
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
        for id in [A, B] {
            let line = json!({"type": "user", "uuid": "u1", "parentUuid": null, "isSidechain": false, "timestamp": "2026-10-09T10:00:00.000Z",
                              "cwd": "/w/app", "sessionId": id, "message": {"role": "user", "content": "hello"}});
            fs::write(project.join(format!("{id}.jsonl")), line.to_string()).unwrap();
        }
        dir
    })
    .path()
}

fn app() -> axum::Router {
    isolate();
    asm_web::api::router(None)
}

async fn send(method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let request = Request::builder().method(method).uri(uri).header("x-asm-request", "1").header("content-type", "application/json");
    let body = body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty);
    let response = app().oneshot(request.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{uri}: {e}: {bytes:?}")))
}

async fn job(id: &Value) -> Value {
    let (status, job) = send("GET", &format!("/api/tasks/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{job}");
    job
}

/// The job once it has finished.
async fn ended(id: &Value) -> Value {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let job = job(id).await;
        if job["state"] == "ok" || job["state"] == "failed" {
            assert!(job["ended"].is_string(), "{job}");
            return job;
        }
        assert!(std::time::Instant::now() < end, "still {job}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn archive(id: &str) -> Value {
    json!({"sessions": [{"agent": "claude-code", "native_id": id}], "action": "archive"})
}

#[tokio::test]
async fn an_async_bulk_answers_at_once_and_ends_with_the_body_it_would_have_answered() {
    let held = tasks::hold(Lane::Local);
    let (status, started) = send("POST", "/api/bulk?async=1", Some(archive(A))).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(started.get("existing").is_none(), "{started}");
    let id = &started["job"];
    let waiting = job(id).await;
    assert_eq!(
        (waiting["state"].as_str(), waiting["kind"].as_str(), waiting["total"].as_u64(), waiting["done"].as_u64()),
        (Some("queued"), Some("bulk"), Some(1), Some(0)),
        "waits for its lane: {waiting}"
    );
    assert_eq!(waiting["key"], format!("bulk:archive:claude-code:{A}"));
    assert!(waiting["started"].is_string() && waiting["ended"].is_null() && waiting["result"].is_null());

    // The same request again is the same job, and starts nothing.
    let (status, again) = send("POST", "/api/bulk?async=1", Some(archive(A))).await;
    assert_eq!((status, again), (StatusCode::ACCEPTED, json!({"job": id, "existing": true})));
    let (_, listed) = send("GET", "/api/tasks", None).await;
    assert_eq!(listed["tasks"].as_array().unwrap().iter().filter(|t| t["id"] == *id).count(), 1);

    drop(held);
    let done = ended(id).await;
    assert_eq!((done["state"].as_str(), done["done"].as_u64()), (Some("ok"), Some(1)), "{done}");
    assert_eq!(done["result"]["summary"], "Archive: 1 of 1 succeeded");

    // Without ?async, the very body the task holds.
    assert_eq!(send("POST", &format!("/api/session/claude-code/{A}/unarchive"), None).await.0, StatusCode::OK);
    let (status, sync) = send("POST", "/api/bulk", Some(archive(A))).await;
    assert_eq!((status, &sync), (StatusCode::OK, &done["result"]));
}

#[tokio::test]
async fn the_same_sessions_sent_somewhere_else_are_another_job() {
    let held = tasks::hold(Lane::Local);
    let export = |dir: &str| {
        let dir = isolate().join(dir);
        json!({"sessions": [{"agent": "claude-code", "native_id": B}], "action": "export", "dir": dir})
    };
    let (_, first) = send("POST", "/api/bulk?async=1", Some(export("out-a"))).await;
    let (_, second) = send("POST", "/api/bulk?async=1", Some(export("out-b"))).await;
    assert_ne!(first["job"], second["job"]);
    assert!(second.get("existing").is_none(), "{second}");
    drop(held);
    for id in [&first["job"], &second["job"]] {
        assert_eq!(ended(id).await["state"], "ok");
    }
}

#[tokio::test]
async fn hub_jobs_wait_for_each_other_and_an_error_is_the_tasks_error() {
    let held = tasks::hold(Lane::Hub);
    let (status, pull) = send("POST", "/api/hub/pull-all?async=1", None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let push = json!({"sessions": [{"agent": "claude-code", "native_id": B}], "action": "push"});
    let (_, pushing) = send("POST", "/api/bulk?async=1", Some(push.clone())).await;
    assert_ne!(pull["job"], pushing["job"]);
    let (_, again) = send("POST", "/api/hub/pull-all?async=1", None).await;
    assert_eq!(again, json!({"job": pull["job"], "existing": true}));
    for id in [&pull["job"], &pushing["job"]] {
        assert_eq!(job(id).await["state"], "queued", "both on the hub lane");
    }
    assert_eq!(job(&pushing["job"]).await["kind"], "push");

    drop(held);
    // This machine joined no hub: the route says so as an HTTP error, the task as its error.
    let failed = ended(&pull["job"]).await;
    assert_eq!(failed["state"], "failed", "{failed}");
    assert!(failed["result"].is_null());
    let (status, sync) = send("POST", "/api/hub/pull-all", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(failed["error"], sync["error"], "the same text");
    assert!(!failed["error"].as_str().unwrap().is_empty());

    // A bulk push is a report, as it always was: the session failed, not the request.
    let pushed = ended(&pushing["job"]).await;
    assert_eq!(pushed["state"], "ok", "{pushed}");
    assert_eq!(pushed["result"]["failed"], 1);
    assert!(pushed["result"]["problems"][0].as_str().unwrap().contains("failed"), "{pushed}");
}

#[tokio::test]
async fn an_unknown_task_is_404_and_the_routes_need_the_token() {
    let (status, error) = send("GET", "/api/tasks/987654321", None).await;
    assert_eq!((status, error), (StatusCode::NOT_FOUND, json!({"error": "no such task"})));
    isolate();
    let guarded = asm_web::api::router(Some(TOKEN.into()));
    for uri in ["/api/tasks", "/api/tasks/1"] {
        let response = guarded.clone().oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
    let request = Request::builder().uri("/api/tasks").header("authorization", format!("Bearer {TOKEN}")).body(Body::empty()).unwrap();
    assert_eq!(guarded.oneshot(request).await.unwrap().status(), StatusCode::OK);
}
