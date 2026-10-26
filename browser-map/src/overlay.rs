//! Text and teammate markers a host draws over the map, at a constant screen size.
//!
//! The renderer draws them after the map and minimap with the main camera's projection, Map
//! Intel site names first, then teammates. Hosts publish a new value when the content changes;
//! panning and zooming only move the camera.

use web_sys::HtmlImageElement;

/// One Map Intel site name, drawn right of and above its site.
#[derive(Clone, Debug, PartialEq)]
pub struct SiteLabel {
    pub world: [f32; 2],
    pub text: String,
    pub rgb: [u8; 3],
}

/// The site names of one Map Intel payload, in drawing order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SiteLabels {
    pub labels: Vec<SiteLabel>,
}

/// A teammate placed on the map.
#[derive(Clone, Debug, PartialEq)]
pub struct Teammate {
    pub world: [f32; 2],
    /// Unit-circle nudge applied after projection, scaled by the head size, so co-located
    /// teammates keep a fixed pixel spread at every zoom.
    pub fan: [f32; 2],
    pub username: String,
    /// The face, once loaded with CORS; `None` draws a dot instead.
    pub face: Option<HtmlImageElement>,
}

/// Every teammate on the map and how to draw them, in drawing order.
#[derive(Clone, Debug, PartialEq)]
pub struct Teammates {
    pub players: Vec<Teammate>,
    /// Face edge length, CSS pixels.
    pub head_size: f32,
    /// Draw faces where loaded; otherwise every teammate is a dot.
    pub show_faces: bool,
    /// Draw names under the markers once zoomed in far enough.
    pub show_names: bool,
}

/// A plate on a war territory's top edge: its countdown, its war clock or how it ended.
#[derive(Clone, Debug, PartialEq)]
pub struct WarPlate {
    /// Top centre of the territory, world units. The plate straddles that edge.
    pub world: [f32; 2],
    pub label: PlateLabel,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlateLabel {
    /// Queued or entered, counting down to the war's start.
    StartsIn {
        entered: bool,
        seconds: i64,
    },
    /// The start has passed but the war is not confirmed.
    Due {
        entered: bool,
    },
    /// Queued or entered with no usable timing.
    Waiting {
        entered: bool,
    },
    /// A confirmed war: its elapsed time and the tower's remaining health, when known.
    Fighting {
        seconds: Option<i64>,
        health: Option<f32>,
    },
    Captured,
    Cancelled,
    Ended,
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
impl PlateLabel {
    /// The words a plate can show besides a clock; the renderer prepares exactly these glyphs.
    pub(crate) const WORDS: [&'static str; 7] = [
        "Due",
        "Queued",
        "Entered",
        "War",
        "Captured",
        "Cancelled",
        "Ended",
    ];

    pub(crate) fn text(self) -> std::borrow::Cow<'static, str> {
        use sequoia_shared::war_board::format_clock;
        let word = match self {
            Self::StartsIn { seconds, .. }
            | Self::Fighting {
                seconds: Some(seconds),
                ..
            } => {
                return format_clock(seconds).into();
            }
            Self::Due { .. } => "Due",
            Self::Waiting { entered: false } => "Queued",
            Self::Waiting { entered: true } => "Entered",
            Self::Fighting { seconds: None, .. } => "War",
            Self::Captured => "Captured",
            Self::Cancelled => "Cancelled",
            Self::Ended => "Ended",
        };
        word.into()
    }
}

/// Every plate on the map, in drawing order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WarPlates {
    pub plates: Vec<WarPlate>,
    /// The live feed is reconnecting, so the plates may be out of date: they are dimmed.
    pub stale: bool,
}

/// Which host input a renderer layer was last prepared from, and whether it must be prepared
/// again regardless (a new pixel ratio or a font that loaded).
///
/// Staleness is its own flag rather than a forgotten input: forgetting would make a layer the
/// host has just hidden look unchanged (`None` then `None`) and keep drawing the old one.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) struct Prepared<T> {
    held: Option<std::sync::Arc<T>>,
    stale: bool,
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
impl<T> Prepared<T> {
    pub(crate) fn new() -> Self {
        Self {
            held: None,
            stale: false,
        }
    }

    /// Marks the prepared layer out of date.
    pub(crate) fn invalidate(&mut self) {
        self.stale = true;
    }

    /// `Some(input)` when the layer must be rebuilt for `input`, where `Some(None)` means
    /// clear it; `None` when what it holds is current.
    pub(crate) fn update(
        &mut self,
        input: Option<&std::sync::Arc<T>>,
    ) -> Option<Option<std::sync::Arc<T>>> {
        let unchanged = match (input, self.held.as_ref()) {
            (Some(next), Some(held)) => std::sync::Arc::ptr_eq(next, held),
            (None, None) => true,
            _ => false,
        };
        if unchanged && !self.stale {
            return None;
        }
        self.stale = false;
        self.held = input.cloned();
        Some(self.held.clone())
    }
}

