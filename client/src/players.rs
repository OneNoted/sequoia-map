//! Teammate heads on the map.
//!
//! The war controller feed already carries every teammate it knows about
//! ([`sequoia_shared::WarPlayer`]). This places them and loads their faces; the map renderer
//! draws them over the map and minimap with its own camera
//! ([`sequoia_browser_map::overlay::Teammates`]), so panning does no work here.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use leptos::prelude::*;
use sequoia_shared::WarPlayer;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::HtmlImageElement;

use crate::app::{
    CurrentMode, MapMode, PlayerHeadRenderHead, PlayerHeadRenderLabel, PlayerHeadSize,
    ShowPlayerHeads, WarControllerData, WarFeedVisible, clamp_player_head_size,
};
use crate::territory::ClientTerritoryMap;
use sequoia_browser_map::BrowserMap;
use sequoia_browser_map::overlay::{Teammate, Teammates};

/// Edge length of the face we ask the skin renderer for, in pixels. Fixed rather than
/// derived from the size setting so dragging the slider rescales what is already loaded
/// instead of refetching every head on every step.
const FACE_TEXTURE_PX: u32 = 64;

/// A teammate resolved to a world position, ready to project.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlayerPoint {
    pub username: String,
    pub x: f64,
    pub z: f64,
    /// Unit-circle nudge applied *after* projection, so the fan below stays a fixed pixel
    /// spread instead of collapsing as you zoom out. `(0.0, 0.0)` for a teammate who does
    /// not share their spot with anyone.
    pub fan: (f64, f64),
}

/// Places every teammate the feed mentions.
///
/// [`WarPlayer::pos`] wins where the backend sent it; a player inside a territory carries
/// no position and is placed at that territory's centre instead. A player with neither is
/// dropped - there is nowhere honest to put them.
///
/// Players sharing a resolved point - a whole war party at one territory centre - get a
/// [`PlayerPoint::fan`] nudge onto a small ring so they do not stack into what looks like a
/// single teammate. The ring is keyed on the name-sorted order rather than feed order so a
/// given player keeps their slot across polls instead of shuffling every few seconds; the
/// war stat cards sort for the same reason.
pub(crate) fn resolve_player_points(
    players: &[WarPlayer],
    territories: &ClientTerritoryMap,
) -> Vec<PlayerPoint> {
    let mut points: Vec<PlayerPoint> = players
        .iter()
        .filter_map(|player| {
            let (x, z) = match player.pos {
                Some(pos) => (pos.x, pos.z),
                None => {
                    let region = &territories
                        .get(player.territory.as_deref()?)?
                        .territory
                        .location;
                    (
                        f64::from(region.midpoint_x()),
                        f64::from(region.midpoint_y()),
                    )
                }
            };
            Some(PlayerPoint {
                username: player.username.clone(),
                x,
                z,
                fan: (0.0, 0.0),
            })
        })
        .collect();
    points.sort_by_key(|point| point.username.to_lowercase());

    // Bucket by exact coordinate: the only way two players land on the same spot in
    // practice is sharing a territory centre, which is an exact match by construction.
    let mut buckets: HashMap<(u64, u64), Vec<usize>> = HashMap::new();
    for (index, point) in points.iter().enumerate() {
        buckets
            .entry((point.x.to_bits(), point.z.to_bits()))
            .or_default()
            .push(index);
    }

    for indices in buckets.values() {
        if indices.len() < 2 {
            continue;
        }
        let count = indices.len() as f64;
        for (slot, &index) in indices.iter().enumerate() {
            let angle = std::f64::consts::TAU * slot as f64 / count;
            points[index].fan = (angle.cos(), angle.sin());
        }
    }

    points
}

type FaceCache = Rc<RefCell<HashMap<String, HtmlImageElement>>>;

