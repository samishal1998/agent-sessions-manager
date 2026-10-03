//! JSON API. Every handler wraps the blocking core ops in spawn_blocking;
//! sessions are addressed as `agent:native_id` exactly like the CLI.

use axum::Json;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use asm_core::adapter::SessionFilter;
use asm_core::model::{AgentKind, Session};
use asm_core::ops;

type ApiError = (StatusCode, Json<Value>);
type ApiResult<T> = Result<Json<T>, ApiError>;

fn err(status: StatusCode, message: impl ToString) -> ApiError {
    (status, Json(json!({ "error": message.to_string() })))
}

fn internal(message: impl ToString) -> ApiError {
    err(StatusCode::INTERNAL_SERVER_ERROR, message)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f).await.map_err(internal)?
}

fn resolve(agent: &str, id: &str) -> Result<Session, ApiError> {
    let query = format!("{agent}:{id}");
    ops::resolve_ref(&query, &SessionFilter::default())
        .map_err(|e| err(StatusCode::NOT_FOUND, e))
}

/// Guard every mutating request.
///
/// The server binds to localhost, but any page in the user's browser can
/// reach localhost too. A cross-origin HTML form POST is a CORS "simple
/// request": it is sent without a preflight and the handler would run. Two
/// cheap defenses close that: require a custom header (which a form cannot
/// set, and which forces a preflight that this server does not answer), and
/// reject requests the browser itself labels cross-site.
async fn guard_mutations(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::Method;

    let mutating = matches!(*request.method(), Method::POST | Method::PUT | Method::DELETE);
    if mutating {
        let headers = request.headers();
        let has_marker = headers.contains_key("x-asm-request");
        let cross_site = headers
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|site| site != "same-origin" && site != "none");
        if !has_marker || cross_site {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "cross-origin or unmarked mutation refused; \
                              send X-Asm-Request: 1 from the asm UI"
                })),
            )
                .into_response();
        }
    }
    next.run(request).await
}

pub fn router() -> axum::Router {
    axum::Router::new()
        .route("/api/meta", get(meta))
        .route("/api/sessions", get(sessions))
        .route("/api/projects", get(projects))
        .route("/api/doctor", get(doctor))
        .route("/api/search", get(search))
        .route("/api/index", get(index_stats))
        .route("/api/index/refresh", post(index_refresh))
        .route("/api/session/{agent}/{id}/ir", get(session_ir))
        .route("/api/session/{agent}/{id}/rename", post(rename))
        .route("/api/session/{agent}/{id}/archive", post(archive))
        .route("/api/session/{agent}/{id}/unarchive", post(unarchive))
        .route("/api/session/{agent}/{id}/delete", post(delete))
        .route("/api/session/{agent}/{id}/move", post(move_session))
        .route("/api/session/{agent}/{id}/import", post(import))
        .route("/api/session/{agent}/{id}/send", post(send))
        .route("/api/bulk", post(bulk))
        .route("/api/hub", get(hub))
        .route("/api/hub/pull", post(hub_pull))
        .route("/api/hub/pull-all", post(hub_pull_all))
        .layer(axum::middleware::from_fn(guard_mutations))
}

/// The UI shortens `/home/you/code/x` to `~/code/x`, which it cannot do
/// without knowing where home is — guessing `/home/<user>` is wrong on
/// macOS, where it is `/Users/<name>`.
async fn meta() -> Json<serde_json::Value> {
    Json(json!({
        "home": asm_core::paths::home().map(|h| h.display().to_string()),
        "hostname": asm_core::process::hostname(),
    }))
}

/// How this machine stands with the hub (`asm_core::hub::actions::hub_status`):
/// whether there is one, whether it answered, and every session with what to
/// do about it. Unreachable is a status here, not an error, so the UI can say
/// so and offer a retry rather than lose the page.
async fn hub() -> ApiResult<Value> {
    blocking(|| {
        let local = ops::list_sessions(&SessionFilter::default()).map_err(internal)?;
        let mut status = serde_json::to_value(asm_core::hub::actions::hub_status(&local)).map_err(internal)?;
        // The daemon is a separate process; this is only what it last wrote.
        // An unreadable status is "no daemon information", not a failed page.
        status["daemon"] = asm_core::hub::daemon::status().ok().and_then(|d| serde_json::to_value(d).ok()).unwrap_or(Value::Null);
        Ok(status)
    })
    .await
    .map(Json)
}

