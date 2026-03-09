use std::path::Path;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    extract::Request,
    http::{HeaderValue, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;

use crate::routes;
use crate::state::AppState;

pub(crate) fn build_app(state: AppState) -> Router {
    let api_body_limit = crate::config::api_body_limit_bytes();
    let claims_static_assets = ServeDir::new("claims-client/dist")
        .precompressed_br()
        .precompressed_gzip();
    let main_static_assets = ServeDir::new("client/dist")
        .precompressed_br()
        .precompressed_gzip();
    let static_assets = Router::new()
        .nest_service("/claims-app", claims_static_assets)
        .fallback_service(main_static_assets)
        .layer(middleware::from_fn(set_static_cache_control));

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
        .route("/claims", axum::routing::get(serve_claims_route))
        .route("/claims/{*path}", axum::routing::get(serve_claims_route));

    app.layer(CompressionLayer::new())
        .layer(DefaultBodyLimit::max(api_body_limit))
        .fallback_service(static_assets)
        .with_state(state)
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
    if is_hashed_bundle_asset(path) {
        return Some("public, max-age=31536000, immutable");
    }

    if path.starts_with("/tiles/") || path.starts_with("/fonts/") || path.starts_with("/icons/") {
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

async fn serve_html_file(path: String) -> Response {
    match tokio::fs::read(&path).await {
        Ok(body) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            body,
        )
            .into_response(),
        Err(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn is_claims_editor_path(path: &str) -> bool {
    matches!(path, "/claims/new" | "/claims/new/")
        || path.starts_with("/claims/new/")
        || matches!(path, "/claims/s" | "/claims/s/")
        || path.starts_with("/claims/s/")
}

fn claims_launcher_path() -> String {
    let manifest_root = env!("CARGO_MANIFEST_DIR");
    let candidates = [
        "server/static/claims-launcher.html".to_string(),
        format!("{manifest_root}/static/claims-launcher.html"),
        format!("{manifest_root}/../server/static/claims-launcher.html"),
    ];

    candidates
        .into_iter()
        .find(|path| Path::new(path).exists())
        .unwrap_or_else(|| format!("{manifest_root}/static/claims-launcher.html"))
}

fn claims_editor_index_path() -> String {
    let manifest_root = env!("CARGO_MANIFEST_DIR");
    let candidates = [
        "claims-client/dist/index.html".to_string(),
        "claims-client/index.html".to_string(),
        format!("{manifest_root}/../claims-client/dist/index.html"),
        format!("{manifest_root}/../claims-client/index.html"),
    ];

    candidates
        .into_iter()
        .find(|path| Path::new(path).exists())
        .unwrap_or_else(|| format!("{manifest_root}/../claims-client/index.html"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
