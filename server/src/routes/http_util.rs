use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;

pub fn json_bytes_response(
    body: Bytes,
    cache_control: &'static str,
    etag: Option<&str>,
) -> Response {
    let mut response = Response::new(Body::from(body));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    if let Some(etag) = etag
        && let Ok(etag_header) = HeaderValue::from_str(etag)
    {
        headers.insert(header::ETAG, etag_header);
    }
    response
}

/// A JSON body that belongs to one session only.
///
/// `Vary: Cookie` on top of `private, no-store` so no shared cache can key this response
/// on its path alone and hand one visitor another viewer's answer.
pub fn private_json_bytes_response(body: Bytes) -> Response {
    let mut response = json_bytes_response(body, "private, no-store", None);
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Cookie"));
    response
}

pub fn not_modified_response(cache_control: &'static str, etag: Option<&str>) -> Response {
    let mut response = StatusCode::NOT_MODIFIED.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    if let Some(etag) = etag
        && let Ok(etag_header) = HeaderValue::from_str(etag)
    {
        headers.insert(header::ETAG, etag_header);
    }
    response
}

fn normalize_etag(candidate: &str) -> &str {
    candidate.strip_prefix("W/").unwrap_or(candidate).trim()
}

pub fn if_none_match_matches(headers: &HeaderMap, etag: &str) -> bool {
    let Some(value) = headers.get(header::IF_NONE_MATCH) else {
        return false;
    };
    let Ok(raw) = value.to_str() else {
        return false;
    };

    raw.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || normalize_etag(candidate) == normalize_etag(etag)
    })
}
