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
        .layer(axum::middleware::from_fn(guard_mutations))
}

#[derive(Deserialize)]
struct SessionsQuery {
    #[serde(default)]
    all: bool,
}

async fn sessions(Query(q): Query<SessionsQuery>) -> ApiResult<Vec<Session>> {
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

async fn index_stats() -> ApiResult<Value> {
    blocking(|| {
        let index = asm_core::index::Index::open().map_err(internal)?;
        serde_json::to_value(index.stats().map_err(internal)?).map_err(internal)
    })
    .await
    .map(Json)
}

async fn index_refresh() -> ApiResult<Value> {
    blocking(|| {
        let mut index = asm_core::index::Index::open().map_err(internal)?;
        let report = index.refresh(|_| {}).map_err(internal)?;
        serde_json::to_value(&report).map_err(internal)
    })
    .await
    .map(Json)
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
