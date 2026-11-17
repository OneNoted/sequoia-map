//! Bringing a territory into view where the viewer can see it. The map canvas fills the
//! window, and the navbar, the sidebar (a bottom sheet on a phone) and the war panel lie
//! over it, so a territory centred in the whole canvas can sit under one of them.

use leptos::prelude::*;
use sequoia_browser_map::MapCamera;
use sequoia_map_engine::gesture::ScreenRect;
use wasm_bindgen::JsCast;

use crate::territory::ClientTerritoryMap;

/// Below this many CSS pixels of clear map in either direction, focusing uses the whole
/// canvas: a sliver would zoom the territory out to nothing.
const MIN_CLEAR_PX: f64 = 120.0;

/// How far panels reach in from each edge of the map, in CSS pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Covers {
    pub top: f64,
    pub left: f64,
    pub right: f64,
    pub bottom: f64,
}

/// Whether the sidebar (a phone's bottom sheet) is open once the caller has finished, which
/// may differ from now: a focus that also opens or closes it makes room for where it ends up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SidebarAfter {
    Open,
    Closed,
}

impl SidebarAfter {
    pub fn open_if(open: bool) -> Self {
        if open { Self::Open } else { Self::Closed }
    }
}

/// The part of a `width` x `height` map that `covers` leave visible, or the whole map when
/// too little is left to show a territory in.
pub(crate) fn clear_area((width, height): (f64, f64), covers: Covers) -> ScreenRect {
    // Edges as the covers leave them, unsorted: covers that overlap leave a negative size,
    // which must fall back rather than be read as the gap between the panels.
    let clear = ScreenRect {
        left: covers.left.max(0.0),
        top: covers.top.max(0.0),
        right: width - covers.right.max(0.0),
        bottom: height - covers.bottom.max(0.0),
    };
    if clear.width() < MIN_CLEAR_PX || clear.height() < MIN_CLEAR_PX {
        ScreenRect::spanning((0.0, 0.0), (width, height))
    } else {
        clear
    }
}

/// Moves the camera onto territory `name`, if the map has it, within the part of the map the
/// panels leave clear once the sidebar is as `sidebar` says.
pub(crate) fn focus_territory(
    camera: MapCamera,
    territories: RwSignal<ClientTerritoryMap>,
    name: &str,
    is_mobile: bool,
    sidebar: SidebarAfter,
) {
    let Some(region) =
        territories.with_untracked(|map| map.get(name).map(|ct| ct.territory.location.clone()))
    else {
        return;
    };
    let area = clear_area(camera.canvas_size(), measured_covers(is_mobile, sidebar));
    camera.focus_within(&region, area);
}

/// What lies over the map with the sidebar as `sidebar` says, from the page's layout boxes.
/// The sidebar and war panel slide in and out with transforms, which layout sizes ignore, so
/// this holds while either is still moving.
fn measured_covers(is_mobile: bool, sidebar: SidebarAfter) -> Covers {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return Covers::default();
    };
    let element = |selector: &str| {
        document
            .query_selector(selector)
            .ok()
            .flatten()
            .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    };
    let mut covers = Covers {
        top: element(".seq-top-nav").map_or(0.0, |nav| nav.get_bounding_client_rect().bottom()),
        ..Covers::default()
    };
    if sidebar == SidebarAfter::Open
        && let Some(panel) = element(".sidebar-wrapper")
    {
        if is_mobile {
            covers.bottom = f64::from(panel.offset_height());
        } else {
            covers.right = f64::from(panel.offset_width());
        }
    }
    if let Some(war_panel) = element(".war-panel.war-panel-open") {
        covers.left = f64::from(war_panel.offset_left() + war_panel.offset_width());
    }
    covers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_desktop_sidebar_and_navbar_leave_the_rest_of_the_map() {
        let area = clear_area(
            (1440.0, 900.0),
            Covers {
                top: 56.0,
                right: 380.0,
                ..Covers::default()
            },
        );
        assert_eq!(
            area,
            ScreenRect {
                left: 0.0,
                top: 56.0,
                right: 1060.0,
                bottom: 900.0,
            }
        );
    }

    #[test]
    fn a_phone_sheet_leaves_the_strip_above_it() {
        let area = clear_area(
            (390.0, 844.0),
            Covers {
                top: 56.0,
                bottom: 590.8,
                ..Covers::default()
            },
        );
        assert_eq!(area.top, 56.0);
        assert!((area.bottom - 253.2).abs() < 1e-9);
        assert_eq!((area.left, area.right), (0.0, 390.0));
    }

    #[test]
    fn overlapping_side_panels_fall_back_to_the_whole_map() {
        // A widened war panel (16 + 460) and sidebar (620) over an 800 px desktop window.
        let area = clear_area(
            (800.0, 900.0),
            Covers {
                top: 56.0,
                left: 476.0,
                right: 620.0,
                ..Covers::default()
            },
        );
        assert_eq!(area, ScreenRect::spanning((0.0, 0.0), (800.0, 900.0)));
    }

    #[test]
    fn overlapping_top_and_bottom_covers_fall_back_to_the_whole_map() {
        let area = clear_area(
            (390.0, 500.0),
            Covers {
                top: 300.0,
                bottom: 350.0,
                ..Covers::default()
            },
        );
        assert_eq!(area, ScreenRect::spanning((0.0, 0.0), (390.0, 500.0)));
    }

    #[test]
    fn too_small_a_clear_area_falls_back_to_the_whole_map() {
        let area = clear_area(
            (390.0, 400.0),
            Covers {
                top: 56.0,
                bottom: 280.0,
                ..Covers::default()
            },
        );
        assert_eq!(
            area,
            ScreenRect {
                left: 0.0,
                top: 0.0,
                right: 390.0,
                bottom: 400.0,
            }
        );
    }
}
