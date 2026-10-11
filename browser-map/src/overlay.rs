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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::Prepared;

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
