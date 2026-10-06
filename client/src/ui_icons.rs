//! DOM icon helpers for the map sidebar and player cards.
use crate::assets::versioned_app_asset_url;
use crate::icons::ATLAS_PATH;
use sequoia_map_engine::icon_atlas::{ICON_COUNT, icon_index};

const CLASS_ICON_DIR: &str = "icons/classes";

pub fn sprite_style(name: &str, size_px: u32) -> Option<String> {
    let idx = icon_index(name)?;
    let atlas_src = versioned_app_asset_url(ATLAS_PATH);
    Some(format!(
        "display:inline-block;width:{size_px}px;height:{size_px}px;flex-shrink:0;vertical-align:middle;background-image:url('{atlas_src}');background-repeat:no-repeat;background-size:{}px {}px;background-position:-{}px 0px;image-rendering:pixelated;",
        ICON_COUNT * size_px,
        size_px,
        idx * size_px,
    ))
}

pub fn class_icon_name(name: &str) -> Option<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "archer" | "hunter" => Some("archer"),
        "assassin" | "ninja" => Some("assassin"),
        "mage" | "darkwizard" | "dark wizard" => Some("mage"),
        "shaman" | "skyseer" => Some("shaman"),
        "warrior" | "knight" => Some("warrior"),
        _ => None,
    }
}

pub fn class_icon_url(name: &str) -> Option<String> {
    let icon = class_icon_name(name)?;
    Some(versioned_app_asset_url(&format!(
        "{CLASS_ICON_DIR}/{icon}.webp"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn class_icon_name_accepts_aliases_and_casing() {
        assert_eq!(class_icon_name("Archer"), Some("archer"));
        assert_eq!(class_icon_name("hunter"), Some("archer"));
        assert_eq!(class_icon_name(" NINJA "), Some("assassin"));
        assert_eq!(class_icon_name("Dark Wizard"), Some("mage"));
        assert_eq!(class_icon_name("skyseer"), Some("shaman"));
        assert_eq!(class_icon_name("knight"), Some("warrior"));
        assert_eq!(class_icon_name("shopkeeper"), None);
    }
}