/// The whole-pixel size of an atlas measured at `width` by `height` device pixels, or `None`
/// unless both sides are finite, at least one pixel and within `max_side`, the device's largest
/// texture side. Checked before the scratch canvas or the texture is sized.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn atlas_extent(width: f64, height: f64, max_side: u32) -> Option<(u32, u32)> {
    let side = |length: f64| {
        (length.is_finite() && (1.0..=f64::from(max_side)).contains(&length))
            .then(|| length.ceil() as u32)
    };
    Some((side(width)?, side(height)?))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{PlateLabel, Prepared, atlas_extent};

    #[test]
    fn plate_text_uses_only_prepared_glyphs() {
        let prepared: String = PlateLabel::WORDS.concat() + "0123456789:";
        let labels = [
            PlateLabel::StartsIn {
                entered: false,
                seconds: 3_725,
            },
            PlateLabel::StartsIn {
                entered: true,
                seconds: 9,
            },
            PlateLabel::Due { entered: true },
            PlateLabel::Waiting { entered: false },
            PlateLabel::Waiting { entered: true },
            PlateLabel::Fighting {
                seconds: Some(47),
                health: Some(0.5),
            },
            PlateLabel::Fighting {
                seconds: None,
                health: None,
            },
            PlateLabel::Captured,
            PlateLabel::Cancelled,
            PlateLabel::Ended,
        ];
        for label in labels {
            let text = label.text();
            assert!(text.chars().all(|ch| prepared.contains(ch)), "{text}");
        }
    }

    #[test]
    fn a_countdown_plate_reads_as_a_clock_and_a_war_without_a_start_as_a_word() {
        assert_eq!(
            PlateLabel::StartsIn {
                entered: false,
                seconds: 128
            }
            .text(),
            "2:08"
        );
        assert_eq!(
            PlateLabel::Fighting {
                seconds: Some(47),
                health: None
            }
            .text(),
            "0:47"
        );
        assert_eq!(
            PlateLabel::Fighting {
                seconds: None,
                health: None
            }
            .text(),
            "War"
        );
        assert_eq!(PlateLabel::Due { entered: false }.text(), "Due");
    }

    #[test]
    fn an_atlas_within_the_device_limit_keeps_its_size() {
        assert_eq!(atlas_extent(2048.0, 96.0, 4096), Some((2048, 96)));
        assert_eq!(atlas_extent(4096.0, 4096.0, 4096), Some((4096, 4096)));
    }

    #[test]
    fn an_atlas_wider_or_taller_than_the_device_limit_is_refused() {
        assert_eq!(atlas_extent(4097.0, 96.0, 4096), None);
        assert_eq!(atlas_extent(2048.0, 4097.0, 4096), None);
    }

    #[test]
    fn an_atlas_with_an_invalid_side_is_refused() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, 0.5, -64.0] {
            assert_eq!(atlas_extent(bad, 96.0, 4096), None, "width {bad}");
            assert_eq!(atlas_extent(96.0, bad, 4096), None, "height {bad}");
        }
    }

    #[test]
    fn an_unchanged_input_is_not_prepared_again() {
        let input = Arc::new(1);
        let mut prepared = Prepared::new();
        prepared.update(Some(&input));
        assert!(prepared.update(Some(&input)).is_none());
    }

    #[test]
    fn a_new_input_is_prepared() {
        let mut prepared = Prepared::new();
        prepared.update(Some(&Arc::new(1)));
        assert_eq!(prepared.update(Some(&Arc::new(2))), Some(Some(Arc::new(2))));
    }

    #[test]
    fn an_invalidated_layer_the_host_hid_at_the_same_time_is_cleared() {
        let mut prepared = Prepared::new();
        prepared.update(Some(&Arc::new(1)));
        prepared.invalidate();
        assert_eq!(prepared.update(None), Some(None));
    }

    #[test]
    fn an_invalidated_layer_is_prepared_again_for_the_same_input() {
        let input = Arc::new(1);
        let mut prepared = Prepared::new();
        prepared.update(Some(&input));
        prepared.invalidate();
        assert_eq!(prepared.update(Some(&input)), Some(Some(input)));
    }

    #[test]
    fn a_hidden_layer_stays_cleared() {
        let mut prepared: Prepared<i32> = Prepared::new();
        prepared.update(None);
        assert!(prepared.update(None).is_none());
    }
}
