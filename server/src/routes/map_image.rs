//! The public live territory map as an image: `GET /api/live/map.webp` and `/api/live/map.png`.
//!
//! Optional `?width=` (800 to 4096 pixels, default 4096). PNG accepts `quality=compact` for
//! bounded RGB precision reduction; omitted or `quality=exact` preserves every pixel.
//! Responses carry a strong `ETag`, honour
//! `If-None-Match`, and describe the snapshot they show in `X-Map-Seq`, `X-Map-Timestamp`
//! (the live state's) and `X-Map-As-Of` (the minute hold times are measured from).

use axum::body::Body;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;

use super::http_util::if_none_match_matches;
use crate::map_image::{ImageFormat, ImageSpec, InvalidWidth, MapImageError, MapScene};
use crate::state::AppState;

const CACHE_CONTROL: &str = "public, max-age=5";
const X_MAP_SEQ: HeaderName = HeaderName::from_static("x-map-seq");
const X_MAP_TIMESTAMP: HeaderName = HeaderName::from_static("x-map-timestamp");
const X_MAP_AS_OF: HeaderName = HeaderName::from_static("x-map-as-of");

#[derive(Debug, Deserialize)]
pub struct ImageQuery {
    width: Option<u32>,
    #[serde(default)]
    quality: PngQuality,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum PngQuality {
    #[default]
    Exact,
    Compact,
}

pub async fn get_map_webp(
    State(state): State<AppState>,
    query: Result<Query<ImageQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Response {
    map_image(&state, ImageFormat::Webp, query, &headers).await
}

pub async fn get_map_png(
    State(state): State<AppState>,
    query: Result<Query<ImageQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Response {
    map_image(&state, ImageFormat::Png, query, &headers).await
}

async fn map_image(
    state: &AppState,
    format: ImageFormat,
    query: Result<Query<ImageQuery>, QueryRejection>,
    request_headers: &HeaderMap,
) -> Response {
    map_image_at(state, format, query, request_headers, Utc::now()).await
}

async fn map_image_at(
    state: &AppState,
    format: ImageFormat,
    query: Result<Query<ImageQuery>, QueryRejection>,
    request_headers: &HeaderMap,
    now: DateTime<Utc>,
) -> Response {
    let query = match query {
        Ok(Query(query)) => query,
        Err(_) => {
            return plain(
                StatusCode::BAD_REQUEST,
                &format!("{InvalidWidth}; PNG quality must be exact or compact"),
                None,
            );
        }
    };
    let format = if query.quality == PngQuality::Compact {
        if format == ImageFormat::Webp {
            return plain(
                StatusCode::BAD_REQUEST,
                "compact quality is only supported for PNG",
                None,
            );
        }
        ImageFormat::CompactPng
    } else {
        format
    };
    let spec = match ImageSpec::new(format, query.width) {
        Ok(spec) => spec,
        Err(error) => return plain(StatusCode::BAD_REQUEST, &error.to_string(), None),
    };

    // Scene and metadata come from one read of the snapshot, so they always agree.
    let (scene, mut headers) = {
        let snapshot = state.live_snapshot.read().await;
        if snapshot.territories.is_empty() {
            return plain(
                StatusCode::SERVICE_UNAVAILABLE,
                "The territory snapshot is not available yet.",
                Some("10"),
            );
        }
        let scene = MapScene::new(&snapshot.territories, now);
        let etag = state.map_images.etag(&scene, spec);
        let headers = metadata_headers(snapshot.seq, &snapshot.timestamp, &scene, etag);
        (scene, headers)
    };

    if headers
        .get(header::ETAG)
        .and_then(|etag| etag.to_str().ok())
        .is_some_and(|etag| if_none_match_matches(request_headers, etag))
    {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        response.headers_mut().extend(headers);
        return response;
    }

    let as_of = scene.as_of().format("%Y-%m-%dT%H-%MZ");
    match state.map_images.image(scene, spec).await {
        Ok(bytes) => {
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(format.content_type()),
            );
            if let Ok(disposition) = HeaderValue::from_str(&format!(
                "inline; filename=\"territory-map-{as_of}.{}\"",
                format.extension()
            )) {
                headers.insert(header::CONTENT_DISPOSITION, disposition);
            }
            let mut response = Response::new(Body::from(bytes));
            response.headers_mut().extend(headers);
            response
        }
        Err(MapImageError::Busy) => plain(
            StatusCode::SERVICE_UNAVAILABLE,
            "The map renderer is busy; retry shortly.",
            Some("5"),
        ),
        Err(error) => {
            tracing::error!(%error, "map image export failed");
            plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The map image could not be rendered.",
                None,
            )
        }
    }
}

fn metadata_headers(seq: u64, timestamp: &str, scene: &MapScene, etag: String) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    let values = [
        (header::ETAG, etag),
        (X_MAP_SEQ, seq.to_string()),
        (X_MAP_TIMESTAMP, timestamp.to_string()),
        (
            X_MAP_AS_OF,
            scene.as_of().to_rfc3339_opts(SecondsFormat::Secs, true),
        ),
    ];
    for (name, value) in values {
        if let Ok(value) = HeaderValue::from_str(&value) {
            headers.insert(name, value);
        }
    }
    headers
}

