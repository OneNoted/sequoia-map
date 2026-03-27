use std::path::Path;
use std::sync::OnceLock;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    extract::Request,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
};
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;

use crate::routes;
use crate::state::AppState;

const X_ROBOTS_TAG: &str = "x-robots-tag";
const CANONICAL_URL_TOKEN: &str = "__SEQUOIA_CANONICAL_URL__";
const OG_IMAGE_URL_TOKEN: &str = "__SEQUOIA_OG_IMAGE_URL__";
const DEFAULT_OG_IMAGE_PATH: &str = "/tiles/main-3-2.webp";

#[derive(Clone, Debug, Default)]
struct HtmlResponseOptions {
    canonical: Option<String>,
    robots: Option<&'static str>,
}

pub(crate) fn build_app(state: AppState) -> Router {
    let api_body_limit = crate::config::api_body_limit_bytes();
    let static_assets = Router::new()
        .nest("/claims-app", static_assets_router("claims-client/dist"))
        .merge(static_assets_router("client/dist"));

    let app = Router::new()
        .route("/", axum::routing::get(serve_map_root))
        .route("/index.html", axum::routing::get(redirect_index_to_root))
        .route("/history", axum::routing::get(serve_map_history))
        .route("/history/{*path}", axum::routing::get(serve_map_history))
        .route("/robots.txt", axum::routing::get(serve_robots_txt))
        .route("/sitemap.xml", axum::routing::get(serve_sitemap_xml))
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
            "/api/season/windows",
            axum::routing::get(routes::api::get_season_windows),
        )
        .route(
            "/api/season/series",
            axum::routing::get(routes::api::get_season_series),
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

    Router::new()
        .fallback_service(
            ServeDir::new(dist_dir)
                .precompressed_br()
                .precompressed_gzip(),
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

async fn redirect_index_to_root() -> Redirect {
    Redirect::permanent("/")
}

async fn serve_map_root() -> impl IntoResponse {
    serve_html_file(
        map_index_path(),
        HtmlResponseOptions {
            canonical: Some(map_root_url()),
            robots: None,
        },
    )
    .await
}

async fn serve_map_history() -> impl IntoResponse {
    serve_html_file(
        map_index_path(),
        HtmlResponseOptions {
            canonical: Some(map_root_url()),
            robots: Some("noindex, follow"),
        },
    )
    .await
}

async fn serve_claims_route(request: Request) -> impl IntoResponse {
    let path = request.uri().path().to_owned();
    let html_path = if is_claims_editor_path(&path) {
        claims_editor_index_path()
    } else {
        claims_launcher_path()
    };
    serve_html_file(
        html_path,
        HtmlResponseOptions {
            canonical: None,
            robots: Some("noindex, follow"),
        },
    )
    .await
}

async fn serve_html_file(path: &str, options: HtmlResponseOptions) -> Response {
    match tokio::fs::read(path).await {
        Ok(body) => {
            let body = apply_html_body_substitutions(body, &options);
            let mut response = (
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                body,
            )
                .into_response();
            apply_html_response_options(response.headers_mut(), &options);
            response
        }
        Err(err) => {
            tracing::error!(path = %path, error = %err, "failed to read HTML file");
            axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn apply_html_body_substitutions(body: Vec<u8>, options: &HtmlResponseOptions) -> Vec<u8> {
    let mut html = match String::from_utf8(body) {
        Ok(html) => html,
        Err(err) => return err.into_bytes(),
    };

    if let Some(canonical) = options.canonical.as_deref() {
        html = html.replace(CANONICAL_URL_TOKEN, canonical);
        html = html.replace(
            OG_IMAGE_URL_TOKEN,
            &format!(
                "{}{}",
                crate::config::map_public_base_url(),
                DEFAULT_OG_IMAGE_PATH
            ),
        );
    }

    html.into_bytes()
}

fn apply_html_response_options(headers: &mut HeaderMap, options: &HtmlResponseOptions) {
    if let Some(robots) = options.robots {
        headers.insert(
            HeaderName::from_static(X_ROBOTS_TAG),
            HeaderValue::from_static(robots),
        );
    }

    if let Some(canonical) = options.canonical.as_deref()
        && let Ok(value) = HeaderValue::from_str(&format!("<{canonical}>; rel=\"canonical\""))
    {
        headers.insert(header::LINK, value);
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

fn map_index_path() -> &'static str {
    static MAP_INDEX_PATH: OnceLock<String> = OnceLock::new();

    let manifest_root = env!("CARGO_MANIFEST_DIR");
    MAP_INDEX_PATH
        .get_or_init(|| {
            resolve_first_existing_path(
                [
                    "client/dist/index.html".to_string(),
                    "client/index.html".to_string(),
                    format!("{manifest_root}/../client/dist/index.html"),
                    format!("{manifest_root}/../client/index.html"),
                ],
                format!("{manifest_root}/../client/index.html"),
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

fn map_root_url() -> String {
    format!("{}/", crate::config::map_public_base_url())
}

async fn serve_robots_txt() -> impl IntoResponse {
    let base_url = crate::config::map_public_base_url();
    let body =
        format!("User-agent: *\nAllow: /\nDisallow: /api/\nSitemap: {base_url}/sitemap.xml\n");

    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        body,
    )
}

async fn serve_sitemap_xml() -> impl IntoResponse {
    let root_url = map_root_url();
    let body = format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
            "  <url>\n",
            "    <loc>{root_url}</loc>\n",
            "  </url>\n",
            "</urlset>\n"
        ),
        root_url = root_url
    );

    (
        [
            (header::CONTENT_TYPE, "application/xml; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use axum::body::{self, Body};
    use axum::http::Request;
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
    fn map_index_path_falls_back_to_source_html() {
        let index_path = map_index_path();
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
    async fn root_route_serves_seo_metadata_and_semantic_shell() {
        let app = build_app(AppState::new(None));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("build root request"),
            )
            .await
            .expect("root request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::LINK)
                .and_then(|value| value.to_str().ok()),
            Some("<https://map.example.com/>; rel=\"canonical\"")
        );
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let body = String::from_utf8(body.to_vec()).expect("utf8 body");
        assert!(body.contains("Sequoia Map | Live Wynncraft Territory Map"));
        assert!(body.contains("name=\"description\""));
        assert!(body.contains("rel=\"canonical\" href=\"https://map.example.com/\""));
        assert!(body.contains("property=\"og:url\" content=\"https://map.example.com/\""));
        assert!(body.contains(
            "property=\"og:image\" content=\"https://map.example.com/tiles/main-3-2.webp\""
        ));
        assert!(body.contains("application/ld+json"));
        assert!(body.contains("Sequoia Map: Live Wynncraft Territory Map"));
        assert!(body.contains("id=\"app\""));
    }

    #[tokio::test]
    async fn history_route_serves_map_shell_with_noindex() {
        let app = build_app(AppState::new(None));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/history")
                    .body(Body::empty())
                    .expect("build history request"),
            )
            .await
            .expect("history request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(HeaderName::from_static(X_ROBOTS_TAG))
                .and_then(|value| value.to_str().ok()),
            Some("noindex, follow")
        );
        assert_eq!(
            response
                .headers()
                .get(header::LINK)
                .and_then(|value| value.to_str().ok()),
            Some("<https://map.example.com/>; rel=\"canonical\"")
        );
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let body = String::from_utf8(body.to_vec()).expect("utf8 body");
        assert!(body.contains("Sequoia Map | Live Wynncraft Territory Map"));
    }

    #[tokio::test]
    async fn claims_routes_are_marked_noindex() {
        let app = build_app(AppState::new(None));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/claims")
                    .body(Body::empty())
                    .expect("build claims request"),
            )
            .await
            .expect("claims request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(HeaderName::from_static(X_ROBOTS_TAG))
                .and_then(|value| value.to_str().ok()),
            Some("noindex, follow")
        );
    }

    #[tokio::test]
    async fn robots_and_sitemap_routes_expose_canonical_root() {
        let app = build_app(AppState::new(None));

        let robots = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/robots.txt")
                    .body(Body::empty())
                    .expect("build robots request"),
            )
            .await
            .expect("robots request should succeed");
        assert_eq!(robots.status(), StatusCode::OK);
        let robots_body = body::to_bytes(robots.into_body(), usize::MAX)
            .await
            .expect("read robots body");
        let robots_body = String::from_utf8(robots_body.to_vec()).expect("utf8 robots body");
        assert!(robots_body.contains("Disallow: /api/"));
        assert!(robots_body.contains("Sitemap: https://map.example.com/sitemap.xml"));

        let sitemap = app
            .oneshot(
                Request::builder()
                    .uri("/sitemap.xml")
                    .body(Body::empty())
                    .expect("build sitemap request"),
            )
            .await
            .expect("sitemap request should succeed");
        assert_eq!(sitemap.status(), StatusCode::OK);
        let sitemap_body = body::to_bytes(sitemap.into_body(), usize::MAX)
            .await
            .expect("read sitemap body");
        let sitemap_body = String::from_utf8(sitemap_body.to_vec()).expect("utf8 sitemap body");
        assert!(sitemap_body.contains("<loc>https://map.example.com/</loc>"));
    }

    #[tokio::test]
    async fn unknown_api_route_returns_404_without_html_shell() {
        let app = Router::new()
            .route("/api/{*path}", axum::routing::any(api_not_found))
            .merge(static_assets_router(std::env::temp_dir()));

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
}
