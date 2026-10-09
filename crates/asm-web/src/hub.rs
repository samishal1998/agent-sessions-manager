//! `asm hub serve`: the HTTP face of a hub store.
//!
//! A separate router from the web UI's, on purpose. The UI's API is
//! unauthenticated by design — a personal dashboard on loopback — and none
//! of it is mounted here. Every hub route requires a machine credential
//! except `join`, which is how a machine gets one; unmatched paths answer
//! 401 as well, so the hub does not describe itself to a stranger.
//!
//! The store underneath is synchronous core code; handlers reach it through
//! `spawn_blocking`. An upload is bridged from the async body to a blocking
//! reader through a bounded channel, so a fast client cannot buffer a whole
//! transcript into memory, and the store verifies the hash as it writes.

use std::io::Read;
use std::sync::Arc;

use anyhow::Context;
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Extension, Path, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;
use tokio_stream::StreamExt;

use asm_core::hub::commands::{Caps, NewCommand, NewPlan, Report};
use asm_core::hub::store::{Hub, HubError, Machine};

pub struct HubState {
    pub hub: Hub,
    /// Largest single file the hub accepts.
    pub max_blob: u64,
}

type Shared = Arc<HubState>;

/// Which token opened a commands route, for the record.
#[derive(Clone, Copy)]
struct Actor(&'static str);

fn error(status: StatusCode, message: impl ToString) -> Response {
    (status, Json(json!({ "error": message.to_string() }))).into_response()
}

fn hub_error(e: HubError) -> Response {
    match e {
        HubError::Unauthorized => error(StatusCode::UNAUTHORIZED, e),
        HubError::BadRequest(_) | HubError::ShaMismatch => error(StatusCode::BAD_REQUEST, e),
        HubError::NotFound => error(StatusCode::NOT_FOUND, e),
        HubError::Busy(_) => error(StatusCode::CONFLICT, e),
        HubError::TooLarge { .. } => error(StatusCode::PAYLOAD_TOO_LARGE, e),
        HubError::Conflict { ref head } => (
            StatusCode::CONFLICT,
            Json(json!({ "error": e.to_string(), "head": head })),
        )
            .into_response(),
        HubError::MissingBlobs(ref missing) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": e.to_string(), "missing": missing })),
        )
            .into_response(),
        HubError::Core(core) => error(StatusCode::INTERNAL_SERVER_ERROR, core),
    }
}

/// Run store code off the async runtime.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, HubError> + Send + 'static,
) -> Result<T, HubError> {
    tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| {
        Err(HubError::Core(asm_core::CoreError::Invalid { msg: format!("hub task failed: {e}") }))
    })
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// The admin page and its assets (a public bundle with no data in it), served
/// only while the hub has an admin token: without one the hub answers 401 to
/// them like to everything else.
fn is_admin_page(request: &Request) -> bool {
    let path = request.uri().path();
    request.method() == Method::GET
        && (matches!(path, "/admin" | "/admin/") || path.starts_with("/assets/"))
}

async fn authenticate(State(state): State<Shared>, mut request: Request, next: Next) -> Response {
    if request.method() == Method::POST && request.uri().path() == "/hub/v1/join" {
        return next.run(request).await;
    }
    // The admin API has its own credential. A machine's does not open it and
    // the admin token does not open the machine routes below.
    if request.uri().path().starts_with("/hub/v1/admin/") {
        let Some(token) = bearer(request.headers()) else {
            return error(StatusCode::UNAUTHORIZED, "the admin token is required");
        };
        let st = state.clone();
        return match blocking(move || st.hub.check_admin(&token)).await {
            Ok(()) => {
                let mut response = next.run(request).await;
                response
                    .headers_mut()
                    .insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
                response
            }
            Err(e) => hub_error(e),
        };
    }
    // Remote control's administrator side: its own token (or the admin one).
    let path = request.uri().path();
    if path.starts_with("/hub/v1/commands") || path.starts_with("/hub/v1/plans") {
        let Some(token) = bearer(request.headers()) else {
            return error(StatusCode::UNAUTHORIZED, "the commands token is required");
        };
        let st = state.clone();
        return match blocking(move || st.hub.check_commands(&token)).await {
            Ok(actor) => {
                request.extensions_mut().insert(Actor(actor));
                let mut response = next.run(request).await;
                response
                    .headers_mut()
                    .insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
                response
            }
            Err(e) => hub_error(e),
        };
    }
    if is_admin_page(&request) && state.hub.admin_enabled() {
        return next.run(request).await;
    }
    let Some(credential) = bearer(request.headers()) else {
        return error(StatusCode::UNAUTHORIZED, "a machine credential is required");
    };
    let st = state.clone();
    match blocking(move || st.hub.authenticate(&credential)).await {
        Ok(machine) => {
            request.extensions_mut().insert(machine);
            next.run(request).await
        }
        Err(e) => hub_error(e),
    }
}

pub fn router(state: Shared) -> axum::Router {
    axum::Router::new()
        // The one route open to anyone: it gets no more body than a join
        // needs, so an unauthenticated caller cannot make the hub buffer
        // the 64 MiB the routes below allow.
        .route("/hub/v1/join", post(join).layer(DefaultBodyLimit::max(4096)))
        .route("/hub/v1/machines", get(machines))
        .route("/hub/v1/sessions", get(sessions))
        .route("/hub/v1/sessions/{agent}/{id}", get(history).put(put_revision))
        .route("/hub/v1/sessions/{agent}/{id}/revisions/{rev}", get(revision))
        .route("/hub/v1/missing", post(missing))
        .route("/hub/v1/blobs/{sha}", get(get_blob).put(put_blob))
        .route("/hub/v1/inbox", get(inbox))
        .route("/hub/v1/inbox/{id}/claim", post(inbox_claim))
        .route("/hub/v1/inbox/{id}/result", post(inbox_result).layer(DefaultBodyLimit::max(8192)))
        .route("/hub/v1/commands", get(commands_list).post(commands_create).layer(DefaultBodyLimit::max(4096)))
        .route("/hub/v1/commands/{id}", get(command_get))
        .route("/hub/v1/commands/{id}/cancel", post(command_cancel))
        .route("/hub/v1/commands/{id}/retry", post(command_retry))
        .route("/hub/v1/plans", get(plans_list).post(plans_create).layer(DefaultBodyLimit::max(4096)))
        .route("/hub/v1/plans/{id}", get(plan_get))
        .route("/hub/v1/plans/{id}/cancel", post(plan_cancel))
        .route("/hub/v1/plans/{id}/retry", post(plan_retry))
        .route("/hub/v1/admin/overview", get(admin_overview))
        .route("/hub/v1/admin/machines", get(admin_machines))
        .route("/hub/v1/admin/machines/{id}/revoke", post(admin_revoke))
        .route("/hub/v1/admin/join-token/rotate", post(admin_rotate_join))
        .route("/hub/v1/admin/sessions", get(admin_sessions))
        .route("/hub/v1/admin/sessions/{agent}/{id}", get(history).delete(admin_delete_session))
        .route("/hub/v1/admin/sessions/{agent}/{id}/transcript", get(admin_transcript))
        .route("/hub/v1/admin/collect", post(admin_collect))
        .route("/hub/v1/admin/log", get(admin_log))
        .route("/admin", get(admin_page))
        .route("/admin/", get(admin_page))
        .route("/assets/{*file}", get(admin_asset))
        .fallback(|| async { error(StatusCode::NOT_FOUND, "not found") })
        // Manifests of large sessions list thousands of files. Blob bodies
        // are read as raw `Body`, which this limit does not govern; they
        // have their own cap, below.
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(axum::middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state)
}

