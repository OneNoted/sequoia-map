//! Where a headquarters' crown and its caption go, for each [`HqCrownStyle`].
//!
//! The caption is the line that names the owner: the guild tag, or for
//! [`HqCrownStyle::NameBelow`] the guild's name. The crown sits in a band at the top of the
//! territory and the caption moves below it; where the territory is too small for both, the
//! crown gives way, so the owner stays readable. Only [`HqCrownStyle::Fill`], chosen
//! explicitly, lets the crown replace the labels.

use crate::settings::{HQ_CROWN_SCALE_MAX, HQ_CROWN_SCALE_MIN, HqCrownStyle, RenderSettings};

/// Fill takes over at or below this many screen pixels per world unit.
const FILL_MAX_PX_PER_WORLD: f32 = 1.15;
const FILL_BOX_FRACTION: f32 = 0.90;
const FILL_MAX_RENDERED_PX: f32 = 96.0;
/// Space around the crown band, in screen pixels.
const TOP_PADDING_PX: f32 = 4.0;
const SIDE_PADDING_PX: f32 = 4.0;
const CAPTION_GAP_PX: f32 = 3.0;
/// A crown is drawn at least this large when there is room for it.
const MIN_COMFORTABLE_PX: f32 = 12.0;
/// Below this there is no crown at all; the gold border still marks the HQ.
const MIN_RENDERED_PX: f32 = 8.0;

/// The user's crown choice, ready to lay out territories with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HqCrown {
    style: HqCrownStyle,
    scale: f32,
}