/// Returns the cached face for `username`, starting the fetch on first sight.
///
/// The image is handed back before it has loaded; [`image_ready`] decides whether there is
/// anything to draw, and `on_settled` runs once the fetch has resolved either way. `None` only
/// when the document will not hand out an element at all.
fn face_image(
    cache: &FaceCache,
    username: &str,
    on_settled: Rc<dyn Fn()>,
) -> Option<HtmlImageElement> {
    if let Some(image) = cache.borrow().get(username) {
        return Some(image.clone());
    }

    let image = HtmlImageElement::new().ok()?;
    // The map renderer copies faces into a GPU texture, which needs CORS-clean pixels: the
    // request mode has to be set before the source. The skin renderer answers with
    // `Access-Control-Allow-Origin: *`; a response without it fails like any other error.
    image.set_cross_origin(Some("anonymous"));
    // NMSR 404s on a name it cannot resolve and rate-limits under load, so the error path
    // has to be wired too - `tiles.rs` does the same. A failed element stays in the cache
    // reporting `natural_width() == 0`, which keeps it from being refetched.
    let on_error = on_settled.clone();
    let onload = Closure::<dyn FnMut()>::new(move || on_settled());
    let onerror = Closure::<dyn FnMut()>::new(move || on_error());
    // `into_js_value` hands ownership of the closure to the image element, which keeps it
    // alive exactly as long as the element that can still fire it.
    image.set_onload(Some(onload.into_js_value().unchecked_ref()));
    image.set_onerror(Some(onerror.into_js_value().unchecked_ref()));
    image.set_src(&crate::auth::nmsr_face(username, FACE_TEXTURE_PX));
    cache
        .borrow_mut()
        .insert(username.to_string(), image.clone());
    Some(image)
}

/// Whether the face has pixels to draw. One still loading, or one that failed, is drawn as
/// a dot instead of an empty plate.
fn image_ready(image: &HtmlImageElement) -> bool {
    image.complete() && image.natural_width() > 0
}

