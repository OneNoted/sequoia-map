#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use js_sys::Reflect;
use wasm_bindgen::JsValue;

pub(crate) const ASSET_BASE_KEY: &str = "__SEQUOIA_ASSET_BASE__";
pub(crate) const ASSET_VERSION_KEY: &str = "SEQUOIA_ASSET_VERSION";

fn normalize_asset_base(base: &str) -> String {
    let trimmed = base.trim();
    if trimmed.is_empty() || trimmed == "/" {
        String::new()
    } else {
        format!("/{}", trimmed.trim_matches('/'))
    }
}

fn join_asset_url(base: &str, path: &str) -> String {
    let trimmed_path = path.trim().trim_start_matches('/');
    if trimmed_path.is_empty() {
        return if base.is_empty() {
            "/".to_string()
        } else {
            normalize_asset_base(base)
        };
    }

    let normalized_base = normalize_asset_base(base);
    if normalized_base.is_empty() {
        format!("/{trimmed_path}")
    } else {
        format!("{normalized_base}/{trimmed_path}")
    }
}

fn normalize_asset_version(version: &str) -> Option<String> {
    let trimmed = version.trim();
    if trimmed.is_empty() || trimmed.contains("__SEQUOIA_") {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn append_asset_version(url: &str, version: Option<&str>) -> String {
    match version.and_then(normalize_asset_version) {
        Some(version) => format!("{url}?v={version}"),
        None => url.to_string(),
    }
}

pub(crate) fn asset_base_path() -> String {
    web_sys::window()
        .and_then(|window| Reflect::get(window.as_ref(), &JsValue::from_str(ASSET_BASE_KEY)).ok())
        .and_then(|value| value.as_string())
        .map(|base| normalize_asset_base(&base))
        .unwrap_or_default()
}

pub(crate) fn asset_version() -> Option<String> {
    web_sys::window()
        .and_then(|window| {
            Reflect::get(window.as_ref(), &JsValue::from_str(ASSET_VERSION_KEY)).ok()
        })
        .and_then(|value| value.as_string())
        .and_then(|version| normalize_asset_version(&version))
}

pub(crate) fn app_asset_url(path: &str) -> String {
    join_asset_url(&asset_base_path(), path)
}

pub(crate) fn versioned_app_asset_url(path: &str) -> String {
    append_asset_version(&app_asset_url(path), asset_version().as_deref())
}

#[cfg(test)]
mod tests {
    use super::{append_asset_version, join_asset_url, normalize_asset_base, normalize_asset_version};

    #[test]
    fn normalize_asset_base_collapses_root_and_slashes() {
        assert_eq!(normalize_asset_base(""), "");
        assert_eq!(normalize_asset_base("/"), "");
        assert_eq!(normalize_asset_base("claims-app"), "/claims-app");
        assert_eq!(normalize_asset_base("/claims-app/"), "/claims-app");
    }

    #[test]
    fn join_asset_url_respects_optional_base_prefix() {
        assert_eq!(
            join_asset_url("", "icons/crown_icon.webp"),
            "/icons/crown_icon.webp"
        );
        assert_eq!(
            join_asset_url("/claims-app/", "/icons/crown_icon.webp"),
            "/claims-app/icons/crown_icon.webp"
        );
    }

    #[test]
    fn normalize_asset_version_ignores_empty_and_unsubstituted_tokens() {
        assert_eq!(normalize_asset_version(""), None);
        assert_eq!(normalize_asset_version(" __SEQUOIA_ASSET_VERSION__ "), None);
        assert_eq!(normalize_asset_version("b7c99ee31b46"), Some("b7c99ee31b46".to_string()));
    }

    #[test]
    fn append_asset_version_appends_cache_buster_when_available() {
        assert_eq!(
            append_asset_version("/tiles/main-5-2.webp", Some("b7c99ee31b46")),
            "/tiles/main-5-2.webp?v=b7c99ee31b46"
        );
        assert_eq!(
            append_asset_version("/tiles/main-5-2.webp", Some("__SEQUOIA_ASSET_VERSION__")),
            "/tiles/main-5-2.webp"
        );
    }
}