/// The territory a crown is laid out in, world units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerritoryBox {
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrownSlot {
    pub center_y: f32,
    pub size: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HqLayout {
    /// `None` when the territory has no room for a crown beside its caption.
    pub crown: Option<CrownSlot>,
    /// Where the caption's centre moves to, clear of the crown.
    pub caption_y: f32,
}

impl HqCrown {
    pub fn new(style: HqCrownStyle, scale: f32) -> Self {
        let scale = if scale.is_finite() {
            scale.clamp(HQ_CROWN_SCALE_MIN, HQ_CROWN_SCALE_MAX)
        } else {
            1.0
        };
        Self { style, scale }
    }

    pub fn from_settings(settings: &RenderSettings) -> Self {
        Self::new(settings.hq_crown_style, settings.hq_crown_scale)
    }

    /// Whether the crown replaces the territory's labels at this zoom.
    pub fn fills(self, px_per_world: f32) -> bool {
        self.style == HqCrownStyle::Fill && px_per_world <= FILL_MAX_PX_PER_WORLD
    }

    /// Crown edge when it [`fills`](Self::fills), centred on the territory.
    pub fn fill_size(self, short_side_world: f32, px_per_world: f32) -> f32 {
        let natural = (short_side_world * FILL_BOX_FRACTION)
            .min(FILL_MAX_RENDERED_PX / px_per_world.max(0.0001));
        (natural * self.scale).clamp(1.0, short_side_world.max(1.0))
    }

    /// Whether the caption is the guild's name rather than its tag.
    pub fn shows_guild_name(self) -> bool {
        self.style == HqCrownStyle::NameBelow
    }

    /// The crown's band and the caption's new centre, for a caption `caption_size` world
    /// units tall that would otherwise be centred at `caption_y`.
    pub fn layout(
        self,
        territory: TerritoryBox,
        px_per_world: f32,
        caption_size: f32,
        caption_y: f32,
    ) -> HqLayout {
        let unchanged = HqLayout {
            crown: None,
            caption_y,
        };
        let px_per_world = px_per_world.max(0.0001);
        let (size_multiplier, band_fraction) = match self.style {
            HqCrownStyle::NameBelow => (2.4, 0.45),
            HqCrownStyle::Compact | HqCrownStyle::Fill => (1.75, 0.24),
        };
        let top_padding = TOP_PADDING_PX / px_per_world;
        let gap = CAPTION_GAP_PX / px_per_world;
        let max_size = (territory.width - 2.0 * SIDE_PADDING_PX / px_per_world)
            .min(territory.height * (band_fraction * self.scale).min(0.6))
            // The caption keeps its line below the crown, inside the territory.
            .min(territory.height - 2.0 * top_padding - gap - caption_size);
        if !max_size.is_finite() || max_size * px_per_world < MIN_RENDERED_PX {
            return unchanged;
        }
        let preferred = caption_size * size_multiplier * self.scale;
        let size = preferred.clamp((MIN_COMFORTABLE_PX / px_per_world).min(max_size), max_size);
        let crown_top = territory.top + top_padding;
        HqLayout {
            crown: Some(CrownSlot {
                center_y: crown_top + size * 0.5,
                size,
            }),
            caption_y: caption_y.max(crown_top + size + gap + caption_size * 0.5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TALL: TerritoryBox = TerritoryBox {
        top: 10.0,
        width: 64.0,
        height: 160.0,
    };

    fn crown(style: HqCrownStyle) -> HqCrown {
        HqCrown::new(style, 1.0)
    }

    #[test]
    fn the_caption_always_clears_the_crown() {
        for style in [
            HqCrownStyle::NameBelow,
            HqCrownStyle::Compact,
            HqCrownStyle::Fill,
        ] {
            let layout = crown(style).layout(TALL, 2.0, 24.0, 45.0);
            let slot = layout.crown.expect("a tall territory has room for a crown");
            assert!(
                layout.caption_y - 12.0 >= slot.center_y + slot.size * 0.5,
                "{style:?}"
            );
        }
    }

    #[test]
    fn the_caption_stays_inside_its_territory() {
        let squat = TerritoryBox {
            top: 0.0,
            width: 120.0,
            height: 40.0,
        };
        let layout = crown(HqCrownStyle::NameBelow).layout(squat, 1.0, 12.0, 20.0);
        assert!(layout.caption_y + 6.0 <= squat.top + squat.height + 0.001);
    }

    #[test]
    fn a_territory_too_small_for_both_keeps_its_caption_and_drops_the_crown() {
        let tiny = TerritoryBox {
            top: 0.0,
            width: 30.0,
            height: 30.0,
        };
        let layout = crown(HqCrownStyle::NameBelow).layout(tiny, 0.5, 20.0, 15.0);
        assert_eq!(layout.crown, None);
        assert_eq!(layout.caption_y, 15.0);
    }

    #[test]
    fn name_below_draws_a_larger_crown_than_compact() {
        let name_below = crown(HqCrownStyle::NameBelow).layout(TALL, 2.0, 24.0, 45.0);
        let compact = crown(HqCrownStyle::Compact).layout(TALL, 2.0, 24.0, 45.0);
        assert!(name_below.crown.expect("crown").size > compact.crown.expect("crown").size);
    }

    #[test]
    fn the_size_setting_rescales_the_crown_within_its_territory() {
        let small = HqCrown::new(HqCrownStyle::Compact, 0.6).layout(TALL, 0.5, 24.0, 45.0);
        let large = HqCrown::new(HqCrownStyle::Compact, 1.6).layout(TALL, 0.5, 24.0, 45.0);
        let (small, large) = (small.crown.expect("crown"), large.crown.expect("crown"));
        assert!(large.size > small.size);
        assert!(large.size <= TALL.width);
    }

    #[test]
    fn only_fill_replaces_the_labels_and_only_when_zoomed_out() {
        assert!(crown(HqCrownStyle::Fill).fills(0.5));
        assert!(!crown(HqCrownStyle::Fill).fills(2.0));
        assert!(!crown(HqCrownStyle::NameBelow).fills(0.5));
        assert!(!crown(HqCrownStyle::Compact).fills(0.5));
    }

    #[test]
    fn a_filling_crown_never_outgrows_the_territory() {
        let huge = HqCrown::new(HqCrownStyle::Fill, 1.6);
        assert!(huge.fill_size(100.0, 0.5) <= 100.0);
        assert!((crown(HqCrownStyle::Fill).fill_size(100.0, 0.5) - 90.0).abs() < 0.001);
    }

    #[test]
    fn a_garbage_size_setting_falls_back_to_the_default() {
        assert_eq!(
            HqCrown::new(HqCrownStyle::Compact, f32::NAN),
            HqCrown::new(HqCrownStyle::Compact, 1.0)
        );
    }
}
