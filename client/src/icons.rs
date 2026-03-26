use std::sync::atomic::{AtomicBool, Ordering};

use futures::future::join3;
use leptos::prelude::*;
use web_sys::HtmlImageElement;

use crate::assets::app_asset_url;

#[derive(Clone)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub struct ResourceAtlas {
    pub resource_image: HtmlImageElement,
    pub hq_crown_image: HtmlImageElement,
    pub territory_ornament_image: HtmlImageElement,
    pub sequoia_territory_ornament_image: HtmlImageElement,
}

pub const ICON_COUNT: u32 = 6;
const ATLAS_PATH: &str = "icons/territory-resources-atlas.png";
const HQ_CROWN_PATH: &str = "icons/crown_icon.png";
const TERRITORY_ORNAMENT_PATH: &str = "icons/territory-ornament.png";
const SEQUOIA_TERRITORY_ORNAMENT_PATH: &str = "icons/seq-border-v1.png";
const TRANSPARENT_PLACEHOLDER_DATA_URL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGBgAAAABQABpfZFQAAAAABJRU5ErkJggg==";

static ATLAS_WARNED: AtomicBool = AtomicBool::new(false);

pub fn icon_index(name: &str) -> Option<u32> {
    match name {
        "emerald" => Some(0),
        "ore" => Some(1),
        "crops" => Some(2),
        "fish" => Some(3),
        "wood" => Some(4),
        "rainbow" => Some(5),
        _ => None,
    }
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[cfg_attr(not(test), allow(dead_code))]
pub fn icon_uv(index: u32) -> [f32; 4] {
    debug_assert!(index < ICON_COUNT, "icon_uv index out of range: {index}");
    let idx = index.min(ICON_COUNT - 1);
    let cell_w = 1.0 / ICON_COUNT as f32;
    let u0 = idx as f32 * cell_w;
    let u1 = u0 + cell_w;
    [u0, 0.0, u1, 1.0]
}

pub fn sprite_style(name: &str, size_px: u32) -> Option<String> {
    let idx = icon_index(name)?;
    let atlas_src = app_asset_url(ATLAS_PATH);
    Some(format!(
        "display:inline-block;width:{size_px}px;height:{size_px}px;flex-shrink:0;vertical-align:middle;background-image:url('{atlas_src}');background-repeat:no-repeat;background-size:{}px {}px;background-position:-{}px 0px;",
        ICON_COUNT * size_px,
        size_px,
        idx * size_px,
    ))
}

fn warn_atlas_once(message: &str) {
    if ATLAS_WARNED
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        web_sys::console::warn_1(&message.into());
    }
}

async fn decode_image(src: &str) -> Result<HtmlImageElement, String> {
    let image =
        HtmlImageElement::new().map_err(|_| format!("Failed to create image element for {src}"))?;
    image.set_src(src);
    wasm_bindgen_futures::JsFuture::from(image.decode())
        .await
        .map_err(|err| format!("{err:?}"))?;
    Ok(image)
}

async fn load_optional_image(
    asset_src: &str,
    asset_name: &str,
) -> Result<HtmlImageElement, String> {
    match decode_image(asset_src).await {
        Ok(image) => Ok(image),
        Err(err) => {
            warn_atlas_once(&format!(
                "Failed to decode {asset_name}; using transparent placeholder: {err}"
            ));
            decode_image(TRANSPARENT_PLACEHOLDER_DATA_URL)
                .await
                .map_err(|placeholder_err| {
                    format!(
                        "transparent icon placeholder failed after {asset_name} load failure: {placeholder_err}"
                    )
                })
        }
    }
}

pub fn load_resource_atlas(signal: RwSignal<Option<ResourceAtlas>>) {
    wasm_bindgen_futures::spawn_local(async move {
        let atlas_src = app_asset_url(ATLAS_PATH);
        let hq_crown_src = app_asset_url(HQ_CROWN_PATH);
        let territory_ornament_src = app_asset_url(TERRITORY_ORNAMENT_PATH);
        let sequoia_territory_ornament_src = app_asset_url(SEQUOIA_TERRITORY_ORNAMENT_PATH);

        let Ok(resource_image) = HtmlImageElement::new() else {
            signal.set(None);
            warn_atlas_once("Failed to create resource atlas image element.");
            return;
        };
        resource_image.set_src(&atlas_src);
        if let Err(err) = wasm_bindgen_futures::JsFuture::from(resource_image.decode()).await {
            signal.set(None);
            warn_atlas_once(&format!("Failed to decode resource atlas: {:?}", err));
            return;
        }

        let (hq_crown_image, territory_ornament_image, sequoia_territory_ornament_image) = join3(
            load_optional_image(&hq_crown_src, "HQ crown icon"),
            load_optional_image(&territory_ornament_src, "territory ornament icon"),
            load_optional_image(
                &sequoia_territory_ornament_src,
                "Sequoia territory ornament icon",
            ),
        )
        .await;

        let Ok(hq_crown_image) = hq_crown_image else {
            signal.set(None);
            warn_atlas_once("Failed to create an HQ crown placeholder image.");
            return;
        };
        let Ok(territory_ornament_image) = territory_ornament_image else {
            signal.set(None);
            warn_atlas_once("Failed to create a territory ornament placeholder image.");
            return;
        };
        let Ok(sequoia_territory_ornament_image) = sequoia_territory_ornament_image else {
            signal.set(None);
            warn_atlas_once("Failed to create a Sequoia ornament placeholder image.");
            return;
        };

        signal.set(Some(ResourceAtlas {
            resource_image,
            hq_crown_image,
            territory_ornament_image,
            sequoia_territory_ornament_image,
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32) {
        let diff = (actual - expected).abs();
        assert!(
            diff < 1e-6,
            "expected {expected}, got {actual} (diff: {diff})"
        );
    }

    #[test]
    fn icon_index_lookup() {
        assert_eq!(icon_index("emerald"), Some(0));
        assert_eq!(icon_index("ore"), Some(1));
        assert_eq!(icon_index("crops"), Some(2));
        assert_eq!(icon_index("fish"), Some(3));
        assert_eq!(icon_index("wood"), Some(4));
        assert_eq!(icon_index("rainbow"), Some(5));
        assert_eq!(icon_index("unknown"), None);
    }

    #[test]
    fn icon_uv_grid_mapping() {
        let uv0 = icon_uv(0);
        assert_close(uv0[0], 0.0);
        assert_close(uv0[1], 0.0);
        assert_close(uv0[2], 1.0 / 6.0);
        assert_close(uv0[3], 1.0);

        let uv1 = icon_uv(1);
        assert_close(uv1[0], 1.0 / 6.0);
        assert_close(uv1[1], 0.0);
        assert_close(uv1[2], 2.0 / 6.0);
        assert_close(uv1[3], 1.0);

        let uv5 = icon_uv(5);
        assert_close(uv5[0], 5.0 / 6.0);
        assert_close(uv5[1], 0.0);
        assert_close(uv5[2], 1.0);
        assert_close(uv5[3], 1.0);
    }
}