/// Pull everything on the hub (`asm pull --all` with no filters), reporting
/// like a bulk action so the UI can reuse its list of what could not land.
async fn hub_pull_all() -> ApiResult<Value> {
    blocking(|| {
        let remote = asm_core::hub::client::load().map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
        let report = asm_core::hub::actions::pull_all(&remote, None, None)
            .map_err(|e| err(StatusCode::BAD_GATEWAY, e))?;
        serde_json::to_value(json!({
            "verb": "Pull",
            "summary": report.summary("Pull"),
            "problems": report.problems(),
            "ok": report.ok(),
            "skipped": report.skipped(),
            "failed": report.failed(),
            "items": report.items,
        }))
        .map_err(internal)
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
struct PullBody {
    agent: String,
    id: String,
    project_dir: Option<std::path::PathBuf>,
}

async fn hub_pull(Json(body): Json<PullBody>) -> ApiResult<Value> {
    blocking(move || {
        let remote = asm_core::hub::client::load().map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
        let pulled = asm_core::hub::actions::pull(
            &remote,
            &format!("{}:{}", body.agent, body.id),
            body.project_dir.as_deref(),
        )
        .map_err(|e| err(StatusCode::CONFLICT, e))?;
        serde_json::to_value(pulled).map_err(internal)
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
struct SessionsQuery {
    #[serde(default)]
    all: bool,
    /// Send them as they are read, one JSON object per line, rather than
    /// one array when the last store has given up its last session.
    /// `1` as well as `true`, so a curl by hand works.
    #[serde(default)]
    stream: Option<String>,
}

impl SessionsQuery {
    fn streaming(&self) -> bool {
        matches!(self.stream.as_deref(), Some("1" | "true"))
    }
}

/// Sessions as each store gives them up: `{"sessions":[…]}` per line, then
/// `{"done":true,"problems":[…]}`. On a machine with thousands of them the
/// first line arrives in a moment and the browser can draw it.
async fn stream_sessions(all: bool) -> axum::response::Response {
    use axum::body::{Body, Bytes};
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    tokio::task::spawn_blocking(move || {
        let filter = SessionFilter { include_children: all, ..SessionFilter::default() };
        let line = |value: Value, tx: &tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>| {
            let mut bytes = serde_json::to_vec(&value).unwrap_or_default();
            bytes.push(b'\n');
            // A send error means the browser went away; stop reading stores.
            tx.blocking_send(Ok(Bytes::from(bytes))).is_ok()
        };
        let mut listening = true;
        let problems = ops::stream_sessions(&filter, |_, batch| {
            listening = line(json!({ "sessions": batch }), &tx);
            listening
        });
        if listening {
            line(json!({ "done": true, "problems": problems }), &tx);
        }
    });
    (
        [(axum::http::header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)),
    )
        .into_response()
}

async fn sessions(Query(q): Query<SessionsQuery>) -> axum::response::Response {
    if q.streaming() {
        return stream_sessions(q.all).await;
    }
    match sessions_at_once(q.all).await {
        Ok(json) => json.into_response(),
        Err(e) => e.into_response(),
    }
}

async fn sessions_at_once(all: bool) -> ApiResult<Vec<Session>> {
    let q = SessionsQuery { all, stream: None };
    blocking(move || {
        let filter = SessionFilter { include_children: q.all, ..SessionFilter::default() };
        ops::list_sessions(&filter).map_err(internal)
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
    agent: Option<String>,
    project: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    40
}

async fn search(Query(params): Query<SearchParams>) -> ApiResult<Value> {
    blocking(move || {
        let index = asm_core::index::Index::open().map_err(internal)?;
        let hits = index
            .search(&asm_core::index::SearchQuery {
                text: params.q,
                agent: params.agent.as_deref().and_then(AgentKind::parse),
                project: params.project.map(std::path::PathBuf::from),
                limit: params.limit.clamp(1, 200),
            })
            .map_err(internal)?;
        serde_json::to_value(&hits).map_err(internal)
    })
    .await
    .map(Json)
}

/// How far the background index has got, if it is running. A process-wide
/// value because there is one index and one server.
// ponytail: a static is enough for one server; give it to the router as
// state if the web ever serves more than one index.
static INDEXING: std::sync::Mutex<Option<asm_core::index::RefreshProgress>> =
    std::sync::Mutex::new(None);
static INDEX_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Why the last refresh stopped, if it did. Without this a failed refresh
/// is only in the server's log, and the UI says the index is up to date.
static INDEX_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Bring the index up to date on a thread of its own, reporting progress
/// as it goes. Returns whether this call started it; a refresh already
/// running is left to finish rather than run twice over one database.
pub fn refresh_index_in_background() -> bool {
    use std::sync::atomic::Ordering;
    if INDEX_RUNNING.swap(true, Ordering::SeqCst) {
        return false;
    }
    // Visible from the moment it starts, not from the first session read:
    // opening the index and listing the stores takes a while on the
    // machines this is for, and until then the UI would say "up to date".
    *INDEXING.lock().unwrap() = Some(asm_core::index::RefreshProgress::default());
    *INDEX_ERROR.lock().unwrap() = None;
    std::thread::spawn(|| {
        match asm_core::index::Index::open() {
            Ok(mut index) => {
                // Held until the refresh returns, not until the last session
                // is read: the final batch is still being written, and a
                // search would find less than the UI says is there.
                let outcome = index.refresh(|progress| {
                    *INDEXING.lock().unwrap() = Some(progress);
                });
                if let Err(e) = outcome {
                    eprintln!("search index refresh failed: {e}");
                    *INDEX_ERROR.lock().unwrap() = Some(e.to_string());
                }
            }
            Err(e) => {
                eprintln!("could not open search index: {e}");
                *INDEX_ERROR.lock().unwrap() = Some(e.to_string());
            }
        }
        *INDEXING.lock().unwrap() = None;
        INDEX_RUNNING.store(false, Ordering::SeqCst);
    });
    true
}

async fn index_stats() -> ApiResult<Value> {
    // The progress is read here rather than in the blocking task so it is
    // current even while a refresh holds the database.
    let indexing = INDEXING.lock().unwrap().clone();
    let running = INDEX_RUNNING.load(std::sync::atomic::Ordering::SeqCst);
    let failed = INDEX_ERROR.lock().unwrap().clone();
    blocking(move || {
        let index = asm_core::index::Index::open().map_err(internal)?;
        let mut stats = serde_json::to_value(index.stats().map_err(internal)?).map_err(internal)?;
        if let Some(object) = stats.as_object_mut() {
            object.insert("indexing".into(), serde_json::to_value(indexing).map_err(internal)?);
            object.insert("running".into(), running.into());
            object.insert("index_error".into(), serde_json::to_value(failed).map_err(internal)?);
        }
        Ok(stats)
    })
    .await
    .map(Json)
}

/// Starts a refresh and says so; the UI follows it through `/api/index`.
/// It does not wait: on a busy machine indexing takes minutes, and a
/// request that hangs that long is a request that times out.
async fn index_refresh() -> ApiResult<Value> {
    let started = refresh_index_in_background();
    Ok(Json(json!({ "started": started, "running": true })))
}

async fn projects() -> ApiResult<Vec<asm_core::model::Project>> {
    blocking(|| ops::list_projects().map_err(internal)).await.map(Json)
}

async fn doctor() -> ApiResult<ops::DoctorReport> {
    blocking(|| ops::doctor().map_err(internal)).await.map(Json)
}

async fn session_ir(Path((agent, id)): Path<(String, String)>) -> ApiResult<Value> {
    blocking(move || {
        let session = resolve(&agent, &id)?;
        let ir = ops::export_ir(&session).map_err(internal)?;
        serde_json::to_value(&ir).map_err(internal)
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
struct RenameBody {
    title: String,
}

async fn rename(
    Path((agent, id)): Path<(String, String)>,
    Json(body): Json<RenameBody>,
) -> ApiResult<Value> {
    blocking(move || {
        let session = resolve(&agent, &id)?;
        ops::rename(&session, &body.title).map_err(|e| err(StatusCode::CONFLICT, e))?;
        Ok(json!({ "ok": true }))
    })
    .await
    .map(Json)
}

async fn archive(Path((agent, id)): Path<(String, String)>) -> ApiResult<Value> {
    blocking(move || {
        let session = resolve(&agent, &id)?;
        let outcome = ops::archive(&session).map_err(|e| err(StatusCode::CONFLICT, e))?;
        serde_json::to_value(&outcome).map_err(internal)
    })
    .await
    .map(Json)
}

async fn unarchive(Path((agent, id)): Path<(String, String)>) -> ApiResult<Value> {
    blocking(move || {
        let query = format!("{agent}:{id}");
        ops::unarchive(&query, &SessionFilter::default())
            .map_err(|e| err(StatusCode::CONFLICT, e))?;
        Ok(json!({ "ok": true }))
    })
    .await
    .map(Json)
}

async fn delete(Path((agent, id)): Path<(String, String)>) -> ApiResult<Value> {
    blocking(move || {
        let session = resolve(&agent, &id)?;
        let report = ops::delete(&session).map_err(|e| err(StatusCode::CONFLICT, e))?;
        serde_json::to_value(&report).map_err(internal)
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
struct MoveBody {
    dir: String,
}

async fn move_session(
    Path((agent, id)): Path<(String, String)>,
    Json(body): Json<MoveBody>,
) -> ApiResult<Value> {
    blocking(move || {
        let session = resolve(&agent, &id)?;
        let outcome = ops::relocate(&session, std::path::Path::new(&body.dir))
            .map_err(|e| err(StatusCode::CONFLICT, e))?;
        serde_json::to_value(&outcome).map_err(internal)
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
struct BulkBody {
    /// Which sessions, as the list rendered them.
    sessions: Vec<BulkTarget>,
    /// The verb, tagged: {"action":"archive"} or {"action":"move","dir":…}.
    #[serde(flatten)]
    action: asm_core::bulk::BulkAction,
}

#[derive(Deserialize)]
struct BulkTarget {
    agent: String,
    native_id: String,
}

/// One verb over many sessions, in one request.
///
/// A round trip per session would be slower, but more importantly it would
/// leave the browser deciding what to do when the fourth of ten fails.
/// The core batch answers that once, for both frontends.
async fn bulk(Json(body): Json<BulkBody>) -> ApiResult<Value> {
    blocking(move || {
        // A session that cannot be resolved is reported as a failed item,
        // not as a failed request: the rest of the batch still runs.
        let mut sessions = Vec::new();
        let mut unresolved = Vec::new();
        for target in &body.sessions {
            match resolve(&target.agent, &target.native_id) {
                Ok(session) => sessions.push(session),
                // Why, too: gone is one reason, one id in two places another.
                Err((_, Json(e))) => unresolved.push(format!(
                    "{}:{}: {}",
                    target.agent,
                    target.native_id,
                    e.get("error").and_then(Value::as_str).unwrap_or("no longer present")
                )),
            }
        }
        let report = asm_core::bulk::run(&sessions, &body.action);
        let verb = body.action.verb();
        serde_json::to_value(json!({
            "verb": verb,
            "summary": report.summary(verb),
            "problems": report.problems(),
            "ok": report.ok(),
            "skipped": report.skipped(),
            "failed": report.failed(),
            "unresolved": unresolved,
            "items": report.items,
        }))
        .map_err(internal)
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
struct ImportBody {
    to: String,
    #[serde(default)]
    seed: bool,
}

async fn import(
    Path((agent, id)): Path<(String, String)>,
    Json(body): Json<ImportBody>,
) -> ApiResult<Value> {
    blocking(move || {
        let session = resolve(&agent, &id)?;
        let target = AgentKind::parse(&body.to)
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("unknown agent '{}'", body.to)))?;
        let opts = asm_core::import::ImportOpts {
            mode: if body.seed {
                asm_core::import::ImportMode::Seed
            } else {
                asm_core::import::ImportMode::Full
            },
            project: None,
            dry_run: false,
        };
        let outcome =
            ops::import(&session, target, &opts).map_err(|e| err(StatusCode::CONFLICT, e))?;
        serde_json::to_value(&outcome).map_err(internal)
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
struct SendBody {
    message: String,
}

/// Send a message into a session and stream the reply as NDJSON.
///
/// A POST returning a stream, rather than an `EventSource` GET, for two
/// reasons that point the same way: `EventSource` cannot set headers, so it
/// could not carry the `X-Asm-Request` marker `guard_mutations` requires,
/// and this *is* a mutation — it spends the user's tokens and lets the
/// agent edit files. Making it a GET would put it outside the guard
/// entirely.
async fn send(
    Path((agent, id)): Path<(String, String)>,
    Json(body): Json<SendBody>,
) -> Result<axum::response::Response, ApiError> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let session = resolve(&agent, &id)?;
    if body.message.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "refusing to send an empty message"));
    }

    // Bounded, so a fast agent cannot outrun a slow client into unbounded
    // memory; the send blocks instead, which is the correct backpressure.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::convert::Infallible>>(64);
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();

    tokio::task::spawn_blocking(move || {
        let mut emit = |event: asm_core::live::LiveEvent| {
            let Ok(line) = serde_json::to_string(&event) else { return };
            // A closed receiver means the browser went away mid-turn.
            // Flipping the flag is what actually kills the agent process;
            // without it the turn would run to completion unwatched.
            if tx.blocking_send(Ok(format!("{line}\n"))).is_err() {
                worker_cancel.store(true, Ordering::Relaxed);
            }
        };
        if let Err(e) =
            asm_core::live::send_cancellable(&session, &body.message, &worker_cancel, &mut emit)
        {
            emit(asm_core::live::LiveEvent::Done { ok: false, error: Some(e.to_string()) });
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "application/x-ndjson"),
            // Streaming through a proxy that buffers would defeat the
            // point; this is the header that asks it not to.
            (axum::http::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response())
}
