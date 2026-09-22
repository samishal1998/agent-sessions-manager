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

use asm_core::hub::store::{Hub, HubError, Machine};

pub struct HubState {
    pub hub: Hub,
    /// Largest single file the hub accepts.
    pub max_blob: u64,
}

type Shared = Arc<HubState>;

fn error(status: StatusCode, message: impl ToString) -> Response {
    (status, Json(json!({ "error": message.to_string() }))).into_response()
}

fn hub_error(e: HubError) -> Response {
    match e {
        HubError::Unauthorized => error(StatusCode::UNAUTHORIZED, e),
        HubError::BadRequest(_) | HubError::ShaMismatch => error(StatusCode::BAD_REQUEST, e),
        HubError::NotFound => error(StatusCode::NOT_FOUND, e),
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

async fn authenticate(State(state): State<Shared>, mut request: Request, next: Next) -> Response {
    if request.method() == Method::POST && request.uri().path() == "/hub/v1/join" {
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
        .route("/hub/v1/missing", post(missing))
        .route("/hub/v1/blobs/{sha}", get(get_blob).put(put_blob))
        .fallback(|| async { error(StatusCode::NOT_FOUND, "not found") })
        // Manifests of large sessions list thousands of files. Blob bodies
        // are read as raw `Body`, which this limit does not govern; they
        // have their own cap, below.
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(axum::middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state)
}

#[derive(Deserialize)]
struct JoinBody {
    token: String,
    name: String,
}

async fn join(State(state): State<Shared>, Json(body): Json<JoinBody>) -> Response {
    match blocking(move || state.hub.join(&body.token, &body.name)).await {
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
            ("GET", "/no/such/route"),
            ("GET", "/api/sessions"),
        ] {
            for cred in [None, Some("asmc_garbage")] {
                let (status, _) = send(&app, method, uri, cred, Body::empty()).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri} with {cred:?}");
            }
        }
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

        for bad in ["/hub/v1/sessions/claude-code/..", "/hub/v1/sessions/nope/x"] {
            let (status, _) = send(&app, "GET", bad, Some(&cred), Body::empty()).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{bad}");
        }
    }
}
