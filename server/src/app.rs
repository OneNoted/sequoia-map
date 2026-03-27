use std::path::Path;
use std::sync::OnceLock;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    extract::Request,
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use tower_http::compression::CompressionLayer;
use tower_http::services::{ServeDir, ServeFile};

use crate::routes;
use crate::state::AppState;

pub(crate) fn build_app(state: AppState) -> Router {
    let api_body_limit = crate::config::api_body_limit_bytes();
    let static_assets = Router::new()
        .nest("/claims-app", static_assets_router("claims-client/dist"))
        .merge(static_assets_router("client/dist"));

    let app = Router::new()
        .route(
            "/api/territories",
            axum::routing::get(routes::api::get_territories),
        )
        .route(
            "/api/live/state",
            axum::routing::get(routes::api::get_live_state),
        )
        .route(
            "/api/guild/{name}",
            axum::routing::get(routes::api::get_guild),
        )
        .route(
            "/api/guilds/online",
            axum::routing::get(routes::api::get_guilds_online),
        )
        .route(
            "/api/guilds/catalog",
            axum::routing::get(routes::claims::get_guild_catalog),
        )
        .route(
            "/api/claims/bootstrap/geometry",
            axum::routing::get(routes::claims::get_claims_bootstrap_geometry),
        )
        .route(
            "/api/season/scalar/current",
            axum::routing::get(routes::api::get_season_scalar_current),
        )
        .route(
            "/api/season/race",
            axum::routing::get(routes::api::get_season_race),
        )
        .route(
            "/api/claims",
            axum::routing::post(routes::claims::create_claim_layout),
        )
        .route(
            "/api/claims/{id}",
            axum::routing::get(routes::claims::get_claim_layout),
        )
        .route(
            "/api/wars/live",
            axum::routing::get(routes::ingest::get_live_wars),
        )
        .route(
            "/api/events",
            axum::routing::get(routes::sse::territory_events),
        )
        .route(
            "/api/internal/ingest/territory",
            axum::routing::post(routes::ingest::ingest_territory),
        )
        .route(
            "/api/internal/ingest/heartbeat",
            axum::routing::post(routes::ingest::heartbeat),
        )
        .route("/api/health", axum::routing::get(routes::api::health))
        .route("/api/metrics", axum::routing::get(routes::api::metrics))
        .route(
            "/api/history/at",
            axum::routing::get(routes::history::history_at),
        )
        .route(
            "/api/history/events",
            axum::routing::get(routes::history::history_events),
        )
        .route(
            "/api/history/sr-samples",
            axum::routing::get(routes::history::history_sr_samples),
        )
        .route(
            "/api/history/bounds",
            axum::routing::get(routes::history::history_bounds),
        )
        .route(
            "/api/history/heat/meta",
            axum::routing::get(routes::history::history_heat_meta),
        )
        .route(
            "/api/history/heat",
            axum::routing::get(routes::history::history_heat),
        )
        .route("/api", axum::routing::any(api_not_found))
        .route("/api/{*path}", axum::routing::any(api_not_found))
        .route("/claims", axum::routing::get(serve_claims_route))
        .route("/claims/{*path}", axum::routing::get(serve_claims_route));

    app.layer(CompressionLayer::new())
        .layer(DefaultBodyLimit::max(api_body_limit))
        .fallback_service(static_assets)
        .with_state(state)
}

fn static_assets_router(dist_dir: impl AsRef<Path>) -> Router {
    let dist_dir = dist_dir.as_ref().to_path_buf();
    let index_path = dist_dir.join("index.html");

    Router::new()
        .fallback_service(
            ServeDir::new(dist_dir)
                .precompressed_br()
                .precompressed_gzip()
                .fallback(ServeFile::new(index_path)),
        )
        .layer(middleware::from_fn(set_static_cache_control))
}

async fn api_not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "Not Found")
}

async fn set_static_cache_control(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let mut response = next.run(request).await;

    if response.status().is_success()
        && let Some(cache_control) = cache_control_for_path(&path)
    {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(cache_control),
        );
    }

    response
}

fn cache_control_for_path(path: &str) -> Option<&'static str> {
    let normalized_path = path.strip_prefix("/claims-app").unwrap_or(path);

    if is_hashed_bundle_asset(normalized_path) {
        return Some("public, max-age=31536000, immutable");
    }

    if normalized_path.starts_with("/tiles/")
        || normalized_path.starts_with("/fonts/")
        || normalized_path.starts_with("/icons/")
    {
        return Some("public, max-age=86400");
    }

    None
}

fn is_hashed_bundle_asset(path: &str) -> bool {
    let Some(ext) = Path::new(path).extension().and_then(|ext| ext.to_str()) else {
        return false;
    };

    if !matches!(ext, "wasm" | "js" | "css") {
        return false;
    }

    let Some(filename) = Path::new(path).file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    filename
        .split(['-', '_', '.'])
        .any(|segment| segment.len() >= 8 && segment.chars().all(|c| c.is_ascii_hexdigit()))
}

async fn serve_claims_route(request: Request) -> impl IntoResponse {
    let path = request.uri().path().to_owned();
    let html_path = if is_claims_editor_path(&path) {
        claims_editor_index_path()
    } else {
        claims_launcher_path()
    };
    serve_html_file(html_path).await
}