#[component]
pub(crate) fn PlayerHeadsOverlay() -> impl IntoView {
    let ShowPlayerHeads(show_heads) = expect_context();
    let PlayerHeadRenderHead(render_head) = expect_context();
    let PlayerHeadRenderLabel(render_label) = expect_context();
    let PlayerHeadSize(head_size) = expect_context();
    let WarControllerData(warcontroller_state) = expect_context();
    let WarFeedVisible(war_feed_visible) = expect_context();
    let CurrentMode(map_mode) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let map = expect_context::<BrowserMap>();

    // The feed describes right now, so the heads must never be drawn over a past snapshot,
    // and never for a viewer the war feed is closed to - the same rule `WarQueuePanel`
    // follows.
    let visible = Memo::new(move |_| {
        show_heads.get() && war_feed_visible.get() && map_mode.get() == MapMode::Live
    });

    // World positions only move with the feed or the territories teammates stand in.
    let points = Memo::new(move |_| {
        if !visible.get() {
            return Vec::new();
        }
        warcontroller_state.with(|state| {
            state.as_ref().map_or_else(Vec::new, |state| {
                territories.with(|map| resolve_player_points(&state.players, map))
            })
        })
    });

    let faces: FaceCache = Rc::new(RefCell::new(HashMap::new()));
    // Bumped when a face finishes loading or fails, so the renderer gets the face or the dot.
    let faces_settled = RwSignal::new(0u64);
    let on_settled: Rc<dyn Fn()> =
        Rc::new(move || faces_settled.update(|count| *count = count.wrapping_add(1)));

    // Faces are cached per username; drop the ones no longer in the feed so a session left
    // open all evening does not accumulate every teammate who logged on at some point.
    // Nothing on screen means nothing worth keeping either.
    Effect::new({
        let faces = faces.clone();
        move || {
            points.with(|points| {
                let live: HashSet<&str> = points.iter().map(|p| p.username.as_str()).collect();
                faces
                    .borrow_mut()
                    .retain(|username, _| live.contains(username.as_str()));
            });
        }
    });

    // What the renderer draws, republished when the roster, a setting or a face changes.
    Effect::new(move || {
        faces_settled.track();
        let show_faces = render_head.get();
        let show_names = render_label.get();
        let head_size = clamp_player_head_size(head_size.get()) as f32;
        let teammates = points.with(|points| {
            (!points.is_empty()).then(|| {
                let players = points
                    .iter()
                    .map(|point| Teammate {
                        world: [point.x as f32, point.z as f32],
                        fan: [point.fan.0 as f32, point.fan.1 as f32],
                        username: point.username.clone(),
                        face: show_faces
                            .then(|| face_image(&faces, &point.username, on_settled.clone()))
                            .flatten()
                            .filter(image_ready),
                    })
                    .collect();
                Arc::new(Teammates {
                    players,
                    head_size,
                    show_faces,
                    show_names,
                })
            })
        });
        map.teammates().set(teammates);
    });
    on_cleanup(move || {
        map.teammates().try_set(None);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sequoia_shared::{GuildRef, Region, Territory, WarPlayerPos};

    use crate::territory::ClientTerritory;

    fn player(username: &str, territory: Option<&str>, pos: Option<(f64, f64)>) -> WarPlayer {
        WarPlayer {
            username: username.to_string(),
            class: "WARRIOR".to_string(),
            territory: territory.map(str::to_string),
            pos: pos.map(|(x, z)| WarPlayerPos { x, z }),
        }
    }

    fn territories(name: &str, start: [i32; 2], end: [i32; 2]) -> ClientTerritoryMap {
        let territory = Territory {
            guild: GuildRef {
                uuid: "0".to_string(),
                name: "Sequoia".to_string(),
                prefix: "SEQ".to_string(),
                color: None,
            },
            acquired: Utc::now(),
            location: Region { start, end },
            resources: Default::default(),
            connections: Vec::new(),
            runtime: None,
        };
        let mut map = ClientTerritoryMap::new();
        map.insert(
            name.to_string(),
            ClientTerritory::from_territory(name, territory),
        );
        map
    }

    #[test]
    fn explicit_position_wins_over_territory() {
        let map = territories("Ragni", [0, 0], [100, 100]);
        let points = resolve_player_points(
            &[player("Roamer", Some("Ragni"), Some((-1517.0, -5130.0)))],
            &map,
        );
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].x, -1517.0);
        assert_eq!(points[0].z, -5130.0);
    }

    #[test]
    fn territory_only_player_lands_on_the_territory_centre() {
        let map = territories("Ragni", [0, 0], [100, 200]);
        let points = resolve_player_points(&[player("Fighter", Some("Ragni"), None)], &map);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].x, 50.0);
        assert_eq!(points[0].z, 100.0);
    }

    #[test]
    fn player_with_neither_position_nor_known_territory_is_dropped() {
        let map = territories("Ragni", [0, 0], [100, 100]);
        assert!(resolve_player_points(&[player("Ghost", None, None)], &map).is_empty());
        assert!(resolve_player_points(&[player("Ghost", Some("Nowhere"), None)], &map).is_empty());
    }

    #[test]
    fn co_located_players_are_fanned_apart_in_a_stable_order() {
        let map = territories("Ragni", [0, 0], [100, 100]);
        let party = [
            player("charlie", Some("Ragni"), None),
            player("alice", Some("Ragni"), None),
            player("bob", Some("Ragni"), None),
        ];
        let points = resolve_player_points(&party, &map);
        assert_eq!(points.len(), 3);

        let names: Vec<&str> = points.iter().map(|p| p.username.as_str()).collect();
        assert_eq!(names, ["alice", "bob", "charlie"]);

        // They keep the shared territory centre; the ring separates them at draw time.
        for point in &points {
            assert_eq!((point.x, point.z), (50.0, 50.0));
            assert!(point.fan != (0.0, 0.0));
        }
        for pair in points.windows(2) {
            assert!(pair[0].fan != pair[1].fan);
        }

        // Feed order must not move anyone: the same party shuffled resolves identically.
        let shuffled = [
            player("bob", Some("Ragni"), None),
            player("charlie", Some("Ragni"), None),
            player("alice", Some("Ragni"), None),
        ];
        assert_eq!(points, resolve_player_points(&shuffled, &map));
    }

    #[test]
    fn a_lone_player_is_not_offset() {
        let map = territories("Ragni", [0, 0], [100, 100]);
        let points = resolve_player_points(&[player("solo", Some("Ragni"), None)], &map);
        assert_eq!(points[0].x, 50.0);
        assert_eq!(points[0].z, 50.0);
        assert_eq!(points[0].fan, (0.0, 0.0));
    }
}