fn plain(status: StatusCode, message: &str, retry_after: Option<&'static str>) -> Response {
    let mut response = (status, message.to_string()).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(seconds) = retry_after {
        headers.insert(header::RETRY_AFTER, HeaderValue::from_static(seconds));
    }
    response
}

#[cfg(test)]
mod tests {
    use axum::body::{self, Body};
    use axum::extract::State;
    use axum::http::{HeaderMap, Request, StatusCode, header};
    use axum::response::Response;
    use chrono::{DateTime, TimeDelta, Utc};
    use sequoia_shared::{GuildRef, Region, Territory, TerritoryMap, TerritoryRuntimeData};
    use tower::util::ServiceExt;

    use super::map_image_at;
    use crate::map_image::{ImageFormat, MapImages};
    use crate::state::AppState;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../client/public");

    fn territory(guild: &str, prefix: &str, start: [i32; 2]) -> Territory {
        Territory {
            guild: GuildRef {
                uuid: format!("{guild}-uuid"),
                name: guild.to_string(),
                prefix: prefix.to_string(),
                color: Some((80, 160, 240)),
            },
            acquired: fixed_time() - TimeDelta::hours(5),
            location: Region {
                start,
                end: [start[0] + 180, start[1] + 140],
            },
            resources: Default::default(),
            connections: Vec::new(),
            runtime: None,
        }
    }

    async fn state_with(territories: TerritoryMap, seq: u64) -> AppState {
        let mut state = AppState::new(None);
        state.map_images = MapImages::new(ASSETS);
        set_snapshot(&state, territories, seq).await;
        state
    }

    async fn set_snapshot(state: &AppState, territories: TerritoryMap, seq: u64) {
        let mut snapshot = state.live_snapshot.write().await;
        snapshot.territories = territories;
        snapshot.seq = seq;
        snapshot.timestamp = format!("2026-10-11T19:25:{:02}Z", seq % 60);
    }

    fn ragni_and_detlas() -> TerritoryMap {
        let mut ragni = territory("Sequoia", "SEQ", [-900, -1700]);
        ragni.connections = vec!["Detlas".to_string()];
        TerritoryMap::from([
            ("Ragni".to_string(), ragni),
            (
                "Detlas".to_string(),
                territory("Avicia", "AVO", [-600, -1700]),
            ),
        ])
    }