async fn serve_html_file(path: &str) -> Response {
    match tokio::fs::read(path).await {
        Ok(body) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            body,
        )
            .into_response(),
        Err(err) => {
            tracing::error!(path = %path, error = %err, "failed to read HTML file");
            axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn is_claims_editor_path(path: &str) -> bool {
    matches!(path, "/claims/new" | "/claims/new/")
        || path.starts_with("/claims/new/")
        || matches!(path, "/claims/s" | "/claims/s/")
        || path.starts_with("/claims/s/")
}

fn resolve_first_existing_path<I>(candidates: I, fallback: String) -> String
where
    I: IntoIterator<Item = String>,
{
    candidates
        .into_iter()
        .find(|path| Path::new(path).exists())
        .unwrap_or(fallback)
}

fn claims_launcher_path() -> &'static str {
    static CLAIMS_LAUNCHER_HTML_PATH: OnceLock<String> = OnceLock::new();

    let manifest_root = env!("CARGO_MANIFEST_DIR");
    CLAIMS_LAUNCHER_HTML_PATH
        .get_or_init(|| {
            resolve_first_existing_path(
                [
                    "server/static/claims-launcher.html".to_string(),
                    format!("{manifest_root}/static/claims-launcher.html"),
                    format!("{manifest_root}/../server/static/claims-launcher.html"),
                ],
                format!("{manifest_root}/static/claims-launcher.html"),
            )
        })
        .as_str()
}

fn claims_editor_index_path() -> &'static str {
    static CLAIMS_EDITOR_INDEX_PATH: OnceLock<String> = OnceLock::new();

    let manifest_root = env!("CARGO_MANIFEST_DIR");
    CLAIMS_EDITOR_INDEX_PATH
        .get_or_init(|| {
            resolve_first_existing_path(
                [
                    "claims-client/dist/index.html".to_string(),
                    "claims-client/index.html".to_string(),
                    format!("{manifest_root}/../claims-client/dist/index.html"),
                    format!("{manifest_root}/../claims-client/index.html"),
                ],
                format!("{manifest_root}/../claims-client/index.html"),
            )
        })
        .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{self, Body};
    use axum::http::Request;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::util::ServiceExt;

    #[test]
    fn immutable_cache_for_hashed_bundle_assets() {
        assert_eq!(
            cache_control_for_path("/sequoia-client-71578f6b278221f3_bg.wasm"),
            Some("public, max-age=31536000, immutable")
        );
        assert_eq!(
            cache_control_for_path("/input-a93762ff3bf6d63a.css"),
            Some("public, max-age=31536000, immutable")
        );
    }

    #[test]
    fn short_cache_for_unhashed_static_assets() {
        assert_eq!(
            cache_control_for_path("/tiles/tile_0_0.webp"),
            Some("public, max-age=86400")
        );
        assert_eq!(
            cache_control_for_path("/fonts/minecraft-regular.otf"),
            Some("public, max-age=86400")
        );
        assert_eq!(
            cache_control_for_path("/claims-app/icons/crown_icon.png"),
            Some("public, max-age=86400")
        );
        assert_eq!(
            cache_control_for_path("/claims-app/tiles/tile_0_0.webp"),
            Some("public, max-age=86400")
        );
    }

    #[test]
    fn no_cache_header_override_for_html() {
        assert_eq!(cache_control_for_path("/"), None);
        assert_eq!(cache_control_for_path("/index.html"), None);
    }

    #[test]
    fn claims_launcher_path_resolves() {
        let index_path = claims_launcher_path();
        assert!(Path::new(&index_path).exists());
    }

    #[test]
    fn claims_editor_index_path_falls_back_to_source_html() {
        let index_path = claims_editor_index_path();
        assert!(Path::new(&index_path).exists());
    }

    #[test]
    fn claims_editor_paths_are_detected() {
        assert!(is_claims_editor_path("/claims/new/blank"));
        assert!(is_claims_editor_path("/claims/new/import"));
        assert!(is_claims_editor_path("/claims/s/example"));
        assert!(!is_claims_editor_path("/claims"));
        assert!(!is_claims_editor_path("/claims/unknown"));
    }

    #[tokio::test]
    async fn history_route_uses_spa_shell_fallback() {
        let temp_dir = create_temp_dist_dir();
        let app = static_assets_router(&temp_dir);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/history")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("history request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let body = String::from_utf8(body.to_vec()).expect("utf8 body");
        assert!(body.contains("Sequoia Test Shell"));
    }

    #[tokio::test]
    async fn unknown_api_route_returns_404_without_html_shell() {
        let temp_dir = create_temp_dist_dir();
        let app = Router::new()
            .route("/api/{*path}", axum::routing::any(api_not_found))
            .merge(static_assets_router(&temp_dir));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/does-not-exist")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("api request should succeed");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let body = String::from_utf8(body.to_vec()).expect("utf8 body");
        assert!(!content_type.contains("text/html"));
        assert!(!body.contains("<html"));
        assert!(body.contains("Not Found"));
    }

    fn create_temp_dist_dir() -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        let unique = format!(
            "sequoia-map-app-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
        );
        dir.push(unique);
        std::fs::create_dir_all(&dir).expect("create temp dist dir");
        std::fs::write(
            dir.join("index.html"),
            "<!DOCTYPE html><html><body>Sequoia Test Shell</body></html>",
        )
        .expect("write test index");
        dir
    }
}
