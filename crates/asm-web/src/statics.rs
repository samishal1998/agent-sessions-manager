//! Embedded frontend assets (built by Vite into `frontend/dist` before
//! `cargo build`; rust-embed compiles them into the binary).

use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "frontend/dist/"]
struct Assets;

pub async fn serve_static(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path).or_else(|| Assets::get("index.html")) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref().to_string())], content.data).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// One embedded file by its path under `dist/`, if there is one: how the hub
/// serves its admin page without borrowing the web UI's catch-all.
pub fn embedded(path: &str) -> Option<Response> {
    let content = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    Some(([(header::CONTENT_TYPE, mime.as_ref().to_string())], content.data).into_response())
}
