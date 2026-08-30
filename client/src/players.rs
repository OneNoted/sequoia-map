//! Teammate heads on the map.
//!
//! The war controller feed already carries every teammate it knows about
//! ([`sequoia_shared::WarPlayer`]); this draws them. It is a 2D overlay canvas over the
//! wgpu map, the same arrangement [`crate::map_intel`] uses - the heads are a handful of
//! screen-space sprites that change every few seconds, which is not worth an instance
//! buffer and a bind group in the GPU renderer.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use leptos::prelude::*;
use sequoia_shared::WarPlayer;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, HtmlImageElement};

use crate::app::{
    CurrentMode, MapMode, PlayerHeadRenderHead, PlayerHeadRenderLabel, PlayerHeadSize,
    ShowPlayerHeads, WarControllerData, WarFeedVisible, clamp_player_head_size,
};
use crate::map_intel::{canvas_context, in_screen_bounds};
use crate::render_loop::RenderScheduler;
use crate::territory::ClientTerritoryMap;
use crate::viewport::Viewport;

/// Edge length of the face we ask the skin renderer for, in pixels. Fixed rather than
/// derived from the size setting so dragging the slider rescales what is already cached
/// instead of refetching every head on every step.
const FACE_TEXTURE_PX: u32 = 64;
/// Screen-space radius of the ring co-located teammates are fanned out onto, as a fraction
/// of the head size. A war party shares one territory centre, and stacked heads would read
/// as a single player.
const FAN_RADIUS_FACTOR: f64 = 0.65;
/// Below this viewport scale the labels are dropped - the same clutter rule the map intel
/// markers follow.
const LABEL_MIN_SCALE: f64 = 0.35;
/// Radius of the dot drawn in place of a head when "Render Head" is off, in screen pixels.
const DOT_RADIUS: f64 = 3.0;

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
/// The image is handed back before it has loaded; [`image_ready`] is what decides whether
/// there is anything to paint, and `on_settled` brings the frame back once the fetch has
/// resolved either way. `None` only when the document will not hand out an element at all,
/// which is not a case that renders.
fn face_image(
    cache: &FaceCache,
    username: &str,
    on_settled: Rc<dyn Fn()>,
) -> Option<HtmlImageElement> {
    if let Some(image) = cache.borrow().get(username) {
        return Some(image.clone());
    }

    let image = HtmlImageElement::new().ok()?;
    // NMSR 404s on a name it cannot resolve and rate-limits under load, so the error path
    // has to be wired too - `tiles.rs` does the same. A failed element stays in the cache
    // reporting `natural_width() == 0`, which keeps it from being refetched every frame.
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

/// Whether the face has pixels to draw.
///
/// `draw_image` on an element that has not loaded is a silent no-op, so drawing the plate
/// unconditionally would leave a solid dark square where the head should be - for the
/// whole roster on the first war of a session, and permanently for a fetch that failed.
fn image_ready(image: &HtmlImageElement) -> bool {
    image.complete() && image.natural_width() > 0
}

fn draw_head(
    ctx: &CanvasRenderingContext2d,
    image: &HtmlImageElement,
    sx: f64,
    sy: f64,
    size: f64,
) {
    let half = size / 2.0;
    // A dark plate behind the face: skins have light pixels, and so do the map tiles.
    ctx.set_fill_style_str("rgba(12,14,23,0.85)");
    ctx.fill_rect(sx - half - 1.0, sy - half - 1.0, size + 2.0, size + 2.0);
    let _ = ctx.draw_image_with_html_image_element_and_dw_and_dh(
        image,
        sx - half,
        sy - half,
        size,
        size,
    );
}

fn draw_dot(ctx: &CanvasRenderingContext2d, sx: f64, sy: f64) {
    ctx.begin_path();
    let _ = ctx.arc(sx, sy, DOT_RADIUS, 0.0, std::f64::consts::TAU);
    ctx.set_fill_style_str("#f5c542");
    ctx.fill();
    ctx.set_line_width(1.0);
    ctx.set_stroke_style_str("rgba(12,14,23,0.9)");
    ctx.stroke();
}

fn draw_name(ctx: &CanvasRenderingContext2d, sx: f64, sy: f64, username: &str) {
    ctx.save();
    ctx.set_font("10px 'JetBrains Mono', monospace");
    ctx.set_text_align("center");
    ctx.set_shadow_color("rgba(0,0,0,0.85)");
    ctx.set_shadow_blur(4.0);
    ctx.set_fill_style_str("#f5c542");
    let _ = ctx.fill_text(username, sx, sy);
    ctx.restore();
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
    let viewport: RwSignal<Viewport> = expect_context();

    // The feed describes right now, so the heads must never be drawn over a past snapshot,
    // and never for a viewer the war feed is closed to - the same rule `WarQueuePanel`
    // follows.
    let visible = Memo::new(move |_| {
        show_heads.get() && war_feed_visible.get() && map_mode.get() == MapMode::Live
    });

    let canvas_ref = NodeRef::<leptos::html::Canvas>::new();
    let cached_ctx: Rc<RefCell<Option<CanvasRenderingContext2d>>> = Rc::new(RefCell::new(None));
    let faces: FaceCache = Rc::new(RefCell::new(HashMap::new()));

    // The render closure needs to schedule frames of its own - a face that finishes
    // loading has to bring one back - and the scheduler owns that closure. It therefore
    // reaches the scheduler weakly; the only strong handle lives in the tracking effect
    // below, so the whole thing is dropped with the component instead of leaking.
    let scheduler: Rc<RefCell<Option<Rc<RenderScheduler>>>> = Rc::new(RefCell::new(None));
    let repaint: Rc<dyn Fn()> = {
        let weak = Rc::downgrade(&scheduler);
        Rc::new(move || {
            if let Some(cell) = weak.upgrade()
                && let Some(scheduler) = cell.borrow().as_ref()
            {
                scheduler.mark_dirty();
            }
        })
    };

    let render = {
        let cached_ctx = cached_ctx.clone();
        let faces = faces.clone();
        let repaint = repaint.clone();
        move || {
            let Some(canvas) = canvas_ref.get_untracked() else {
                return false;
            };
            let canvas: &HtmlCanvasElement = &canvas;
            let Some((ctx, width, height)) = canvas_context(canvas, &cached_ctx) else {
                return false;
            };
            ctx.clear_rect(0.0, 0.0, width, height);
            if !visible.get_untracked() {
                // Nothing on screen means nothing worth keeping warm either.
                faces.borrow_mut().clear();
                return false;
            }

            let size = clamp_player_head_size(head_size.get_untracked());
            let points = warcontroller_state.with_untracked(|state| {
                state.as_ref().map_or_else(Vec::new, |state| {
                    territories.with_untracked(|map| resolve_player_points(&state.players, map))
                })
            });

            // Faces are cached per username; drop the ones no longer in the feed so a
            // session left open all evening does not accumulate every teammate who logged
            // on at some point.
            let live: HashSet<&str> = points.iter().map(|p| p.username.as_str()).collect();
            faces
                .borrow_mut()
                .retain(|username, _| live.contains(username.as_str()));

            let vp = viewport.get_untracked();
            let with_head = render_head.get_untracked();
            let with_label = render_label.get_untracked() && vp.scale >= LABEL_MIN_SCALE;

            ctx.set_image_smoothing_enabled(false);
            let fan_radius = size * FAN_RADIUS_FACTOR;
            for point in &points {
                let (sx, sy) = vp.world_to_screen(point.x, point.z);
                let (sx, sy) = (sx + point.fan.0 * fan_radius, sy + point.fan.1 * fan_radius);
                if !in_screen_bounds(sx, sy, width, height, size + 16.0) {
                    continue;
                }
                // The dot stands in for a face that is still loading or never arrived, as
                // well as for a label-only - or all-off - configuration, which would
                // otherwise leave the master toggle looking broken.
                let drew_head = with_head
                    && face_image(&faces, &point.username, repaint.clone())
                        .is_some_and(|image| {
                            let ready = image_ready(&image);
                            if ready {
                                draw_head(&ctx, &image, sx, sy, size);
                            }
                            ready
                        });
                if !drew_head {
                    draw_dot(&ctx, sx, sy);
                }
                if with_label {
                    let anchor = if drew_head { size / 2.0 } else { DOT_RADIUS };
                    draw_name(&ctx, sx, sy + anchor + 11.0, &point.username);
                }
            }
            ctx.set_image_smoothing_enabled(true);
            false
        }
    };
    *scheduler.borrow_mut() = Some(Rc::new(RenderScheduler::new(render)));

    Effect::new({
        let scheduler = scheduler.clone();
        move || {
            visible.track();
            viewport.track();
            warcontroller_state.track();
            territories.track();
            head_size.track();
            render_head.track();
            render_label.track();
            if let Some(scheduler) = scheduler.borrow().as_ref() {
                scheduler.mark_dirty();
            }
        }
    });

    // `z-index: 7` rather than 8: the overlay is mounted after `DefenseLegend`, which sits
    // at 8, and equal z-index means later DOM order wins - a teammate near the corner would
    // paint over the legend panel. `MapIntelOverlay` stays under it for the same reason.
    view! {
        <canvas
            node_ref=canvas_ref
            style:display=move || if visible.get() { "block" } else { "none" }
            style="position: absolute; inset: 0; width: 100%; height: 100%; z-index: 7; pointer-events: none;"
        />
    }
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