async fn admin_page() -> Response {
    crate::statics::embedded("hub.html").unwrap_or_else(|| error(StatusCode::NOT_FOUND, "not found"))
}

async fn admin_asset(Path(file): Path<String>) -> Response {
    // Only files directly under assets/; the embedded set has no other layout.
    if file.contains("..") || file.contains('\\') {
        return error(StatusCode::NOT_FOUND, "not found");
    }
    crate::statics::embedded(&format!("assets/{file}")).unwrap_or_else(|| error(StatusCode::NOT_FOUND, "not found"))
}

async fn admin_overview(State(state): State<Shared>) -> Response {
    let st = state.clone();
    match blocking(move || {
        Ok(json!({
            "stats": st.hub.stats()?,
            "join_token": st.hub.join_token()?,
            "version": env!("CARGO_PKG_VERSION"),
            "root": st.hub.root().display().to_string(),
            "max_blob_bytes": st.max_blob,
        }))
    })
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn admin_machines(State(state): State<Shared>) -> Response {
    match blocking(move || state.hub.admin_machines().map_err(HubError::from)).await {
        Ok(m) => Json(m).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn admin_revoke(State(state): State<Shared>, Path(id): Path<String>) -> Response {
    match blocking(move || state.hub.admin_revoke(&id)).await {
        Ok(m) => Json(m).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn admin_rotate_join(State(state): State<Shared>) -> Response {
    match blocking(move || state.hub.admin_rotate_join_token().map_err(HubError::from)).await {
        Ok(token) => Json(json!({ "token": token })).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn admin_sessions(State(state): State<Shared>) -> Response {
    match blocking(move || state.hub.admin_sessions().map_err(HubError::from)).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn admin_delete_session(State(state): State<Shared>, Path((agent, id)): Path<(String, String)>) -> Response {
    match blocking(move || state.hub.delete_session(&agent, &id)).await {
        Ok(revisions) => Json(json!({ "deleted_revisions": revisions })).into_response(),
        Err(e) => hub_error(e),
    }
}

#[derive(Deserialize)]
struct CollectQuery {
    #[serde(default)]
    dry_run: bool,
}

async fn admin_collect(State(state): State<Shared>, axum::extract::Query(q): axum::extract::Query<CollectQuery>) -> Response {
    match blocking(move || state.hub.gc(q.dry_run).map_err(HubError::from)).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn admin_log(State(state): State<Shared>) -> Response {
    match blocking(move || Ok(state.hub.audit_log(50))).await {
        Ok(entries) => Json(entries).into_response(),
        Err(e) => hub_error(e),
    }
}

#[derive(Deserialize)]
struct JoinBody {
    token: String,
    name: String,
    /// The joining box's identity; an older client sends none.
    #[serde(default)]
    uid: Option<String>,
    /// The credential of this machine's last registration, which proves it
    /// owns that record.
    #[serde(default)]
    previous: Option<String>,
}

#[derive(Deserialize)]
struct InboxQuery {
    v: Option<u32>,
    ops: Option<String>,
    enabled: Option<u8>,
    /// Agents the machine cannot archive (an older daemon sends none).
    no_archive: Option<String>,
}

/// What a machine's poll says it can do. Only the operations and agents this
/// hub knows are kept.
fn caps_of(q: InboxQuery) -> Caps {
    let ops: Vec<String> = q
        .ops
        .unwrap_or_default()
        .split(',')
        .filter(|o| asm_core::hub::commands::OPS.iter().any(|k| k.as_str() == *o))
        .map(String::from)
        .collect();
    let no_archive: Vec<String> = q
        .no_archive
        .unwrap_or_default()
        .split(',')
        .filter_map(asm_core::model::AgentKind::parse)
        .map(|a| a.as_str().to_string())
        .collect();
    Caps { v: q.v.unwrap_or(1), ops, enabled: q.enabled == Some(1), no_archive }
}

/// A machine asks for its commands and says what it can do. Only the
/// operations this hub knows are kept.
async fn inbox(
    State(state): State<Shared>,
    Extension(machine): Extension<Machine>,
    axum::extract::Query(q): axum::extract::Query<InboxQuery>,
) -> Response {
    let caps = caps_of(q);
    match blocking(move || state.hub.inbox(&machine, caps)).await {
        Ok(work) => Json(work).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn inbox_claim(State(state): State<Shared>, Extension(machine): Extension<Machine>, Path(id): Path<String>) -> Response {
    match blocking(move || state.hub.claim(&machine, &id)).await {
        Ok(work) => Json(work).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn inbox_result(
    State(state): State<Shared>,
    Extension(machine): Extension<Machine>,
    Path(id): Path<String>,
    Json(report): Json<Report>,
) -> Response {
    match blocking(move || state.hub.report(&machine, &id, report)).await {
        Ok(command) => Json(command).into_response(),
        Err(e) => hub_error(e),
    }
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<usize>,
}

async fn commands_list(State(state): State<Shared>, axum::extract::Query(q): axum::extract::Query<ListQuery>) -> Response {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    match blocking(move || Ok(state.hub.commands(limit)?)).await {
        Ok(list) => Json(list).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn commands_create(State(state): State<Shared>, Extension(Actor(by)): Extension<Actor>, Json(request): Json<NewCommand>) -> Response {
    match blocking(move || state.hub.enqueue(request, by)).await {
        Ok(command) => (StatusCode::CREATED, Json(command)).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn command_get(State(state): State<Shared>, Path(id): Path<String>) -> Response {
    match blocking(move || state.hub.command(&id)).await {
        Ok(command) => Json(command).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn command_cancel(State(state): State<Shared>, Extension(Actor(by)): Extension<Actor>, Path(id): Path<String>) -> Response {
    match blocking(move || state.hub.cancel_command(&id, by)).await {
        Ok(command) => Json(command).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn command_retry(State(state): State<Shared>, Extension(Actor(by)): Extension<Actor>, Path(id): Path<String>) -> Response {
    match blocking(move || state.hub.retry_command(&id, by)).await {
        Ok(command) => Json(command).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn plans_list(State(state): State<Shared>, axum::extract::Query(q): axum::extract::Query<ListQuery>) -> Response {
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    match blocking(move || state.hub.plans(limit)).await {
        Ok(list) => Json(list).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn plans_create(State(state): State<Shared>, Extension(Actor(by)): Extension<Actor>, Json(request): Json<NewPlan>) -> Response {
    match blocking(move || state.hub.create_plan(request, by)).await {
        Ok(plan) => (StatusCode::CREATED, Json(plan)).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn plan_get(State(state): State<Shared>, Path(id): Path<String>) -> Response {
    match blocking(move || state.hub.plan(&id)).await {
        Ok(plan) => Json(plan).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn plan_cancel(State(state): State<Shared>, Extension(Actor(by)): Extension<Actor>, Path(id): Path<String>) -> Response {
    match blocking(move || state.hub.cancel_plan(&id, by)).await {
        Ok(plan) => Json(plan).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn plan_retry(State(state): State<Shared>, Extension(Actor(by)): Extension<Actor>, Path(id): Path<String>) -> Response {
    match blocking(move || state.hub.retry_plan(&id, by)).await {
        Ok(plan) => Json(plan).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn join(State(state): State<Shared>, Json(body): Json<JoinBody>) -> Response {
    match blocking(move || state.hub.join_as(&body.token, &body.name, body.uid.as_deref(), body.previous.as_deref())).await {
        Ok(joined) => (StatusCode::CREATED, Json(joined)).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn machines(State(state): State<Shared>) -> Response {
    match blocking(move || state.hub.machines().map_err(HubError::from)).await {
        Ok(m) => Json(m).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn sessions(State(state): State<Shared>) -> Response {
    match blocking(move || state.hub.heads().map_err(HubError::from)).await {
        Ok(h) => Json(h).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn history(State(state): State<Shared>, Path((agent, id)): Path<(String, String)>) -> Response {
    match blocking(move || state.hub.history(&agent, &id)).await {
        Ok(h) => Json(h).into_response(),
        Err(e) => hub_error(e),
    }
}

/// Most recent messages the transcript endpoint returns.
const TRANSCRIPT_MESSAGES: usize = 2000;

async fn admin_transcript(State(state): State<Shared>, Path((agent, id)): Path<(String, String)>) -> Response {
    match blocking(move || asm_core::hub::transcript::render(&state.hub, &agent, &id, TRANSCRIPT_MESSAGES)).await {
        Ok(view) => Json(view).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn revision(
    State(state): State<Shared>,
    Path((agent, id, rev)): Path<(String, String, String)>,
) -> Response {
    match blocking(move || state.hub.revision(&agent, &id, &rev)).await {
        Ok(m) => Json(m).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn put_revision(
    State(state): State<Shared>,
    Extension(machine): Extension<Machine>,
    Path((agent, id)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    match blocking(move || state.hub.put_revision(&agent, &id, &body, &machine)).await {
        Ok(rev) => (StatusCode::CREATED, Json(json!({ "rev": rev }))).into_response(),
        Err(e) => hub_error(e),
    }
}

#[derive(Deserialize)]
struct MissingBody {
    shas: Vec<String>,
}

async fn missing(State(state): State<Shared>, Json(body): Json<MissingBody>) -> Response {
    match blocking(move || state.hub.missing(&body.shas)).await {
        Ok(missing) => Json(json!({ "missing": missing })).into_response(),
        Err(e) => hub_error(e),
    }
}

async fn get_blob(State(state): State<Shared>, Path(sha): Path<String>) -> Response {
    let path = match blocking(move || state.hub.blob(&sha)).await {
        Ok(path) => path,
        Err(e) => return hub_error(e),
    };
    // Streamed from disk through a bounded channel — the upload path in
    // reverse — so a blob of hundreds of megabytes never sits in memory.
    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(8);
    tokio::task::spawn_blocking(move || {
        let mut file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                let _ = tx.blocking_send(Err(e));
                return;
            }
        };
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    // A send error means the client went away.
                    if tx.blocking_send(Ok(Bytes::copy_from_slice(&buf[..n]))).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    break;
                }
            }
        }
    });
    let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    ([(header::CONTENT_TYPE, "application/octet-stream")], body).into_response()
}

/// The blocking half of an upload: reads what the async half forwards.
struct ChannelReader {
    rx: tokio::sync::mpsc::Receiver<std::io::Result<Bytes>>,
    buf: Bytes,
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.buf.is_empty() {
            match self.rx.blocking_recv() {
                Some(Ok(chunk)) => self.buf = chunk,
                Some(Err(e)) => return Err(e),
                None => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len());
        out[..n].copy_from_slice(&self.buf[..n]);
        self.buf = self.buf.slice(n..);
        Ok(n)
    }
}

async fn put_blob(
    State(state): State<Shared>,
    Path(sha): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let limit = state.max_blob;
    // An honest client says how big it is; refuse before a byte is stored.
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|len| len > limit) {
        return hub_error(HubError::TooLarge { limit });
    }

    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(8);
    let st = state.clone();
    let writer = tokio::task::spawn_blocking(move || {
        st.hub.put_blob(&sha, ChannelReader { rx, buf: Bytes::new() }, limit)
    });
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(std::io::Error::other);
        let failed = chunk.is_err();
        // A closed channel means the store stopped reading — over the cap,
        // most likely — so the rest of the body is not wanted.
        if tx.send(chunk).await.is_err() || failed {
            break;
        }
    }
    drop(tx);
    match writer.await {
        Ok(Ok(true)) => (StatusCode::CREATED, Json(json!({ "stored": true }))).into_response(),
        Ok(Ok(false)) => (StatusCode::OK, Json(json!({ "stored": false }))).into_response(),
        Ok(Err(e)) => hub_error(e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

/// Serve the hub at `<data>/hub` until interrupted.
pub fn run(host: &str, port: u16, max_blob: u64) -> anyhow::Result<()> {
    let root = Hub::default_root().context("cannot determine asm's data directory")?;
    let hub = Hub::open(&root).with_context(|| format!("cannot open the hub store at {}", root.display()))?;
    let token = hub.join_token()?;
    let admin = hub.admin_enabled();
    let addr = crate::resolve_bind(host, port)?;
    let state = Arc::new(HubState { hub, max_blob });

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("cannot bind {addr}"))?;
        let advertise = if addr.ip().is_unspecified() {
            asm_core::process::hostname().unwrap_or_else(|| "this-host".into())
        } else {
            crate::display_addr(&addr).rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or_default()
        };
        eprintln!("asm hub on http://{}/  (ctrl-c to stop)", crate::display_addr(&addr));
        eprintln!("  store      {}", root.display());
        eprintln!("  join with  ASM_JOIN_TOKEN={token} asm join http://{advertise}:{}", addr.port());
        if admin {
            eprintln!("  admin      http://{}/admin  (token: `asm hub admin-token`)", crate::display_addr(&addr));
        } else {
            eprintln!("  admin      off — `asm hub admin-token` turns on the admin page");
        }
        if addr.ip().is_loopback() {
            eprintln!(
                "\n  Bound to loopback: only this machine can reach it. Publish it through \
                 HTTPS (a reverse proxy, `tailscale serve`) and join with that URL, or pass \
                 --host to bind a network you trust."
            );
        } else {
            eprintln!(
                "\n  Plain HTTP: transcripts and credentials cross the network unencrypted. \
                 Keep this on a network you trust, or put HTTPS in front of it."
            );
        }
        axum::serve(listener, router(state)).with_graceful_shutdown(shutdown()).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    fn app() -> (tempfile::TempDir, Shared, axum::Router) {
        let dir = tempfile::tempdir().unwrap();
        let hub = Hub::open(&dir.path().join("hub")).unwrap();
        let state = Arc::new(HubState { hub, max_blob: 4096 });
        let router = router(state.clone());
        (dir, state, router)
    }

    async fn send(
        app: &axum::Router,
        method: &str,
        uri: &str,
        credential: Option<&str>,
        body: Body,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = HttpRequest::builder().method(method).uri(uri);
        if let Some(c) = credential {
            req = req.header("authorization", format!("Bearer {c}"));
        }
        req = req.header("content-type", "application/json");
        let response = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    async fn credential(app: &axum::Router, state: &Shared) -> String {
        let body = json!({ "token": state.hub.join_token().unwrap(), "name": "laptop" }).to_string();
        let (status, v) = send(app, "POST", "/hub/v1/join", None, Body::from(body)).await;
        assert_eq!(status, StatusCode::CREATED);
        v["credential"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn the_admin_transcript_renders_a_stored_session() {
        let (_d, state, app) = app();
        let cred = credential(&app, &state).await;
        let admin = state.hub.rotate_admin_token().unwrap();
        let id = "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43";
        let uri = format!("/hub/v1/admin/sessions/claude-code/{id}/transcript");

        let (status, _) = send(&app, "GET", &uri, Some(&cred), Body::empty()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "a machine credential is not admin");
        let (status, _) = send(&app, "GET", &uri, Some(&admin), Body::empty()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let line = |kind: &str, i: u8, text: &str| {
            let content = if kind == "user" { json!(text) } else { json!([{ "type": "text", "text": text }]) };
            format!(
                "{}\n",
                json!({ "type": kind, "cwd": "/home/a/x", "sessionId": id, "uuid": format!("u{i}"),
                        "parentUuid": i.checked_sub(1).map(|p| format!("u{p}")),
                        "timestamp": format!("2026-09-22T10:00:0{i}Z"),
                        "message": { "role": kind, "content": content } })
            )
        };
        let text = line("user", 0, "hello hub") + &line("assistant", 1, "hello back");
        let sha = asm_core::fsutil::sha256_hex(text.as_bytes());
        state.hub.put_blob(&sha, text.as_bytes(), 1 << 20).unwrap();
        let manifest = json!({
            "schema": 1, "agent": "claude-code", "id": id, "project_root": "/home/a/x",
            "project_root_portable": "${HOME}/x", "canonical": sha, "parent_rev": null,
            "files": [{ "path": "transcript.jsonl", "sha256": sha, "size": text.len() }],
        });
        let (status, _) = send(&app, "PUT", &format!("/hub/v1/sessions/claude-code/{id}"), Some(&cred), Body::from(manifest.to_string())).await;
        assert!(status.is_success(), "{status}");

        let (status, v) = send(&app, "GET", &uri, Some(&admin), Body::empty()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!((v["available"].as_bool(), v["truncated"].as_bool()), (Some(true), Some(false)));
        assert_eq!(v["ir"]["messages"].as_array().unwrap().len(), 2, "{v}");
        assert!(v.to_string().contains("hello hub"));
    }

    /// The admin API is its own door: shut until an admin token exists,
    /// then opened only by that token — never by a machine's credential or
    /// the join token — and it opens none of the machine routes.
    #[tokio::test]
    async fn the_admin_api_answers_only_to_the_admin_token() {
        let (_d, state, app) = app();
        let cred = credential(&app, &state).await;
        let join = state.hub.join_token().unwrap();
        let routes = [
            ("GET", "/hub/v1/admin/overview"),
            ("GET", "/hub/v1/admin/machines"),
            ("GET", "/hub/v1/admin/sessions"),
            ("GET", "/hub/v1/admin/log"),
            ("POST", "/hub/v1/admin/collect"),
            ("POST", "/hub/v1/admin/join-token/rotate"),
            ("POST", "/hub/v1/admin/machines/x/revoke"),
            ("DELETE", "/hub/v1/admin/sessions/claude-code/x"),
        ];
        // No admin token minted: nothing opens it, and neither does a
        // credential.
        for (method, uri) in routes {
            for who in [None, Some(cred.as_str()), Some(join.as_str()), Some("asma_guess")] {
                let (status, _) = send(&app, method, uri, who, Body::empty()).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri} {who:?}");
            }
        }
        let (status, _) = send(&app, "GET", "/admin", None, Body::empty()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "no admin page without a token");

        let admin = state.hub.rotate_admin_token().unwrap();
        for (method, uri) in routes {
            for who in [None, Some(cred.as_str()), Some(join.as_str())] {
                let (status, _) = send(&app, method, uri, who, Body::empty()).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri} {who:?} once enabled");
            }
        }
        let (status, v) = send(&app, "GET", "/hub/v1/admin/overview", Some(&admin), Body::empty()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["stats"]["machines"], 1);
        assert!(v["join_token"].as_str().unwrap().starts_with("asmj_"));
        // The admin token is no machine credential.
        let (status, _) = send(&app, "GET", "/hub/v1/machines", Some(&admin), Body::empty()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = send(&app, "GET", "/admin", None, Body::empty()).await;
        assert_eq!(status, StatusCode::OK, "the page is served once an admin token exists");
    }

    #[tokio::test]
    async fn an_admin_can_revoke_rotate_collect_and_it_is_recorded() {
        let (_d, state, app) = app();
        let cred = credential(&app, &state).await;
        let admin = state.hub.rotate_admin_token().unwrap();

        let (status, v) = send(&app, "GET", "/hub/v1/admin/machines", Some(&admin), Body::empty()).await;
        assert_eq!((status, v[0]["name"].as_str()), (StatusCode::OK, Some("laptop")));
        assert!(v[0].get("credential_sha256").is_none());
        let id = v[0]["id"].as_str().unwrap().to_string();

        let (status, v) = send(&app, "POST", "/hub/v1/admin/collect?dry_run=true", Some(&admin), Body::empty()).await;
        assert_eq!((status, v["dry_run"].as_bool()), (StatusCode::OK, Some(true)));

        let old = state.hub.join_token().unwrap();
        let (status, v) =
            send(&app, "POST", "/hub/v1/admin/join-token/rotate", Some(&admin), Body::empty()).await;
        assert_eq!(status, StatusCode::OK);
        assert_ne!(v["token"].as_str().unwrap(), old);

        let (status, _) =
            send(&app, "POST", &format!("/hub/v1/admin/machines/{id}/revoke"), Some(&admin), Body::empty()).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = send(&app, "GET", "/hub/v1/machines", Some(&cred), Body::empty()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "a revoked machine is out");

        let (status, v) = send(&app, "GET", "/hub/v1/admin/log", Some(&admin), Body::empty()).await;
        assert_eq!(status, StatusCode::OK);
        let actions: Vec<&str> = v.as_array().unwrap().iter().filter_map(|e| e["action"].as_str()).collect();
        assert!(actions.contains(&"revoke machine") && actions.contains(&"rotate join token"), "{actions:?}");
    }

    /// Nothing but join answers without a credential — including paths that
    /// do not exist, so a stranger cannot even map the API.
    #[tokio::test]
    async fn every_route_but_join_needs_a_credential() {
        let (_d, _s, app) = app();
        for (method, uri) in [
            ("GET", "/hub/v1/machines"),
            ("GET", "/hub/v1/sessions"),
            ("GET", "/hub/v1/sessions/claude-code/x"),
            ("PUT", "/hub/v1/sessions/claude-code/x"),
            ("POST", "/hub/v1/missing"),
            ("GET", "/hub/v1/blobs/00"),
            ("PUT", "/hub/v1/blobs/00"),
            ("GET", "/hub/v1/inbox"),
            ("POST", "/hub/v1/inbox/x/claim"),
            ("POST", "/hub/v1/inbox/x/result"),
            ("GET", "/hub/v1/commands"),
            ("POST", "/hub/v1/commands"),
            ("GET", "/hub/v1/commands/x"),
            ("POST", "/hub/v1/commands/x/cancel"),
            ("POST", "/hub/v1/commands/x/retry"),
            ("GET", "/hub/v1/plans"),
            ("POST", "/hub/v1/plans"),
            ("GET", "/hub/v1/plans/x"),
            ("POST", "/hub/v1/plans/x/cancel"),
            ("POST", "/hub/v1/plans/x/retry"),
            ("GET", "/no/such/route"),
            ("GET", "/api/sessions"),
        ] {
            for cred in [None, Some("asmc_garbage")] {
                let (status, _) = send(&app, method, uri, cred, Body::empty()).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri} with {cred:?}");
            }
        }
    }

    /// The poll says which agents the machine cannot archive; an older
    /// daemon says nothing, and what the hub does not know is dropped.
    #[test]
    fn a_poll_reports_what_the_machine_cannot_archive() {
        let q = |no_archive: Option<&str>| InboxQuery {
            v: Some(1),
            ops: Some("push,pull,archive,bogus".into()),
            enabled: Some(1),
            no_archive: no_archive.map(String::from),
        };
        let caps = caps_of(q(Some("opencode,not-an-agent,claude-code")));
        assert_eq!(caps.ops, ["push", "pull", "archive"]);
        assert_eq!(caps.no_archive, ["opencode", "claude-code"]);
        assert!(caps_of(q(None)).no_archive.is_empty(), "an older daemon");
        assert!(caps_of(q(Some(""))).no_archive.is_empty());
    }

    /// Remote control through the door that matters: a machine asks and
    /// answers with its credential, an administrator creates with a token that
    /// opens nothing else, and neither opens the other's routes.
    #[tokio::test]
    async fn remote_control_runs_a_command_from_creation_to_result() {
        let (_d, state, app) = app();
        let cred = credential(&app, &state).await;
        let inbox = "/hub/v1/inbox?v=1&ops=push,pull&enabled=1";
        let (status, v) = send(&app, "GET", inbox, Some(&cred), Body::empty()).await;
        assert_eq!((status, v.as_array().map(Vec::len)), (StatusCode::OK, Some(0)));

        let ask = json!({ "op": "push", "machine": "laptop", "agent": "claude-code", "session": "s1" }).to_string();
        let (status, _) = send(&app, "POST", "/hub/v1/commands", Some(&cred), Body::from(ask.clone())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "a machine cannot create commands");
        let (status, _) = send(&app, "POST", "/hub/v1/commands", Some("asmk_none"), Body::from(ask.clone())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "no commands token exists yet");

        let token = state.hub.rotate_commands_token().unwrap();
        let admin = state.hub.rotate_admin_token().unwrap();
        let (status, _) = send(&app, "GET", "/hub/v1/admin/overview", Some(&token), Body::empty()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "the commands token opens no admin route");
        let (status, _) = send(&app, "GET", inbox, Some(&token), Body::empty()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "nor a machine's");
        let (status, _) = send(&app, "GET", "/hub/v1/commands", Some(&cred), Body::empty()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, made) = send(&app, "POST", "/hub/v1/commands", Some(&token), Body::from(ask)).await;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        assert_eq!((made["state"].as_str(), made["created_by"].as_str()), (Some("queued"), Some("commands-token")));
        let id = made["id"].as_str().unwrap().to_string();
        let again = json!({ "op": "push", "machine": "laptop", "agent": "claude-code", "session": "s1" }).to_string();
        let (status, v) = send(&app, "POST", "/hub/v1/commands", Some(&admin), Body::from(again)).await;
        assert_eq!(status, StatusCode::CONFLICT, "one command per session: {v}");

        let (status, v) = send(&app, "GET", inbox, Some(&cred), Body::empty()).await;
        assert_eq!((status, v[0]["id"].as_str(), v[0]["op"].as_str()), (StatusCode::OK, Some(id.as_str()), Some("push")));
        assert!(v[0].get("created_by").is_none(), "the machine sees the work, not the record");
        let claim = format!("/hub/v1/inbox/{id}/claim");
        assert_eq!(send(&app, "POST", &claim, Some(&cred), Body::empty()).await.0, StatusCode::OK);
        assert_eq!(send(&app, "POST", &claim, Some(&cred), Body::empty()).await.0, StatusCode::CONFLICT);

        let result = json!({ "code": "diverged", "detail": "both machines continued it" }).to_string();
        let (status, v) = send(&app, "POST", &format!("/hub/v1/inbox/{id}/result"), Some(&cred), Body::from(result)).await;
        assert_eq!((status, v["state"].as_str(), v["code"].as_str()), (StatusCode::OK, Some("blocked"), Some("diverged")));

        let (status, v) = send(&app, "GET", &format!("/hub/v1/commands/{id}"), Some(&token), Body::empty()).await;
        assert_eq!((status, v["detail"].as_str()), (StatusCode::OK, Some("both machines continued it")));
        let (status, v) = send(&app, "POST", &format!("/hub/v1/commands/{id}/retry"), Some(&token), Body::empty()).await;
        assert_eq!((status, v["state"].as_str()), (StatusCode::OK, Some("queued")));
        let (status, v) = send(&app, "POST", &format!("/hub/v1/commands/{id}/cancel"), Some(&token), Body::empty()).await;
        assert_eq!((status, v["state"].as_str()), (StatusCode::OK, Some("cancelled")));
        // The record says who asked for each change.
        let log = std::fs::read_to_string(state.hub.root().join("commands.log")).unwrap();
        for event in ["retry", "cancel"] {
            let line = log.lines().find(|l| l.contains(&format!("\"event\":\"{event}\""))).unwrap();
            assert!(line.contains("\"by\":\"commands-token\""), "{line}");
        }
        let (status, v) = send(&app, "GET", "/hub/v1/admin/machines", Some(&admin), Body::empty()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v[0]["remote"]["enabled"].as_bool(), Some(true), "the admin page sees what the machine reported");
    }

    /// Sending a session from one machine to another, through the door that
    /// matters: the plan is made with the commands token, each machine only
    /// ever sees its own step, and the pull gets the revision the push made.
    #[tokio::test]
    async fn a_plan_sends_a_session_from_one_machine_to_another() {
        let (_d, state, app) = app();
        let alpha = credential(&app, &state).await;
        let body = json!({ "token": state.hub.join_token().unwrap(), "name": "desk" }).to_string();
        let (status, v) = send(&app, "POST", "/hub/v1/join", None, Body::from(body)).await;
        assert_eq!(status, StatusCode::CREATED);
        let beta = v["credential"].as_str().unwrap().to_string();
        let inbox = "/hub/v1/inbox?v=1&ops=push,pull&enabled=1";
        for who in [&alpha, &beta] {
            assert_eq!(send(&app, "GET", inbox, Some(who), Body::empty()).await.0, StatusCode::OK);
        }
        let id = "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43";
        let ask = |session: &str, from: &str, to: &str| {
            json!({ "kind": "send", "agent": "claude-code", "session": session, "from": from, "to": to }).to_string()
        };

        // Who may make a plan: the commands token or the admin token, no machine.
        for (method, uri) in [("POST", "/hub/v1/plans"), ("GET", "/hub/v1/plans"), ("GET", "/hub/v1/plans/x")] {
            for who in [None, Some(alpha.as_str()), Some("asmk_none")] {
                assert_eq!(send(&app, method, uri, who, Body::from(ask(id, "laptop", "desk"))).await.0, StatusCode::UNAUTHORIZED, "{method} {uri} {who:?}");
            }
        }
        let token = state.hub.rotate_commands_token().unwrap();
        let admin = state.hub.rotate_admin_token().unwrap();
        assert_eq!(send(&app, "GET", "/hub/v1/plans", Some(&alpha), Body::empty()).await.0, StatusCode::UNAUTHORIZED, "a machine cannot read plans");
        assert_eq!(send(&app, "GET", "/hub/v1/plans", Some(&beta), Body::empty()).await.0, StatusCode::UNAUTHORIZED);

        // Refusals say why, and leave nothing behind.
        let (status, v) = send(&app, "POST", "/hub/v1/plans", Some(&token), Body::from(ask(id, "laptop", "laptop"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(v["error"].as_str().unwrap().contains("both ends"), "{v}");
        let (status, _) = send(&app, "POST", "/hub/v1/plans", Some(&token), Body::from(ask(id, "laptop", "nobody"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (_, v) = send(&app, "GET", "/hub/v1/plans", Some(&token), Body::empty()).await;
        assert_eq!(v.as_array().map(Vec::len), Some(0));

        let (status, plan) = send(&app, "POST", "/hub/v1/plans", Some(&token), Body::from(ask(id, "laptop", "desk"))).await;
        assert_eq!(status, StatusCode::CREATED, "{plan}");
        assert_eq!((plan["kind"].as_str(), plan["state"].as_str()), (Some("send"), Some("queued")));
        let steps = plan["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        let (push, pull) = (steps[0]["id"].as_str().unwrap().to_string(), steps[1]["id"].as_str().unwrap().to_string());
        assert_eq!((steps[0]["step"].as_u64(), steps[1]["step"].as_u64()), (Some(1), Some(2)));
        assert_eq!((steps[1]["state"].as_str(), steps[1]["needs"].as_str()), (Some("pending"), Some(push.as_str())));
        assert!(steps[1]["rev"].is_null(), "no revision yet: {plan}");
        assert_eq!(steps[1]["plan"], plan["id"]);
        let plan_id = plan["id"].as_str().unwrap().to_string();

        let (status, v) = send(&app, "POST", "/hub/v1/plans", Some(&admin), Body::from(ask(id, "laptop", "desk"))).await;
        assert_eq!(status, StatusCode::CONFLICT, "one command per session, plans included: {v}");

        // Each machine sees only its own step; the pull not yet.
        let (_, v) = send(&app, "GET", inbox, Some(&beta), Body::empty()).await;
        assert_eq!(v.as_array().map(Vec::len), Some(0), "the pending pull is not offered");
        let (_, v) = send(&app, "GET", inbox, Some(&alpha), Body::empty()).await;
        assert_eq!((v[0]["id"].as_str(), v[0]["op"].as_str()), (Some(push.as_str()), Some("push")));
        assert_eq!(send(&app, "POST", &format!("/hub/v1/inbox/{pull}/claim"), Some(&beta), Body::empty()).await.0, StatusCode::CONFLICT);
        assert_eq!(send(&app, "POST", &format!("/hub/v1/inbox/{push}/claim"), Some(&alpha), Body::empty()).await.0, StatusCode::OK);

        // The push really uploads a copy; a result naming a revision that is not
        // there is refused and moves nothing.
        let sha = asm_core::fsutil::sha256_hex(b"line\n");
        send(&app, "PUT", &format!("/hub/v1/blobs/{sha}"), Some(&alpha), Body::from("line\n")).await;
        let manifest = json!({
            "schema": 1, "agent": "claude-code", "id": id, "project_root": "/x", "project_root_portable": "/x",
            "canonical": "c", "parent_rev": null, "files": [{ "path": "transcript.jsonl", "sha256": sha, "size": 5 }]
        });
        let (status, v) = send(&app, "PUT", &format!("/hub/v1/sessions/claude-code/{id}"), Some(&alpha), Body::from(manifest.to_string())).await;
        assert_eq!(status, StatusCode::CREATED);
        let rev = v["rev"].as_str().unwrap().to_string();
        let result = |code: &str, rev: Option<&str>| json!({ "code": code, "rev": rev }).to_string();
        let bogus = "0".repeat(64);
        let (status, _) = send(&app, "POST", &format!("/hub/v1/inbox/{push}/result"), Some(&alpha), Body::from(result("ok", Some(&bogus)))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (_, v) = send(&app, "GET", &format!("/hub/v1/plans/{plan_id}"), Some(&token), Body::empty()).await;
        assert_eq!(v["state"].as_str(), Some("running"));

        // The session cannot be deleted from under the plan.
        let (status, v) = send(&app, "DELETE", &format!("/hub/v1/admin/sessions/claude-code/{id}"), Some(&admin), Body::empty()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{v}");
        assert!(v["error"].as_str().unwrap().contains("still in flight"));

        let (status, v) = send(&app, "POST", &format!("/hub/v1/inbox/{push}/result"), Some(&alpha), Body::from(result("ok", Some(&rev)))).await;
        assert_eq!((status, v["state"].as_str(), v["rev"].as_str()), (StatusCode::OK, Some("ok"), Some(rev.as_str())));

        // Now the pull is offered, pinned to that revision, and nobody else has it.
        let (_, v) = send(&app, "GET", inbox, Some(&beta), Body::empty()).await;
        assert_eq!((v[0]["id"].as_str(), v[0]["op"].as_str(), v[0]["rev"].as_str()), (Some(pull.as_str()), Some("pull"), Some(rev.as_str())));
        let (_, v) = send(&app, "GET", inbox, Some(&alpha), Body::empty()).await;
        assert_eq!(v.as_array().map(Vec::len), Some(0));
        let (_, v) = send(&app, "GET", &format!("/hub/v1/commands/{pull}"), Some(&token), Body::empty()).await;
        assert_eq!((v["state"].as_str(), v["rev"].as_str(), v["plan"].as_str()), (Some("queued"), Some(rev.as_str()), Some(plan_id.as_str())));
        assert_eq!(send(&app, "POST", &format!("/hub/v1/inbox/{pull}/claim"), Some(&beta), Body::empty()).await.0, StatusCode::OK);
        let (status, _) = send(&app, "POST", &format!("/hub/v1/inbox/{pull}/result"), Some(&beta), Body::from(result("ok", None))).await;
        assert_eq!(status, StatusCode::OK);

        let (status, v) = send(&app, "GET", &format!("/hub/v1/plans/{plan_id}"), Some(&admin), Body::empty()).await;
        assert_eq!((status, v["state"].as_str()), (StatusCode::OK, Some("ok")));
        assert!(v["steps"].as_array().unwrap().iter().all(|s| s["state"] == "ok"));
        let (_, v) = send(&app, "GET", "/hub/v1/plans?limit=5", Some(&token), Body::empty()).await;
        assert_eq!((v.as_array().map(Vec::len), v[0]["id"].as_str()), (Some(1), Some(plan_id.as_str())));
        let (_, v) = send(&app, "GET", "/hub/v1/commands", Some(&token), Body::empty()).await;
        assert_eq!(v.as_array().map(Vec::len), Some(2), "plan steps are commands too");
        assert_eq!(send(&app, "GET", "/hub/v1/plans/nope", Some(&token), Body::empty()).await.0, StatusCode::NOT_FOUND);
        // Finished: nothing to cancel or retry (400), and an unknown plan is 404.
        for route in ["cancel", "retry"] {
            let (status, _) = send(&app, "POST", &format!("/hub/v1/plans/{plan_id}/{route}"), Some(&token), Body::empty()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{route}");
            let (status, _) = send(&app, "POST", &format!("/hub/v1/plans/nope/{route}"), Some(&token), Body::empty()).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{route}");
        }

        // Cancel and retry a second plan.
        let (status, plan) = send(&app, "POST", "/hub/v1/plans", Some(&token), Body::from(ask(id, "desk", "laptop"))).await;
        assert_eq!(status, StatusCode::CREATED, "{plan}");
        let plan_id = plan["id"].as_str().unwrap().to_string();
        let (status, v) = send(&app, "POST", &format!("/hub/v1/plans/{plan_id}/cancel"), Some(&token), Body::empty()).await;
        assert_eq!((status, v["state"].as_str()), (StatusCode::OK, Some("cancelled")));
        assert_eq!((v["steps"][0]["state"].as_str(), v["steps"][1]["skipped"].as_bool()), (Some("cancelled"), Some(true)));
        let (status, v) = send(&app, "POST", &format!("/hub/v1/plans/{plan_id}/retry"), Some(&token), Body::empty()).await;
        assert_eq!((status, v["state"].as_str()), (StatusCode::OK, Some("queued")));
        assert_eq!((v["steps"][0]["state"].as_str(), v["steps"][1]["state"].as_str()), (Some("queued"), Some("pending")));
        assert!(v["steps"][1].get("skipped").is_none());
    }

    /// Moving: push on one machine, pull on another, then archive on the first,
    /// through the routes. Nothing is made without the confirmation, and the
    /// archive is not offered before the pull succeeded.
    #[tokio::test]
    async fn a_move_archives_the_source_only_once_the_destination_has_the_copy() {
        let (_d, state, app) = app();
        let alpha = credential(&app, &state).await;
        let body = json!({ "token": state.hub.join_token().unwrap(), "name": "desk" }).to_string();
        let (_, v) = send(&app, "POST", "/hub/v1/join", None, Body::from(body)).await;
        let beta = v["credential"].as_str().unwrap().to_string();
        let ids: std::collections::HashMap<String, String> = state.hub.machines().unwrap().into_iter().map(|m| (m.name, m.id)).collect();
        // The source allows archive; an op this hub does not know is dropped.
        let inbox_a = "/hub/v1/inbox?v=1&ops=push,pull,archive,rm&enabled=1";
        let inbox_b = "/hub/v1/inbox?v=1&ops=push,pull&enabled=1";
        assert_eq!(send(&app, "GET", inbox_a, Some(&alpha), Body::empty()).await.0, StatusCode::OK);
        assert_eq!(send(&app, "GET", inbox_b, Some(&beta), Body::empty()).await.0, StatusCode::OK);
        let token = state.hub.rotate_commands_token().unwrap();
        let admin = state.hub.rotate_admin_token().unwrap();
        let (_, v) = send(&app, "GET", "/hub/v1/admin/machines", Some(&admin), Body::empty()).await;
        let laptop = v.as_array().unwrap().iter().find(|m| m["name"] == "laptop").unwrap();
        assert_eq!(laptop["remote"]["ops"], json!(["push", "pull", "archive"]));

        let id = "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43";
        let ask = |confirm: Option<&str>, from: &str, to: &str| {
            json!({ "kind": "move", "agent": "claude-code", "session": id, "from": from, "to": to, "confirm_archive": confirm }).to_string()
        };
        let laptop_id = ids["laptop"].clone();
        for who in [None, Some(alpha.as_str()), Some(beta.as_str()), Some("asmk_none")] {
            let (status, _) = send(&app, "POST", "/hub/v1/plans", who, Body::from(ask(Some(&laptop_id), "laptop", "desk"))).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{who:?}");
        }
        // No confirmation, the wrong one (a name, the other machine's id): refused with what to type.
        for wrong in [None, Some("laptop"), Some(ids["desk"].as_str())] {
            let (status, v) = send(&app, "POST", "/hub/v1/plans", Some(&token), Body::from(ask(wrong, "laptop", "desk"))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{wrong:?}");
            let want = format!("a move archives the session on laptop once desk has it: confirm it with confirm_archive set to {laptop_id}");
            assert_eq!(v["error"].as_str(), Some(want.as_str()));
        }
        // The destination has not allowed archive, and is not asked to: but as the source it is refused.
        let (status, v) = send(&app, "POST", "/hub/v1/plans", Some(&token), Body::from(ask(Some(&ids["desk"]), "desk", "laptop"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(v["error"].as_str().unwrap().contains("desk does not allow archive commands"), "{v}");
        let (_, v) = send(&app, "GET", "/hub/v1/plans", Some(&token), Body::empty()).await;
        assert_eq!(v.as_array().map(Vec::len), Some(0), "refusals leave nothing behind");

        let (status, plan) = send(&app, "POST", "/hub/v1/plans", Some(&token), Body::from(ask(Some(&laptop_id), "laptop", "desk"))).await;
        assert_eq!(status, StatusCode::CREATED, "{plan}");
        assert_eq!((plan["kind"].as_str(), plan["state"].as_str()), (Some("move"), Some("queued")));
        let steps = plan["steps"].as_array().unwrap();
        assert_eq!(steps.iter().map(|s| s["op"].as_str().unwrap()).collect::<Vec<_>>(), ["push", "pull", "archive"]);
        assert_eq!(steps.iter().map(|s| s["state"].as_str().unwrap()).collect::<Vec<_>>(), ["queued", "pending", "pending"]);
        assert_eq!((steps[0]["args"]["exact"].as_bool(), steps[2]["machine"]["name"].as_str()), (Some(true), Some("laptop")));
        let (push, pull, archive) = (steps[0]["id"].as_str().unwrap(), steps[1]["id"].as_str().unwrap(), steps[2]["id"].as_str().unwrap());
        assert_eq!(steps[2]["needs"].as_str(), Some(pull));
        let plan_id = plan["id"].as_str().unwrap().to_string();
        let (status, _) = send(&app, "POST", "/hub/v1/plans", Some(&admin), Body::from(ask(Some(&laptop_id), "laptop", "desk"))).await;
        assert_eq!(status, StatusCode::CONFLICT, "one command per session");

        // Push really uploads a copy.
        assert_eq!(send(&app, "POST", &format!("/hub/v1/inbox/{push}/claim"), Some(&alpha), Body::empty()).await.0, StatusCode::OK);
        let sha = asm_core::fsutil::sha256_hex(b"line\n");
        send(&app, "PUT", &format!("/hub/v1/blobs/{sha}"), Some(&alpha), Body::from("line\n")).await;
        let manifest = json!({
            "schema": 1, "agent": "claude-code", "id": id, "project_root": "/x", "project_root_portable": "/x",
            "canonical": "c", "parent_rev": null, "files": [{ "path": "transcript.jsonl", "sha256": sha, "size": 5 }]
        });
        let (_, v) = send(&app, "PUT", &format!("/hub/v1/sessions/claude-code/{id}"), Some(&alpha), Body::from(manifest.to_string())).await;
        let rev = v["rev"].as_str().unwrap().to_string();
        let result = |code: &str, rev: Option<&str>| json!({ "code": code, "rev": rev }).to_string();
        assert_eq!(send(&app, "POST", &format!("/hub/v1/inbox/{push}/result"), Some(&alpha), Body::from(result("ok", Some(&rev)))).await.0, StatusCode::OK);

        // Only the pull is offered, to the destination; the archive is not offered to anyone, nor claimable.
        let (_, v) = send(&app, "GET", inbox_a, Some(&alpha), Body::empty()).await;
        assert_eq!(v.as_array().map(Vec::len), Some(0), "the archive waits for the pull: {v}");
        assert_eq!(send(&app, "POST", &format!("/hub/v1/inbox/{archive}/claim"), Some(&alpha), Body::empty()).await.0, StatusCode::CONFLICT);
        let (_, v) = send(&app, "GET", inbox_b, Some(&beta), Body::empty()).await;
        assert_eq!((v[0]["op"].as_str(), v[0]["rev"].as_str()), (Some("pull"), Some(rev.as_str())));
        assert_eq!(send(&app, "POST", &format!("/hub/v1/inbox/{pull}/claim"), Some(&beta), Body::empty()).await.0, StatusCode::OK);
        let (_, v) = send(&app, "GET", &format!("/hub/v1/plans/{plan_id}"), Some(&token), Body::empty()).await;
        assert_eq!(v["state"].as_str(), Some("running"));
        assert_eq!(send(&app, "POST", &format!("/hub/v1/inbox/{pull}/result"), Some(&beta), Body::from(result("ok", None))).await.0, StatusCode::OK);

        // Now the source is asked to archive, pinned to the revision that was sent; the destination is not.
        let (_, v) = send(&app, "GET", inbox_b, Some(&beta), Body::empty()).await;
        assert_eq!(v.as_array().map(Vec::len), Some(0));
        let (_, v) = send(&app, "GET", inbox_a, Some(&alpha), Body::empty()).await;
        assert_eq!((v[0]["id"].as_str(), v[0]["op"].as_str(), v[0]["rev"].as_str()), (Some(archive), Some("archive"), Some(rev.as_str())));
        assert_eq!(send(&app, "POST", &format!("/hub/v1/inbox/{archive}/claim"), Some(&beta), Body::empty()).await.0, StatusCode::NOT_FOUND);
        assert_eq!(send(&app, "POST", &format!("/hub/v1/inbox/{archive}/claim"), Some(&alpha), Body::empty()).await.0, StatusCode::OK);
        // The source refuses: the session changed since. The plan says where it stopped, and a retry queues it again.
        let refusal = json!({ "code": "changed_since_move", "detail": "the session changed on laptop after the copy that was sent; nothing was archived" }).to_string();
        let (status, v) = send(&app, "POST", &format!("/hub/v1/inbox/{archive}/result"), Some(&alpha), Body::from(refusal)).await;
        assert_eq!((status, v["state"].as_str(), v["code"].as_str()), (StatusCode::OK, Some("blocked"), Some("changed_since_move")));
        let (_, v) = send(&app, "GET", &format!("/hub/v1/plans/{plan_id}"), Some(&admin), Body::empty()).await;
        assert_eq!((v["state"].as_str(), v["steps"][0]["state"].as_str(), v["steps"][1]["state"].as_str()), (Some("blocked"), Some("ok"), Some("ok")));
        let (status, v) = send(&app, "POST", &format!("/hub/v1/plans/{plan_id}/retry"), Some(&token), Body::empty()).await;
        assert_eq!((status, v["state"].as_str(), v["steps"][2]["state"].as_str(), v["steps"][2]["rev"].as_str()), (StatusCode::OK, Some("queued"), Some("queued"), Some(rev.as_str())));
        assert_eq!(send(&app, "POST", &format!("/hub/v1/inbox/{archive}/claim"), Some(&alpha), Body::empty()).await.0, StatusCode::OK);
        let (status, _) = send(&app, "POST", &format!("/hub/v1/inbox/{archive}/result"), Some(&alpha), Body::from(result("ok", None))).await;
        assert_eq!(status, StatusCode::OK);
        let (_, v) = send(&app, "GET", &format!("/hub/v1/plans/{plan_id}"), Some(&token), Body::empty()).await;
        assert_eq!((v["state"].as_str(), v["kind"].as_str()), (Some("ok"), Some("move")));
        let (_, v) = send(&app, "GET", "/hub/v1/commands", Some(&token), Body::empty()).await;
        assert_eq!(v.as_array().map(Vec::len), Some(3), "plan steps are commands too");

        // Archive cannot be created as a command of its own.
        let alone = json!({ "op": "archive", "machine": "laptop", "agent": "claude-code", "session": "other" }).to_string();
        let (status, v) = send(&app, "POST", "/hub/v1/commands", Some(&admin), Body::from(alone)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(v["error"].as_str().unwrap().contains("a step of a move"), "{v}");
    }

    #[tokio::test]
    async fn a_wrong_join_token_is_refused_and_a_right_one_works() {
        let (_d, state, app) = app();
        let body = json!({ "token": "asmj_wrong", "name": "x" }).to_string();
        let (status, _) = send(&app, "POST", "/hub/v1/join", None, Body::from(body)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(state.hub.machines().unwrap().is_empty());

        // Nothing near the 64 MiB the authenticated routes take.
        let padded = json!({ "token": "x".repeat(8192), "name": "x" }).to_string();
        let (status, _) = send(&app, "POST", "/hub/v1/join", None, Body::from(padded)).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

        let cred = credential(&app, &state).await;
        let (status, v) = send(&app, "GET", "/hub/v1/machines", Some(&cred), Body::empty()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v[0]["name"], "laptop");
        assert!(v[0].get("credential_sha256").is_none(), "hashes never leave the hub");

        // Unknown routes still answer 404 to a machine that is allowed in.
        let (status, _) = send(&app, "GET", "/no/such/route", Some(&cred), Body::empty()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn uploads_are_verified_capped_and_leave_nothing_behind() {
        let (_d, state, app) = app();
        let cred = credential(&app, &state).await;
        let sha = asm_core::fsutil::sha256_hex(b"hello");

        let (status, _) =
            send(&app, "PUT", &format!("/hub/v1/blobs/{sha}"), Some(&cred), Body::from("wrong")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "content must match its name");

        let (status, _) =
            send(&app, "PUT", &format!("/hub/v1/blobs/{sha}"), Some(&cred), Body::from("hello")).await;
        assert_eq!(status, StatusCode::CREATED);
        let response = app
            .clone()
            .oneshot(
                HttpRequest::get(format!("/hub/v1/blobs/{sha}"))
                    .header("authorization", format!("Bearer {cred}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(axum::body::to_bytes(response.into_body(), 64).await.unwrap(), "hello");

        // Over the cap: refused whether the size is declared up front or
        // only discovered while streaming.
        let big = vec![b'x'; 5000];
        let big_sha = asm_core::fsutil::sha256_hex(&big);
        let (status, _) = send(
            &app,
            "PUT",
            &format!("/hub/v1/blobs/{big_sha}"),
            Some(&cred),
            Body::from(big.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        let chunks: Vec<Result<Bytes, std::io::Error>> =
            big.chunks(1000).map(|c| Ok(Bytes::copy_from_slice(c))).collect();
        let streamed = Body::from_stream(tokio_stream::iter(chunks));
        let (status, _) =
            send(&app, "PUT", &format!("/hub/v1/blobs/{big_sha}"), Some(&cred), streamed).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

        let tmp = state.hub.root().join("tmp");
        assert_eq!(std::fs::read_dir(tmp).unwrap().count(), 0, "no partial uploads left");
        let (status, v) = send(
            &app,
            "POST",
            "/hub/v1/missing",
            Some(&cred),
            Body::from(json!({ "shas": [sha, big_sha] }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["missing"], json!([big_sha]));
    }

    #[tokio::test]
    async fn revisions_answer_with_the_codes_a_client_branches_on() {
        let (_d, state, app) = app();
        let cred = credential(&app, &state).await;
        let id = "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43";
        let sha = asm_core::fsutil::sha256_hex(b"line\n");
        let manifest = |parent: Option<&str>| {
            json!({
                "schema": 1, "agent": "claude-code", "id": id,
                "project_root": "/x", "project_root_portable": "/x", "canonical": "c",
                "parent_rev": parent,
                "files": [{ "path": "transcript.jsonl", "sha256": sha, "size": 5 }]
            })
            .to_string()
        };
        let uri = format!("/hub/v1/sessions/claude-code/{id}");

        let (status, v) = send(&app, "PUT", &uri, Some(&cred), Body::from(manifest(None))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(v["missing"], json!([sha]));

        send(&app, "PUT", &format!("/hub/v1/blobs/{sha}"), Some(&cred), Body::from("line\n")).await;
        let (status, v) = send(&app, "PUT", &uri, Some(&cred), Body::from(manifest(None))).await;
        assert_eq!(status, StatusCode::CREATED);
        let rev = v["rev"].as_str().unwrap().to_string();

        let (status, v) = send(&app, "PUT", &uri, Some(&cred), Body::from(manifest(None))).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(v["head"], rev);

        let (status, v) = send(&app, "GET", &uri, Some(&cred), Body::empty()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["manifest"]["machine"]["name"], "laptop");

        // One revision, files and all, for a pull to compare against.
        let (status, v) = send(&app, "GET", &format!("{uri}/revisions/{rev}"), Some(&cred), Body::empty()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(v["files"].as_array().is_some_and(|f| !f.is_empty()));
        let unknown = format!("{uri}/revisions/{}", "0".repeat(64));
        assert_eq!(send(&app, "GET", &unknown, Some(&cred), Body::empty()).await.0, StatusCode::NOT_FOUND);
        assert_eq!(send(&app, "GET", &format!("{uri}/revisions/{rev}"), None, Body::empty()).await.0, StatusCode::UNAUTHORIZED);

        for bad in ["/hub/v1/sessions/claude-code/..", "/hub/v1/sessions/nope/x"] {
            let (status, _) = send(&app, "GET", bad, Some(&cred), Body::empty()).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{bad}");
        }
    }
}
