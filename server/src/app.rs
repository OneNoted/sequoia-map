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
const ASSET_VERSION_TOKEN: &str = "__SEQUOIA_ASSET_VERSION__";
const DEFAULT_OG_IMAGE_PATH: &str = "/tiles/main-3-2.webp";
const IMMUTABLE_STATIC_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
/// Served directories whose files are requested with the `?v=` asset version.
const VERSIONED_ASSET_DIRS: [&str; 3] = ["tiles", "fonts", "icons"];
const MAP_DIST_DIR: &str = "client/dist";
const CLAIMS_DIST_DIR: &str = "claims-client/dist";
const SHORT_STATIC_CACHE_CONTROL: &str = "public, max-age=86400";

#[derive(Clone, Debug, Default)]
struct HtmlResponseOptions {
    canonical: Option<String>,
    robots: Option<&'static str>,
}

pub(crate) fn build_app(state: AppState) -> Router {
    let api_body_limit = crate::config::api_body_limit_bytes();
    let static_assets = Router::new()
        .nest("/claims-app", static_assets_router(CLAIMS_DIST_DIR))
        .merge(static_assets_router(MAP_DIST_DIR));
    // Hash the versioned assets before serving rather than on the first page request.
    let _ = public_asset_version();

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
            "/api/map/intel",
            axum::routing::get(routes::api::get_map_intel),
        )
        .route(
            "/api/map/intel/overlay",
            axum::routing::get(routes::api::get_map_intel_overlay),
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
        .route("/api/auth/me", axum::routing::get(routes::auth::me))
        .route("/api/auth/login", axum::routing::get(routes::auth::login))
        .route("/api/auth/logout", axum::routing::get(routes::auth::logout))
        .route("/api/health", axum::routing::get(routes::api::health))
        .route("/api/metrics", axum::routing::get(routes::api::metrics))
        .route(
            "/api/warcontroller",
            axum::routing::get(routes::api::get_warcontroller),
        )
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
    let versioned = has_asset_version_query(request.uri().query());
    let mut response = next.run(request).await;

    if response.status().is_success()
        && let Some(cache_control) = cache_control_for_path(&path, versioned)
    {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(cache_control),
        );
    }

    response
}

fn cache_control_for_path(path: &str, versioned: bool) -> Option<&'static str> {
    let normalized_path = path.strip_prefix("/claims-app").unwrap_or(path);

    if is_hashed_bundle_asset(normalized_path) {
        return Some(IMMUTABLE_STATIC_CACHE_CONTROL);
    }

    if normalized_path.starts_with("/tiles/")
        || normalized_path.starts_with("/fonts/")
        || normalized_path.starts_with("/icons/")
    {
        return Some(if versioned {
            IMMUTABLE_STATIC_CACHE_CONTROL
        } else {
            SHORT_STATIC_CACHE_CONTROL
        });
    }

    None
}

fn has_asset_version_query(query: Option<&str>) -> bool {
    query
        .map(|query| {
            query
                .split('&')
                .filter_map(|pair| pair.split_once('='))
                .any(|(key, value)| key == "v" && !value.trim().is_empty())
        })
        .unwrap_or(false)
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
            let body = apply_html_body_substitutions(body, &options, public_asset_version());
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

fn apply_html_body_substitutions(
    body: Vec<u8>,
    options: &HtmlResponseOptions,
    asset_version: Option<&str>,
) -> Vec<u8> {
    let mut html = match String::from_utf8(body) {
        Ok(html) => html,
        Err(err) => return err.into_bytes(),
    };

    html = html.replace(ASSET_VERSION_TOKEN, asset_version.unwrap_or(""));

    if let Some(canonical) = options.canonical.as_deref() {
        html = html.replace(CANONICAL_URL_TOKEN, canonical);
        html = html.replace(
            OG_IMAGE_URL_TOKEN,
            &format!(
                "{}{}",
                crate::config::map_public_base_url(),
                versioned_asset_path(DEFAULT_OG_IMAGE_PATH, asset_version)
            ),
        );
    }

    html.into_bytes()
}

/// The `?v=` cache key for tiles, fonts and icons: a digest of the files served from those
/// directories, so an unchanged asset keeps its URL, and its year-long browser cache, across
/// deployments. Without built assets (a source checkout), the deployed commit, if any.
fn public_asset_version() -> Option<&'static str> {
    static VERSION: OnceLock<Option<String>> = OnceLock::new();
    VERSION
        .get_or_init(|| {
            asset_content_version(&[Path::new(MAP_DIST_DIR), Path::new(CLAIMS_DIST_DIR)]).or_else(
                || {
                    std::env::var("SOURCE_COMMIT")
                        .ok()
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty())
                },
            )
        })
        .as_deref()
}

