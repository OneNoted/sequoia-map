use std::sync::atomic::{AtomicBool, Ordering};

use futures::future::join3;
use leptos::prelude::*;
use web_sys::HtmlImageElement;

#[derive(Clone)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub struct ResourceAtlas {
    pub resource_image: HtmlImageElement,
    pub hq_crown_image: HtmlImageElement,
    pub territory_ornament_image: HtmlImageElement,
    pub sequoia_territory_ornament_image: HtmlImageElement,
}

pub const ICON_COUNT: u32 = 6;
pub const ATLAS_SRC: &str = "/icons/territory-resources-atlas.png";
pub const HQ_CROWN_SRC: &str = "/icons/crown_icon.png";
pub const TERRITORY_ORNAMENT_SRC: &str = "/icons/territory-ornament.png";
pub const SEQUOIA_TERRITORY_ORNAMENT_SRC: &str = "/icons/seq-border-v1.png";

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
    Some(format!(
        "display:inline-block;width:{size_px}px;height:{size_px}px;flex-shrink:0;vertical-align:middle;background-image:url('{ATLAS_SRC}');background-repeat:no-repeat;background-size:{}px {}px;background-position:-{}px 0px;",
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

pub fn load_resource_atlas(signal: RwSignal<Option<ResourceAtlas>>) {
    wasm_bindgen_futures::spawn_local(async move {
        let Ok(resource_image) = HtmlImageElement::new() else {
            signal.set(None);
            warn_atlas_once("Failed to create resource atlas image element.");
            return;
        };
        resource_image.set_src(ATLAS_SRC);
        if let Err(err) = wasm_bindgen_futures::JsFuture::from(resource_image.decode()).await {
            signal.set(None);
            warn_atlas_once(&format!("Failed to decode resource atlas: {:?}", err));
            return;
        }

        let Ok(hq_crown_image) = HtmlImageElement::new() else {
            signal.set(None);
            warn_atlas_once("Failed to create HQ crown icon image element.");
            return;
        };
        hq_crown_image.set_src(HQ_CROWN_SRC);
        let Ok(territory_ornament_image) = HtmlImageElement::new() else {
            signal.set(None);
            warn_atlas_once("Failed to create territory ornament image element.");
            return;
        };
        territory_ornament_image.set_src(TERRITORY_ORNAMENT_SRC);
        let Ok(sequoia_territory_ornament_image) = HtmlImageElement::new() else {
            signal.set(None);
            warn_atlas_once("Failed to create Sequoia territory ornament image element.");
            return;
        };
        sequoia_territory_ornament_image.set_src(SEQUOIA_TERRITORY_ORNAMENT_SRC);

        let (crown_result, ornament_result, sequoia_ornament_result) = join3(
            wasm_bindgen_futures::JsFuture::from(hq_crown_image.decode()),
            wasm_bindgen_futures::JsFuture::from(territory_ornament_image.decode()),
            wasm_bindgen_futures::JsFuture::from(sequoia_territory_ornament_image.decode()),
        )
        .await;

        if let Err(err) = crown_result {
            signal.set(None);
            warn_atlas_once(&format!("Failed to decode HQ crown icon: {:?}", err));
            return;
        }

        if let Err(err) = ornament_result {
            signal.set(None);
            warn_atlas_once(&format!(
                "Failed to decode territory ornament icon: {:?}",
                err
            ));
            return;
        }

        let sequoia_territory_ornament_image = match sequoia_ornament_result {
            Ok(_) => sequoia_territory_ornament_image,
            Err(err) => {
                warn_atlas_once(&format!(
                    "Failed to decode Sequoia territory ornament icon: {:?}",
                    err
                ));
                territory_ornament_image.clone()
            }
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
