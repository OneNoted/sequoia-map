use std::sync::atomic::{AtomicBool, Ordering};

use futures::future::join3;
use leptos::prelude::*;
use web_sys::HtmlImageElement;

use crate::assets::versioned_app_asset_url;

#[derive(Clone)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub struct ResourceAtlas {
    pub resource_image: HtmlImageElement,
    pub hq_crown_image: HtmlImageElement,
    pub territory_ornament_image: HtmlImageElement,
    pub sequoia_territory_ornament_image: HtmlImageElement,
}

pub(crate) const ATLAS_PATH: &str = "icons/territory-resources-atlas.webp";
const HQ_CROWN_PATH: &str = "icons/crown_icon.webp";
const TERRITORY_ORNAMENT_PATH: &str = "icons/territory-ornament.webp";
const SEQUOIA_TERRITORY_ORNAMENT_PATH: &str = "icons/seq-border-v1.webp";
const TRANSPARENT_PLACEHOLDER_DATA_URL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGBgAAAABQABpfZFQAAAAABJRU5ErkJggg==";

static ATLAS_WARNED: AtomicBool = AtomicBool::new(false);

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
        let atlas_src = versioned_app_asset_url(ATLAS_PATH);
        let hq_crown_src = versioned_app_asset_url(HQ_CROWN_PATH);
        let territory_ornament_src = versioned_app_asset_url(TERRITORY_ORNAMENT_PATH);
        let sequoia_territory_ornament_src =
            versioned_app_asset_url(SEQUOIA_TERRITORY_ORNAMENT_PATH);

        let Ok(resource_image) = HtmlImageElement::new() else {
            signal.set(None);
            warn_atlas_once("Failed to create resource atlas image element.");
            return;
        };
        resource_image.set_src(&atlas_src);
        if let Err(err) = wasm_bindgen_futures::JsFuture::from(resource_image.decode()).await {
            signal.set(None);
            warn_atlas_once(&format!("Failed to decode resource atlas: {err:?}"));
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
