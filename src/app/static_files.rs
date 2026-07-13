use crate::app::paths::AppPaths;
use crate::app::state::AppState;
use axum::{
    Router,
    body::Body,
    extract::Request,
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
};
use std::path::PathBuf;
use tower_http::services::{ServeDir, ServeFile};

const HTML_CACHE_CONTROL: &str = "no-cache, must-revalidate";
const STYLE_CACHE_CONTROL: &str = "public, max-age=86400, stale-while-revalidate=604800";

pub fn add_static_routes(app: Router<AppState>, paths: &AppPaths) -> Router<AppState> {
    let react_app_dir = paths.static_dir.join("react-app");
    let react_assets_dir = react_app_dir.join("assets");
    let react_index = react_app_dir.join("index.html");
    let react_index_for_root = react_index.clone();
    let react_index_for_index = react_index.clone();
    let styles_css = paths.static_dir.join("styles.css");
    let static_service =
        ServeDir::new(paths.static_dir.clone()).fallback(ServeFile::new(react_index));

    app.route(
        "/",
        get(move || {
            serve_file(
                react_index_for_root.clone(),
                "text/html; charset=utf-8",
                HTML_CACHE_CONTROL,
            )
        }),
    )
    .route(
        "/index.html",
        get(move || {
            serve_file(
                react_index_for_index.clone(),
                "text/html; charset=utf-8",
                HTML_CACHE_CONTROL,
            )
        }),
    )
    .route(
        "/styles.css",
        get(move || {
            serve_file(
                styles_css.clone(),
                "text/css; charset=utf-8",
                STYLE_CACHE_CONTROL,
            )
        }),
    )
    .nest_service(
        "/user-fonts",
        ServeDir::new(paths.appearance_assets_dir.clone()),
    )
    .nest_service(
        "/profile-images",
        ServeDir::new(
            paths
                .settings_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("profile-images"),
        ),
    )
    .nest_service("/react-app/assets", ServeDir::new(react_assets_dir))
    .fallback_service(static_service)
}

pub async fn cache_response_headers(request: Request<Body>, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let mut response = next.run(request).await;
    if !response.status().is_success() || response.headers().contains_key(header::CACHE_CONTROL) {
        return response;
    }

    let cache_control =
        if path.starts_with("/api/") || path.starts_with("/sonos/") || path.starts_with("/upnp/") {
            "no-store"
        } else if path.starts_with("/react-app/assets/") || path.starts_with("/fonts/") {
            "public, max-age=31536000, immutable"
        } else if path == "/"
            || path.ends_with(".html")
            || !path.rsplit('/').next().unwrap_or("").contains('.')
        {
            HTML_CACHE_CONTROL
        } else if path == "/styles.css" {
            STYLE_CACHE_CONTROL
        } else {
            "public, max-age=3600"
        };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    response
}

async fn serve_file(
    path: PathBuf,
    content_type: &'static str,
    cache_control: &'static str,
) -> Response {
    match tokio::fs::read(path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, cache_control),
            ],
            bytes,
        )
            .into_response(),
        Err(err) => (StatusCode::NOT_FOUND, err.to_string()).into_response(),
    }
}