/// FNV-1a over source assets under the versioned directories of `dist_dirs`, in path
/// order: each file's path, length and bytes. Generated compression sidecars are ignored;
/// their gzip headers can change with build timestamps without changing the asset.
/// `None` when no assets exist or traversal fails.
fn asset_content_version(dist_dirs: &[&Path]) -> Option<String> {
    let mut files = Vec::new();
    for dist in dist_dirs {
        for dir in VERSIONED_ASSET_DIRS {
            let path = dist.join(dir);
            if let Err(error) = collect_files(&path, &mut files) {
                // A distribution may omit an optional asset directory. Never hash a
                // partially enumerated directory as if it were a complete asset set.
                if error.kind() == std::io::ErrorKind::NotFound && !path.exists() {
                    continue;
                }
                tracing::warn!(path = %path.display(), %error, "cannot enumerate versioned assets");
                return None;
            }
        }
    }
    if files.is_empty() {
        return None;
    }
    files.sort();
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };
    for path in &files {
        let Ok(contents) = std::fs::read(path) else {
            tracing::warn!(path = %path.display(), "unreadable versioned asset");
            return None;
        };
        feed(path.to_string_lossy().as_bytes());
        feed(&(contents.len() as u64).to_le_bytes());
        feed(&contents);
    }
    Some(format!("{hash:016x}"))
}