    fn fixed_time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-11T19:25:00Z")
            .expect("fixture time")
            .to_utc()
    }

    async fn get_at(
        state: &AppState,
        uri: &str,
        etag: Option<&str>,
        now: DateTime<Utc>,
    ) -> Response {
        let mut request = Request::builder().uri(uri);
        if let Some(etag) = etag {
            request = request.header(header::IF_NONE_MATCH, etag);
        }
        // Exercise the same HTTP extraction and response contract with a controlled
        // clock; a slow image render must not make cache assertions cross a minute.
        axum::Router::new()
            .route(
                "/api/live/map.png",
                axum::routing::get(
                    move |State(state): State<AppState>, query, headers: HeaderMap| async move {
                        map_image_at(&state, ImageFormat::Png, query, &headers, now).await
                    },
                ),
            )
            .route(
                "/api/live/map.webp",
                axum::routing::get(
                    move |State(state): State<AppState>, query, headers: HeaderMap| async move {
                        map_image_at(&state, ImageFormat::Webp, query, &headers, now).await
                    },
                ),
            )
            .with_state(state.clone())
            .oneshot(request.body(Body::empty()).expect("request"))
            .await
            .expect("response")
    }

    async fn get(state: &AppState, uri: &str, etag: Option<&str>) -> axum::response::Response {
        let mut request = Request::builder().uri(uri);
        if let Some(etag) = etag {
            request = request.header(header::IF_NONE_MATCH, etag);
        }
        crate::app::build_app(state.clone())
            .oneshot(request.body(Body::empty()).expect("request"))
            .await
            .expect("response")
    }

    fn header_value<'a>(response: &'a axum::response::Response, name: &str) -> &'a str {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
    }

    async fn body_bytes(response: axum::response::Response) -> bytes::Bytes {
        body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body")
    }

    #[tokio::test]
    async fn webp_export_carries_snapshot_metadata_and_revalidates() {
        let state = state_with(ragni_and_detlas(), 41).await;

        let first = get_at(&state, "/api/live/map.webp?width=800", None, fixed_time()).await;

        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(header_value(&first, "content-type"), "image/webp");
        assert_eq!(header_value(&first, "cache-control"), "public, max-age=5");
        assert_eq!(header_value(&first, "x-map-seq"), "41");
        assert_eq!(
            header_value(&first, "x-map-timestamp"),
            "2026-10-11T19:25:41Z"
        );
        assert!(header_value(&first, "x-map-as-of").ends_with(":00Z"));
        assert!(header_value(&first, "content-disposition").ends_with(".webp\""));
        let etag = header_value(&first, "etag").to_string();
        assert!(etag.starts_with('"'), "strong validator expected: {etag}");
        let body = body_bytes(first).await;
        assert_eq!((&body[..4], &body[8..12]), (&b"RIFF"[..], &b"WEBP"[..]));

        let revalidated = get_at(
            &state,
            "/api/live/map.webp?width=800",
            Some(&etag),
            fixed_time(),
        )
        .await;

        assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(header_value(&revalidated, "etag"), etag);
        assert_eq!(header_value(&revalidated, "x-map-seq"), "41");
        assert!(body_bytes(revalidated).await.is_empty());
    }

    #[tokio::test]
    async fn runtime_only_update_keeps_the_image_with_fresh_metadata() {
        let state = state_with(ragni_and_detlas(), 7).await;
        let first = get_at(&state, "/api/live/map.webp?width=800", None, fixed_time()).await;
        let etag = header_value(&first, "etag").to_string();
        let mut updated = ragni_and_detlas();
        if let Some(ragni) = updated.get_mut("Ragni") {
            ragni.runtime = Some(TerritoryRuntimeData {
                treasury: Some("VERY_HIGH".to_string()),
                ..Default::default()
            });
        }
        set_snapshot(&state, updated, 8).await;

        let revalidated = get_at(
            &state,
            "/api/live/map.webp?width=800",
            Some(&etag),
            fixed_time(),
        )
        .await;

        assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(header_value(&revalidated, "etag"), etag);
        assert_eq!(header_value(&revalidated, "x-map-seq"), "8");
    }

    #[tokio::test]
    async fn ownership_change_replaces_the_image() {
        let state = state_with(ragni_and_detlas(), 7).await;
        let first = get_at(&state, "/api/live/map.webp?width=800", None, fixed_time()).await;
        let etag = header_value(&first, "etag").to_string();
        let mut updated = ragni_and_detlas();
        updated.insert(
            "Detlas".to_string(),
            territory("Sequoia", "SEQ", [-600, -1700]),
        );
        set_snapshot(&state, updated, 9).await;

        let changed = get_at(
            &state,
            "/api/live/map.webp?width=800",
            Some(&etag),
            fixed_time(),
        )
        .await;

        assert_eq!(changed.status(), StatusCode::OK);
        assert_ne!(header_value(&changed, "etag"), etag);
        assert_eq!(header_value(&changed, "x-map-seq"), "9");
    }

    #[tokio::test]
    async fn minute_rollover_replaces_the_image_with_fresh_hold_times() {
        let state = state_with(ragni_and_detlas(), 7).await;
        let first = get_at(&state, "/api/live/map.webp?width=800", None, fixed_time()).await;
        let etag = header_value(&first, "etag").to_string();

        let changed = get_at(
            &state,
            "/api/live/map.webp?width=800",
            Some(&etag),
            fixed_time() + TimeDelta::minutes(1),
        )
        .await;

        assert_eq!(changed.status(), StatusCode::OK);
        assert_ne!(header_value(&changed, "etag"), etag);
        assert_eq!(
            header_value(&changed, "x-map-as-of"),
            "2026-10-11T19:26:00Z"
        );
        assert_eq!(header_value(&changed, "x-map-seq"), "7");
    }

    #[tokio::test]
    async fn png_export_has_the_requested_width() {
        let state = state_with(ragni_and_detlas(), 3).await;

        let response = get(&state, "/api/live/map.png?width=800", None).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header_value(&response, "content-type"), "image/png");
        let body = body_bytes(response).await;
        let reader = png::Decoder::new(std::io::Cursor::new(body))
            .read_info()
            .expect("png header");
        assert_eq!(reader.info().width, 800);
        assert!(reader.info().height > 1_000);
    }

    #[tokio::test]
    async fn compact_png_is_opt_in_and_has_an_independent_cache_and_validator() {
        let state = state_with(ragni_and_detlas(), 3).await;
        let exact = get_at(&state, "/api/live/map.png?width=800", None, fixed_time()).await;
        assert_eq!(exact.status(), StatusCode::OK);
        let exact_tag = header_value(&exact, "etag").to_string();
        let exact_bytes = body_bytes(exact).await;
        let explicitly_exact = get_at(
            &state,
            "/api/live/map.png?width=800&quality=exact",
            Some(&exact_tag),
            fixed_time(),
        )
        .await;
        assert_eq!(explicitly_exact.status(), StatusCode::NOT_MODIFIED);

        let compact = get_at(
            &state,
            "/api/live/map.png?width=800&quality=compact",
            Some(&exact_tag),
            fixed_time(),
        )
        .await;
        assert_eq!(compact.status(), StatusCode::OK);
        assert_eq!(header_value(&compact, "content-type"), "image/png");
        let compact_tag = header_value(&compact, "etag").to_string();
        assert_ne!(compact_tag, exact_tag);
        let compact_bytes = body_bytes(compact).await;
        let decode = |bytes| {
            let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
                .read_info()
                .expect("header");
            let mut pixels = vec![0; reader.output_buffer_size().expect("size")];
            let info = reader.next_frame(&mut pixels).expect("pixels");
            assert_eq!(info.width, 800);
            pixels
        };
        let exact_pixels = decode(&exact_bytes);
        let compact_pixels = decode(&compact_bytes);
        assert_eq!(exact_pixels.len(), compact_pixels.len());
        assert_ne!(exact_pixels, compact_pixels);
        assert!(
            exact_pixels
                .iter()
                .zip(compact_pixels)
                .all(|(&exact, compact)| exact.abs_diff(compact) <= 4)
        );
        let compact_cached = get_at(
            &state,
            "/api/live/map.png?width=800&quality=compact",
            Some(&format!("W/{compact_tag}")),
            fixed_time(),
        )
        .await;
        assert_eq!(compact_cached.status(), StatusCode::NOT_MODIFIED);
        assert!(body_bytes(compact_cached).await.is_empty());

        let exact_cached = get_at(
            &state,
            "/api/live/map.png?width=800",
            Some(&compact_tag),
            fixed_time(),
        )
        .await;
        assert_eq!(exact_cached.status(), StatusCode::OK);
        assert_eq!(header_value(&exact_cached, "etag"), exact_tag);
        assert_eq!(body_bytes(exact_cached).await, exact_bytes);
    }

    #[tokio::test]
    async fn unsupported_quality_options_fail_before_rendering() {
        let mut state = state_with(ragni_and_detlas(), 3).await;
        state.map_images = MapImages::new("/nonexistent-map-assets");
        for uri in [
            "/api/live/map.png?quality=low",
            "/api/live/map.webp?quality=compact",
        ] {
            let response = get(&state, uri, None).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(header_value(&response, "cache-control"), "no-store");
        }
    }

    #[tokio::test]
    async fn invalid_widths_are_rejected_without_rendering() {
        let mut state = state_with(ragni_and_detlas(), 3).await;
        state.map_images = MapImages::new("/nonexistent-map-assets");

        for uri in [
            "/api/live/map.webp?width=10",
            "/api/live/map.webp?width=99999",
            "/api/live/map.png?width=wide",
        ] {
            let response = get(&state, uri, None).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(header_value(&response, "cache-control"), "no-store");
            let body = body_bytes(response).await;
            assert!(
                String::from_utf8_lossy(&body).contains("width must be"),
                "{uri}"
            );
        }
    }

    #[tokio::test]
    async fn empty_snapshot_is_unavailable() {
        let mut state = AppState::new(None);
        state.map_images = MapImages::new(ASSETS);

        let response = get(&state, "/api/live/map.webp", None).await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(header_value(&response, "retry-after"), "10");
    }

    #[tokio::test]
    async fn missing_assets_fail_with_a_server_error() {
        let mut state = state_with(ragni_and_detlas(), 3).await;
        state.map_images = MapImages::new("/nonexistent-map-assets");

        let response = get(&state, "/api/live/map.webp?width=800", None).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(header_value(&response, "cache-control"), "no-store");
        let body = body_bytes(response).await;
        assert_eq!(&body[..], b"The map image could not be rendered.");
    }
}
