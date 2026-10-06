//! Layout of the bundled territory-resource atlas.

pub const RESOURCE_ICONS: [&str; 6] = ["emerald", "ore", "crops", "fish", "wood", "rainbow"];
pub const ICON_COUNT: u32 = RESOURCE_ICONS.len() as u32;

pub fn icon_index(name: &str) -> Option<u32> {
    RESOURCE_ICONS
        .iter()
        .position(|icon| *icon == name)
        .map(|index| index as u32)
}