fn collect_files(dir: &Path, files: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        match entry.file_type()? {
            kind if kind.is_dir() => collect_files(&path, files)?,
            kind if kind.is_file() => {
                if !matches!(
                    path.extension().and_then(|ext| ext.to_str()),
                    Some("br" | "gz")
                ) {
                    files.push(path);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn versioned_asset_path(path: &str, asset_version: Option<&str>) -> String {
    match asset_version {
        Some(asset_version) if !asset_version.trim().is_empty() => {
            format!("{path}?v={asset_version}")
        }
        _ => path.to_string(),
    }
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
            cache_control_for_path("/sequoia-client-71578f6b278221f3_bg.wasm", false),
            Some(IMMUTABLE_STATIC_CACHE_CONTROL)
        );
        assert_eq!(
            cache_control_for_path("/input-a93762ff3bf6d63a.css", false),
            Some(IMMUTABLE_STATIC_CACHE_CONTROL)
        );
    }

    #[test]
    fn short_cache_for_unversioned_static_assets() {
        assert_eq!(
            cache_control_for_path("/tiles/tile_0_0.webp", false),
            Some(SHORT_STATIC_CACHE_CONTROL)
        );
        assert_eq!(
            cache_control_for_path("/fonts/minecraft-regular.otf", false),
            Some(SHORT_STATIC_CACHE_CONTROL)
        );
        assert_eq!(
            cache_control_for_path("/claims-app/icons/crown_icon.webp", false),
            Some(SHORT_STATIC_CACHE_CONTROL)
        );
        assert_eq!(
            cache_control_for_path("/claims-app/tiles/tile_0_0.webp", false),
            Some(SHORT_STATIC_CACHE_CONTROL)
        );
    }

    #[test]
    fn no_cache_header_override_for_html() {
        assert_eq!(cache_control_for_path("/", false), None);
        assert_eq!(cache_control_for_path("/index.html", false), None);
    }

    #[test]
    fn immutable_cache_for_versioned_static_assets() {
        assert_eq!(
            cache_control_for_path("/tiles/main-5-2.webp", true),
            Some(IMMUTABLE_STATIC_CACHE_CONTROL)
        );
        assert_eq!(
            cache_control_for_path("/icons/crown_icon.webp", true),
            Some(IMMUTABLE_STATIC_CACHE_CONTROL)
        );
        assert_eq!(
            cache_control_for_path("/claims-app/fonts/minecraft-regular.otf", true),
            Some(IMMUTABLE_STATIC_CACHE_CONTROL)
        );
    }

    #[test]
    fn asset_version_query_detection_requires_non_empty_v_parameter() {
        assert!(has_asset_version_query(Some("v=b7c99ee31b46")));
        assert!(has_asset_version_query(Some("foo=bar&v=b7c99ee31b46")));
        assert!(!has_asset_version_query(Some("foo=bar")));
        assert!(!has_asset_version_query(Some("v=")));
        assert!(!has_asset_version_query(None));
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

    #[test]
    fn asset_version_follows_the_versioned_files_only() {
        let root =
            std::env::temp_dir().join(format!("sequoia-asset-version-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dist = root.join("dist");
        std::fs::create_dir_all(dist.join("tiles")).expect("create tiles");
        std::fs::create_dir_all(dist.join("fonts")).expect("create fonts");
        std::fs::write(dist.join("tiles/main-1-1.webp"), b"tile").expect("write tile");
        std::fs::write(dist.join("fonts/font.woff2"), b"font").expect("write font");
        let version = || asset_content_version(&[dist.as_path()]);

        let first = version().expect("assets present");
        // A new bundle or page leaves the asset version alone.
        std::fs::write(dist.join("index.html"), b"<html>").expect("write html");
        std::fs::write(dist.join("sequoia-client-0123456789abcdef.js"), b"js").expect("write js");
        assert_eq!(version().as_deref(), Some(first.as_str()));
        // Encoded variants are generated at build time; gzip timestamps must not bust
        // the canonical asset URL when its decoded contents have not changed.
        let encoded = dist.join("fonts/font.woff2.gz");
        std::fs::write(&encoded, b"encoded-at-build-one").expect("write encoding");
        assert_eq!(version().as_deref(), Some(first.as_str()));
        std::fs::write(&encoded, b"encoded-at-build-two").expect("rewrite encoding");
        assert_eq!(version().as_deref(), Some(first.as_str()));
        // Any changed or added asset changes it.
        std::fs::write(dist.join("tiles/main-1-1.webp"), b"tile2").expect("rewrite tile");
        let changed = version().expect("assets present");
        assert_ne!(changed, first);
        std::fs::write(dist.join("fonts/other.woff2"), b"font").expect("write font");
        assert_ne!(version().expect("assets present"), changed);

        assert_eq!(
            asset_content_version(&[root.join("missing").as_path()]),
            None
        );
        // An existing but invalid asset directory must not produce a partial digest.
        std::fs::write(dist.join("icons"), b"not a directory").expect("write invalid icons");
        assert_eq!(version(), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn versioned_asset_path_appends_query_when_version_present() {
        assert_eq!(
            versioned_asset_path("/tiles/main-5-2.webp", Some("b7c99ee31b46")),
            "/tiles/main-5-2.webp?v=b7c99ee31b46"
        );
        assert_eq!(
            versioned_asset_path("/tiles/main-5-2.webp", Some("")),
            "/tiles/main-5-2.webp"
        );
        assert_eq!(
            versioned_asset_path("/tiles/main-5-2.webp", None),
            "/tiles/main-5-2.webp"
        );
    }

    #[test]
    fn html_asset_version_substitution_does_not_corrupt_runtime_property_names() {
        let html = br#"<script>const rawAssetVersion="__SEQUOIA_ASSET_VERSION__".trim(); window.SEQUOIA_ASSET_VERSION = rawAssetVersion;</script>"#.to_vec();
        let body = apply_html_body_substitutions(
            html,
            &HtmlResponseOptions {
                canonical: None,
                robots: None,
            },
            Some("abc123"),
        );

        let body = String::from_utf8(body).expect("utf8 body");
        assert!(body.contains(r#"const rawAssetVersion="abc123".trim();"#));
        assert!(body.contains("window.SEQUOIA_ASSET_VERSION = rawAssetVersion;"));
        assert!(!body.contains("window.abc123"));
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
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
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
        assert!(body.contains("name=\"description\""));
        assert!(body.contains("rel=\"canonical\" href=\"https://map.example.com/\""));
        assert!(body.contains("property=\"og:url\" content=\"https://map.example.com/\""));
        let expected_og_image = format!(
            "property=\"og:image\" content=\"{}{}\"",
            crate::config::map_public_base_url(),
            versioned_asset_path(DEFAULT_OG_IMAGE_PATH, public_asset_version())
        );
        assert!(body.contains(&expected_og_image));
        assert!(body.contains("application/ld+json"));
        assert!(body.contains("Sequoia Map: Live Wynncraft Territory Map"));
        assert!(body.contains("id=\"app\""));
    }

    #[tokio::test]
    async fn static_tile_route_keeps_unversioned_assets_short_cached() {
        let test_dir =
            std::env::temp_dir().join(format!("sequoia-static-cache-test-{}", std::process::id()));
        let tiles_dir = test_dir.join("tiles");
        std::fs::create_dir_all(&tiles_dir).expect("create test static dir");
        std::fs::write(tiles_dir.join("main-3-2.webp"), b"fake webp")
            .expect("write test tile asset");

        let app = static_assets_router(&test_dir);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/tiles/main-3-2.webp")
                    .body(Body::empty())
                    .expect("build static asset request"),
            )
            .await
            .expect("static asset request should succeed");

        let _ = std::fs::remove_dir_all(&test_dir);

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(SHORT_STATIC_CACHE_CONTROL)
        );
    }

    #[tokio::test]
    async fn static_tile_route_serves_immutable_cache_header_for_versioned_assets() {
        let test_dir = std::env::temp_dir().join(format!(
            "sequoia-static-cache-versioned-test-{}",
            std::process::id()
        ));
        let tiles_dir = test_dir.join("tiles");
        std::fs::create_dir_all(&tiles_dir).expect("create test static dir");
        std::fs::write(tiles_dir.join("main-3-2.webp"), b"fake webp")
            .expect("write test tile asset");

        let app = static_assets_router(&test_dir);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/tiles/main-3-2.webp?v=abc123")
                    .body(Body::empty())
                    .expect("build static asset request"),
            )
            .await
            .expect("static asset request should succeed");

        let _ = std::fs::remove_dir_all(&test_dir);

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(IMMUTABLE_STATIC_CACHE_CONTROL)
        );
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
