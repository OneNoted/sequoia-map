use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use leptos::prelude::*;
use sequoia_map_engine::map_markers::{
    MapMarker, MapMarkers, MarkerShape, NODE_CLUSTER_SCALE, node_radius,
};
use sequoia_shared::{
    GatheringNodeMarker, MapActivityMarker, MapIntelOverlay as MapIntelPayload, WorldEventMarker,
};
use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

use crate::app::{IsMobile, MapIntelModeEnabled, SidebarOpen, SidebarWidth, canvas_dimensions};
use crate::viewport::Viewport;
use sequoia_browser_map::BrowserMap;
use sequoia_browser_map::render_loop::RenderScheduler;

const NODE_BUCKET_WORLD_SIZE: f64 = 256.0;
/// Event, raid and camp names show above this scale.
const SITE_LABEL_MIN_SCALE: f64 = 0.58;
const EVENT_COLOR: &str = "#f5c542";
const RAID_COLOR: &str = "#b18cff";
const CAMP_COLOR: &str = "#5bd6c8";
const FETCH_RETRY_DELAY_SECS: u64 = 10;
const MAP_INTEL_ENDPOINT: &str = "/api/map/intel/overlay";

#[derive(Clone, Debug, PartialEq)]
struct IntelHover {
    screen_x: f64,
    screen_y: f64,
    title: String,
    meta: String,
    color: &'static str,
}

#[derive(Debug)]
struct MapIntelModel {
    raids: Vec<RenderActivity>,
    camps: Vec<RenderActivity>,
    world_events: Vec<RenderWorldEvent>,
    gathering_nodes: NodeIndex,
    /// Every marker, for the map renderer to draw.
    markers: Arc<MapMarkers>,
}

impl MapIntelModel {
    fn from_payload(payload: MapIntelPayload) -> Self {
        let mut model = Self {
            raids: payload
                .raids
                .into_iter()
                .map(|entry| RenderActivity::from_marker(entry, "Raid"))
                .collect(),
            camps: payload
                .camps
                .into_iter()
                .map(|entry| RenderActivity::from_marker(entry, "Camp"))
                .collect(),
            world_events: payload
                .world_events
                .into_iter()
                .map(RenderWorldEvent::from_marker)
                .collect(),
            gathering_nodes: NodeIndex::from_markers(payload.gathering_nodes),
            markers: Arc::default(),
        };
        model.markers = Arc::new(model.map_markers());
        model
    }

    /// Nodes in a stable order, one summary per area and profession, then events, raids
    /// and camps, as the canvas overlay drew them.
    fn map_markers(&self) -> MapMarkers {
        let site = |x: f64, z: f64, shape: MarkerShape, color: &str| MapMarker {
            world: [x as f32, z as f32],
            shape,
            rgb: hex_rgb(color),
        };
        let mut sites = Vec::new();
        for event in &self.world_events {
            for &(x, z) in &event.locations {
                sites.push(site(x, z, MarkerShape::Event, EVENT_COLOR));
            }
        }
        for raid in &self.raids {
            sites.push(site(raid.x, raid.z, MarkerShape::Raid, RAID_COLOR));
        }
        for camp in &self.camps {
            sites.push(site(camp.x, camp.z, MarkerShape::Camp, CAMP_COLOR));
        }
        MapMarkers::new(
            self.gathering_nodes
                .sorted_buckets()
                .flatten()
                .map(|node| MapMarker {
                    world: [node.x as f32, node.z as f32],
                    shape: node.shape.marker_shape(),
                    rgb: hex_rgb(node.profession.style().color),
                })
                .collect(),
            self.gathering_nodes
                .summaries
                .iter()
                .map(|summary| MapMarker {
                    world: [summary.x as f32, summary.z as f32],
                    shape: MarkerShape::Summary,
                    rgb: hex_rgb(summary.profession.style().color),
                })
                .collect(),
            sites,
        )
    }

    fn total_markers(&self) -> usize {
        self.gathering_nodes.len() + self.world_events.len() + self.raids.len() + self.camps.len()
    }
}

#[derive(Debug)]
struct RenderActivity {
    name: String,
    meta: String,
    x: f64,
    z: f64,
}

impl RenderActivity {
    fn from_marker(entry: MapActivityMarker, kind_label: &str) -> Self {
        let meta = activity_meta(kind_label, &entry);
        Self {
            name: entry.name,
            meta,
            x: entry.location.x,
            z: entry.location.z,
        }
    }
}

#[derive(Debug)]
struct RenderWorldEvent {
    name: String,
    meta: String,
    locations: Vec<(f64, f64)>,
}

impl RenderWorldEvent {
    fn from_marker(event: WorldEventMarker) -> Self {
        let meta = event_meta(&event);
        Self {
            name: event.name,
            meta,
            locations: event
                .locations
                .into_iter()
                .map(|location| (location.x, location.z))
                .collect(),
        }
    }
}

#[derive(Default, Debug)]
struct NodeIndex {
    buckets: HashMap<NodeBucketKey, Vec<RenderNode>>,
    summaries: Vec<NodeBucketSummary>,
    count: usize,
}

impl NodeIndex {
    fn from_markers(nodes: Vec<GatheringNodeMarker>) -> Self {
        let mut index = Self {
            buckets: HashMap::new(),
            summaries: Vec::new(),
            count: nodes.len(),
        };

        for node in nodes {
            let node = RenderNode::from_marker(node);
            index
                .buckets
                .entry(NodeBucketKey::for_world_point(node.x, node.z))
                .or_default()
                .push(node);
        }

        for bucket in index.buckets.values_mut() {
            bucket.sort_by_key(|node| (node.profession, node.shape));
        }
        index.summaries = build_node_bucket_summaries(&index.buckets);

        index
    }

    fn len(&self) -> usize {
        self.count
    }

    /// Each area's nodes, areas ordered by position.
    fn sorted_buckets(&self) -> impl Iterator<Item = &Vec<RenderNode>> {
        let mut keys: Vec<&NodeBucketKey> = self.buckets.keys().collect();
        keys.sort_unstable();
        keys.into_iter().map(|key| &self.buckets[key])
    }

    fn for_each_in_world_bounds(
        &self,
        min_x: f64,
        min_z: f64,
        max_x: f64,
        max_z: f64,
        mut visit: impl FnMut(&RenderNode),
    ) {
        let min_bucket_x = node_bucket_coord(min_x);
        let max_bucket_x = node_bucket_coord(max_x);
        let min_bucket_z = node_bucket_coord(min_z);
        let max_bucket_z = node_bucket_coord(max_z);

        for bucket_x in min_bucket_x..=max_bucket_x {
            for bucket_z in min_bucket_z..=max_bucket_z {
                let key = NodeBucketKey {
                    x: bucket_x,
                    z: bucket_z,
                };
                let Some(nodes) = self.buckets.get(&key) else {
                    continue;
                };
                for node in nodes {
                    if node.x >= min_x && node.x <= max_x && node.z >= min_z && node.z <= max_z {
                        visit(node);
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
struct NodeBucketSummary {
    x: f64,
    z: f64,
    profession: ProfessionKind,
    count: usize,
}

fn build_node_bucket_summaries(
    buckets: &HashMap<NodeBucketKey, Vec<RenderNode>>,
) -> Vec<NodeBucketSummary> {
    let mut summaries = Vec::with_capacity(buckets.len());
    let mut keys: Vec<&NodeBucketKey> = buckets.keys().collect();
    keys.sort_unstable();
    for nodes in keys.into_iter().map(|key| &buckets[key]) {
        let mut counts = [0usize; ProfessionKind::COUNT];
        let mut sum_x = [0.0; ProfessionKind::COUNT];
        let mut sum_z = [0.0; ProfessionKind::COUNT];

        for node in nodes {
            let index = node.profession.index();
            counts[index] += 1;
            sum_x[index] += node.x;
            sum_z[index] += node.z;
        }

        for profession in ProfessionKind::ALL {
            let index = profession.index();
            let count = counts[index];
            if count == 0 {
                continue;
            }
            summaries.push(NodeBucketSummary {
                x: sum_x[index] / count as f64,
                z: sum_z[index] / count as f64,
                profession,
                count,
            });
        }
    }
    summaries
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct NodeBucketKey {
    x: i32,
    z: i32,
}

impl NodeBucketKey {
    fn for_world_point(x: f64, z: f64) -> Self {
        Self {
            x: node_bucket_coord(x),
            z: node_bucket_coord(z),
        }
    }
}

fn node_bucket_coord(value: f64) -> i32 {
    (value / NODE_BUCKET_WORLD_SIZE).floor() as i32
}

#[derive(Debug)]
struct RenderNode {
    x: f64,
    z: f64,
    shape: NodeShape,
    profession: ProfessionKind,
    title: String,
    meta: String,
}

impl RenderNode {
    fn from_marker(node: GatheringNodeMarker) -> Self {
        let profession = resource_profession_kind(&node.resource);
        let node_type = title_label(&node.node_type);
        Self {
            x: node.location.x,
            z: node.location.z,
            shape: NodeShape::from_node_type(&node.node_type),
            profession,
            title: format!("{} Node", title_label(&node.resource)),
            meta: format!(
                "{} / {} / {}",
                profession.style().label,
                level_label(node.level),
                node_type
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum NodeShape {
    Dot,
    Corner,
    Wall,
}

impl NodeShape {
    fn from_node_type(value: &str) -> Self {
        match value {
            "CORNER" => Self::Corner,
            "WALL" => Self::Wall,
            _ => Self::Dot,
        }
    }

    fn marker_shape(self) -> MarkerShape {
        match self {
            Self::Dot => MarkerShape::Dot,
            Self::Corner => MarkerShape::Corner,
            Self::Wall => MarkerShape::Wall,
        }
    }
}

#[component]
pub(crate) fn MapIntelOverlay() -> impl IntoView {
    let MapIntelModeEnabled(enabled) = expect_context();
    let map = expect_context::<BrowserMap>();
    let camera = map.camera();
    let pointer = map.pointer();
    let IsMobile(is_mobile) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let SidebarWidth(sidebar_width) = expect_context();

    let data: RwSignal<Option<Rc<MapIntelModel>>, LocalStorage> = RwSignal::new_local(None);
    let loading: RwSignal<bool> = RwSignal::new(false);
    let error: RwSignal<Option<String>> = RwSignal::new(None);
    let retry_nonce: RwSignal<u64> = RwSignal::new(0);
    let canvas_ref = NodeRef::<leptos::html::Canvas>::new();
    let cached_ctx: Rc<RefCell<Option<CanvasRenderingContext2d>>> = Rc::new(RefCell::new(None));

    Effect::new(move || {
        retry_nonce.track();
        if !enabled.get() || data.with(|payload| payload.is_some()) || loading.get_untracked() {
            return;
        }
        loading.set(true);
        error.set(None);

        wasm_bindgen_futures::spawn_local(async move {
            let result = fetch_map_intel_overlay().await;
            match result {
                Ok(payload) => data.set(Some(Rc::new(MapIntelModel::from_payload(payload)))),
                Err(message) => {
                    error.set(Some(message));
                    loading.set(false);
                    gloo_timers::future::sleep(std::time::Duration::from_secs(
                        FETCH_RETRY_DELAY_SECS,
                    ))
                    .await;
                    if enabled.get_untracked() && data.with_untracked(|payload| payload.is_none()) {
                        retry_nonce.update(|nonce| *nonce = nonce.saturating_add(1));
                    }
                    return;
                }
            }
            loading.set(false);
        });
    });

    // The map renderer draws the markers. A frame drawn on this canvas clears its whole
    // DPR-sized surface (milliseconds in Firefox), so it only carries the site names and
    // stays untouched while it has none.
    Effect::new(move || {
        let markers = if enabled.get() {
            data.with(|payload| payload.as_ref().map(|payload| payload.markers.clone()))
        } else {
            None
        };
        map.markers().set(markers);
    });
    on_cleanup(move || {
        map.markers().try_set(None);
    });

    let labels_drawn = Rc::new(Cell::new(false));
    let scheduler = Rc::new(RenderScheduler::new({
        let cached_ctx = cached_ctx.clone();
        move || {
            let Some(canvas) = canvas_ref.get_untracked() else {
                return false;
            };
            let canvas: &HtmlCanvasElement = &canvas;
            let vp = camera.get_untracked();
            let shown = enabled.get_untracked();
            data.with_untracked(|payload| {
                let width = f64::from(canvas.client_width().max(1));
                let height = f64::from(canvas.client_height().max(1));
                let labels = payload
                    .as_ref()
                    .filter(|_| shown)
                    .map(|payload| site_labels(payload, &vp, width, height))
                    .unwrap_or_default();
                if labels.is_empty() && !labels_drawn.get() {
                    return;
                }
                let Some((ctx, width, height)) = canvas_context(canvas, &cached_ctx) else {
                    return;
                };
                ctx.clear_rect(0.0, 0.0, width, height);
                for label in &labels {
                    draw_label(&ctx, label.x, label.y, label.text, label.color);
                }
                labels_drawn.set(!labels.is_empty());
            });
            false
        }
    }));

    Effect::new({
        let scheduler = scheduler.clone();
        move || {
            enabled.track();
            camera.track();
            data.track();
            scheduler.mark_dirty();
        }
    });

    let hover = Memo::new(move |_| {
        if !enabled.get() {
            return None;
        }
        let (sx, sy) = pointer.get();
        let vp = camera.get();
        data.with(|payload| {
            payload
                .as_ref()
                .and_then(|payload| closest_hover(payload, &vp, sx, sy))
        })
    });

    view! {
        <canvas
            node_ref=canvas_ref
            style:display=move || if enabled.get() { "block" } else { "none" }
            style="position: absolute; inset: 0; width: 100%; height: 100%; z-index: 7; pointer-events: none;"
        />
        <Show when=move || enabled.get()>
            <div
                style:right=move || {
                    if !is_mobile.get() {
                        if sidebar_open.get() {
                            format!("{:.0}px", sidebar_width.get() + 16.0)
                        } else {
                            "64px".to_string()
                        }
                    } else {
                        "16px".to_string()
                    }
                }
                style="position: absolute; top: calc(var(--nav-height) + 16px); z-index: 9; pointer-events: none; width: 218px; padding: 9px 10px; border: 1px solid rgba(58,63,92,0.78); border-radius: 4px; background: rgba(19,22,31,0.92); box-shadow: 0 8px 24px rgba(0,0,0,0.34);"
            >
                <div style="display: flex; align-items: center; justify-content: space-between; gap: 8px; margin-bottom: 7px;">
                    <span style="font-family: var(--font-display); font-size: 0.64rem; letter-spacing: 0.12em; text-transform: uppercase; color: var(--color-text-secondary);">
                        "Map Intel"
                    </span>
                    <span style="font-family: var(--font-mono); font-size: 0.62rem; color: #6f748f;">
                        {move || data.with(|payload| map_intel_status(payload, loading.get(), error.get().as_deref()))}
                    </span>
                </div>
                <div style="display: grid; grid-template-columns: auto auto; gap: 4px 9px; align-items: center; margin-bottom: 8px;">
                    <LegendSwatch color="#c9a27d" shape="dot" label="Mining" />
                    <LegendSwatch color="#50c878" shape="dot" label="Woodcutting" />
                    <LegendSwatch color="#f5c542" shape="dot" label="Farming" />
                    <LegendSwatch color="#66c7f4" shape="dot" label="Fishing" />
                    <LegendSwatch color="#f5c542" shape="diamond" label="Events" />
                    <LegendSwatch color="#b18cff" shape="square" label="Raids" />
                    <LegendSwatch color="#5bd6c8" shape="triangle" label="Camps" />
                    <LegendSwatch color="#c7a3ff" shape="dot" label="Other" />
                </div>
                <div style="display: grid; grid-template-columns: repeat(4, 1fr); gap: 4px; border-top: 1px solid rgba(40,44,62,0.65); padding-top: 7px;">
                    <CountCell label="Nodes" value=move || data.with(|payload| payload.as_ref().map_or("-".to_string(), |payload| format_count(payload.gathering_nodes.len()))) />
                    <CountCell label="Events" value=move || data.with(|payload| payload.as_ref().map_or("-".to_string(), |payload| format_count(payload.world_events.len()))) />
                    <CountCell label="Raids" value=move || data.with(|payload| payload.as_ref().map_or("-".to_string(), |payload| format_count(payload.raids.len()))) />
                    <CountCell label="Camps" value=move || data.with(|payload| payload.as_ref().map_or("-".to_string(), |payload| format_count(payload.camps.len()))) />
                </div>
            </div>
            {move || {
                let Some(info) = hover.get() else {
                    return view! { <div style="display: none;" /> }.into_any();
                };
                let (canvas_w, canvas_h) = canvas_dimensions();
                let left = (info.screen_x + 14.0).clamp(8.0, (canvas_w - 238.0).max(8.0));
                let top = (info.screen_y + 14.0).clamp(8.0, (canvas_h - 74.0).max(8.0));
                view! {
                    <div style={format!(
                        "position: absolute; left: {left:.0}px; top: {top:.0}px; z-index: 10; pointer-events: none; width: 222px; padding: 8px 9px; border: 1px solid {color}; border-radius: 4px; background: rgba(12,14,23,0.94); box-shadow: 0 8px 22px rgba(0,0,0,0.36);",
                        color = alpha_border(info.color),
                    )}>
                        <div style="font-family: var(--font-display); font-size: 0.7rem; color: var(--color-text-primary); line-height: 1.25;">
                            {info.title}
                        </div>
                        <div style="font-family: var(--font-mono); font-size: 0.62rem; color: var(--color-text-secondary); margin-top: 3px; line-height: 1.25;">
                            {info.meta}
                        </div>
                    </div>
                }.into_any()
            }}
        </Show>
    }
}

#[component]
fn LegendSwatch(color: &'static str, shape: &'static str, label: &'static str) -> impl IntoView {
    let mark_style = match shape {
        "diamond" => format!(
            "width: 9px; height: 9px; background: {color}; transform: rotate(45deg); border: 1px solid rgba(255,255,255,0.22);"
        ),
        "square" => format!(
            "width: 9px; height: 9px; background: {color}; border: 1px solid rgba(255,255,255,0.22); border-radius: 2px;"
        ),
        "triangle" => format!(
            "width: 0; height: 0; border-left: 5px solid transparent; border-right: 5px solid transparent; border-bottom: 9px solid {color};"
        ),
        _ => format!(
            "width: 9px; height: 9px; border-radius: 50%; background: {color}; border: 1px solid rgba(255,255,255,0.22);"
        ),
    };

    view! {
        <div style="display: flex; align-items: center; gap: 6px; min-width: 0;">
            <span style=mark_style />
            <span style="font-family: var(--font-mono); font-size: 0.61rem; color: #d8d5cb; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;">
                {label}
            </span>
        </div>
    }
}

#[component]
fn CountCell<F>(label: &'static str, value: F) -> impl IntoView
where
    F: Fn() -> String + Copy + Send + Sync + 'static,
{
    view! {
        <div style="min-width: 0;">
            <div style="font-family: var(--font-mono); font-size: 0.68rem; color: var(--color-gold); line-height: 1.2; font-variant-numeric: tabular-nums;">
                {move || value()}
            </div>
            <div style="font-family: var(--font-mono); font-size: 0.53rem; color: #6f748f; text-transform: uppercase; line-height: 1.2; overflow: hidden; text-overflow: ellipsis;">
                {label}
            </div>
        </div>
    }
}

async fn fetch_map_intel_overlay() -> Result<MapIntelPayload, String> {
    let response = gloo_net::http::Request::get(MAP_INTEL_ENDPOINT)
        .send()
        .await
        .map_err(|error| format!("request failed: {error}"))?;
    if !response.ok() {
        return Err(format!("status {}", response.status()));
    }
    response
        .json::<MapIntelPayload>()
        .await
        .map_err(|error| format!("decode failed: {error}"))
}

pub(crate) fn canvas_context(
    canvas: &HtmlCanvasElement,
    cached_ctx: &Rc<RefCell<Option<CanvasRenderingContext2d>>>,
) -> Option<(CanvasRenderingContext2d, f64, f64)> {
    let width = canvas.client_width().max(1) as f64;
    let height = canvas.client_height().max(1) as f64;
    let scale = web_sys::window()
        .map(|window| window.device_pixel_ratio())
        .unwrap_or(1.0)
        .clamp(1.0, 3.0);
    let expected_width = (width * scale).round() as u32;
    let expected_height = (height * scale).round() as u32;

    if canvas.width() != expected_width || canvas.height() != expected_height {
        canvas.set_width(expected_width);
        canvas.set_height(expected_height);
        *cached_ctx.borrow_mut() = None;
    }

    let mut ctx_cache = cached_ctx.borrow_mut();
    if ctx_cache.is_none() {
        let ctx = canvas
            .get_context("2d")
            .ok()
            .flatten()?
            .dyn_into::<CanvasRenderingContext2d>()
            .ok()?;
        *ctx_cache = Some(ctx);
    }
    let ctx = ctx_cache.clone()?;
    ctx.set_transform(scale, 0.0, 0.0, scale, 0.0, 0.0).ok()?;
    Some((ctx, width, height))
}

struct SiteLabel<'a> {
    x: f64,
    y: f64,
    text: &'a str,
    color: &'static str,
}

/// Names of the events, raids and camps on screen, once zoomed in far enough.
fn site_labels<'a>(
    payload: &'a MapIntelModel,
    viewport: &Viewport,
    width: f64,
    height: f64,
) -> Vec<SiteLabel<'a>> {
    let mut labels = Vec::new();
    if viewport.scale <= SITE_LABEL_MIN_SCALE {
        return labels;
    }
    let mut push = |x: f64, z: f64, text: &'a str, color: &'static str| {
        let (sx, sy) = viewport.world_to_screen(x, z);
        if in_screen_bounds(sx, sy, width, height, 24.0) {
            labels.push(SiteLabel {
                x: sx + 9.0,
                y: sy - 7.0,
                text,
                color,
            });
        }
    };
    for event in &payload.world_events {
        for &(x, z) in &event.locations {
            push(x, z, &event.name, EVENT_COLOR);
        }
    }
    for raid in &payload.raids {
        push(raid.x, raid.z, &raid.name, RAID_COLOR);
    }
    for camp in &payload.camps {
        push(camp.x, camp.z, &camp.name, CAMP_COLOR);
    }
    labels
}

fn draw_label(ctx: &CanvasRenderingContext2d, x: f64, y: f64, label: &str, color: &str) {
    ctx.save();
    ctx.set_font("10px 'JetBrains Mono', monospace");
    ctx.set_shadow_color("rgba(0,0,0,0.85)");
    ctx.set_shadow_blur(4.0);
    ctx.set_fill_style_str(color);
    let _ = ctx.fill_text(label, x, y);
    ctx.restore();
}

#[derive(Clone, Copy)]
enum MarkerKind {
    Raid,
    Camp,
}

fn closest_hover(
    payload: &MapIntelModel,
    viewport: &Viewport,
    sx: f64,
    sy: f64,
) -> Option<IntelHover> {
    let mut best = closest_world_event(&payload.world_events, viewport, sx, sy);
    best = closer(
        best,
        closest_activity(&payload.raids, MarkerKind::Raid, viewport, sx, sy),
    );
    best = closer(
        best,
        closest_activity(&payload.camps, MarkerKind::Camp, viewport, sx, sy),
    );
    best = closer(
        best,
        closest_node(&payload.gathering_nodes, viewport, sx, sy),
    );
    best.map(|(_, hover)| hover)
}

fn closest_world_event(
    events: &[RenderWorldEvent],
    viewport: &Viewport,
    sx: f64,
    sy: f64,
) -> Option<(f64, IntelHover)> {
    let mut best = None;
    for event in events {
        for (x, z) in &event.locations {
            let (mx, my) = viewport.world_to_screen(*x, *z);
            let dist = distance_sq(sx, sy, mx, my);
            if dist <= 14.0 * 14.0 && best.as_ref().is_none_or(|(current, _)| dist < *current) {
                best = Some((
                    dist,
                    IntelHover {
                        screen_x: mx,
                        screen_y: my,
                        title: event.name.clone(),
                        meta: event.meta.clone(),
                        color: EVENT_COLOR,
                    },
                ));
            }
        }
    }
    best
}

fn closest_activity(
    entries: &[RenderActivity],
    kind: MarkerKind,
    viewport: &Viewport,
    sx: f64,
    sy: f64,
) -> Option<(f64, IntelHover)> {
    let mut best = None;
    for entry in entries {
        let (mx, my) = viewport.world_to_screen(entry.x, entry.z);
        let dist = distance_sq(sx, sy, mx, my);
        if dist <= 14.0 * 14.0 && best.as_ref().is_none_or(|(current, _)| dist < *current) {
            let color = match kind {
                MarkerKind::Raid => RAID_COLOR,
                MarkerKind::Camp => CAMP_COLOR,
            };
            best = Some((
                dist,
                IntelHover {
                    screen_x: mx,
                    screen_y: my,
                    title: entry.name.clone(),
                    meta: entry.meta.clone(),
                    color,
                },
            ));
        }
    }
    best
}

fn closest_node(
    nodes: &NodeIndex,
    viewport: &Viewport,
    sx: f64,
    sy: f64,
) -> Option<(f64, IntelHover)> {
    if viewport.scale < NODE_CLUSTER_SCALE {
        return closest_node_summary(nodes, viewport, sx, sy);
    }

    let threshold = (node_radius(viewport.scale) + 4.5).max(6.5);
    let threshold_sq = threshold * threshold;
    let mut best = None;
    let (wx, wz) = viewport.screen_to_world(sx, sy);
    let world_radius = threshold / viewport.scale.max(0.001);

    nodes.for_each_in_world_bounds(
        wx - world_radius,
        wz - world_radius,
        wx + world_radius,
        wz + world_radius,
        |node| {
            let (mx, my) = viewport.world_to_screen(node.x, node.z);
            let dist = distance_sq(sx, sy, mx, my);
            if dist <= threshold_sq && best.as_ref().is_none_or(|(current, _)| dist < *current) {
                best = Some((
                    dist,
                    IntelHover {
                        screen_x: mx,
                        screen_y: my,
                        title: node.title.clone(),
                        meta: node.meta.clone(),
                        color: node.profession.style().color,
                    },
                ));
            }
        },
    );
    best
}

fn closest_node_summary(
    nodes: &NodeIndex,
    viewport: &Viewport,
    sx: f64,
    sy: f64,
) -> Option<(f64, IntelHover)> {
    let threshold = 8.0;
    let threshold_sq = threshold * threshold;
    let mut best = None;

    for summary in &nodes.summaries {
        let (mx, my) = viewport.world_to_screen(summary.x, summary.z);
        let dist = distance_sq(sx, sy, mx, my);
        if dist <= threshold_sq && best.as_ref().is_none_or(|(current, _)| dist < *current) {
            let style = summary.profession.style();
            best = Some((
                dist,
                IntelHover {
                    screen_x: mx,
                    screen_y: my,
                    title: format!("{} Nodes", style.label),
                    meta: format!(
                        "{} grouped {}",
                        summary.count,
                        node_count_label(summary.count)
                    ),
                    color: style.color,
                },
            ));
        }
    }

    best
}

fn closer(
    left: Option<(f64, IntelHover)>,
    right: Option<(f64, IntelHover)>,
) -> Option<(f64, IntelHover)> {
    match (left, right) {
        (Some(left), Some(right)) => Some(if right.0 < left.0 { right } else { left }),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn distance_sq(ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let dx = ax - bx;
    let dy = ay - by;
    dx * dx + dy * dy
}

pub(crate) fn in_screen_bounds(x: f64, y: f64, width: f64, height: f64, margin: f64) -> bool {
    x >= -margin && y >= -margin && x <= width + margin && y <= height + margin
}

fn event_meta(event: &WorldEventMarker) -> String {
    let level = level_label(event.level);
    let schedule = event.schedule.as_deref().unwrap_or("unscheduled");
    format!(
        "World event / {} / {} / {}",
        level,
        clean_meta(event.difficulty.as_deref()),
        schedule
    )
}

fn activity_meta(kind: &str, entry: &MapActivityMarker) -> String {
    format!(
        "{} / {} / {}",
        kind,
        level_label(entry.level),
        clean_meta(entry.difficulty.as_deref())
    )
}

fn level_label(level: Option<i32>) -> String {
    level.map_or_else(|| "Any level".to_string(), |level| format!("Level {level}"))
}

fn clean_meta(value: Option<&str>) -> String {
    value
        .map(title_label)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Unknown".to_string())
}

fn title_label(value: &str) -> String {
    value
        .split_whitespace()
        .map(|part| {
            let mut chars = part.chars();
            let Some(first) = chars.next() else {
                return String::new();
            };
            let mut label = first.to_uppercase().collect::<String>();
            label.push_str(&chars.as_str().to_ascii_lowercase());
            label
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn map_intel_status(
    payload: &Option<Rc<MapIntelModel>>,
    loading: bool,
    error: Option<&str>,
) -> String {
    if loading {
        return "Loading".to_string();
    }
    if error.is_some() {
        return "Retrying".to_string();
    }
    payload
        .as_ref()
        .map(|payload| format_count(payload.total_markers()))
        .unwrap_or_else(|| "-".to_string())
}

fn format_count(value: usize) -> String {
    if value >= 1000 {
        format!("{:.1}k", value as f64 / 1000.0)
    } else {
        value.to_string()
    }
}

fn node_count_label(count: usize) -> &'static str {
    if count == 1 { "node" } else { "nodes" }
}

#[derive(Clone, Copy)]
struct ProfessionStyle {
    label: &'static str,
    color: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ProfessionKind {
    Mining,
    Woodcutting,
    Farming,
    Fishing,
    Other,
}

impl ProfessionKind {
    const ALL: [Self; Self::COUNT] = [
        Self::Mining,
        Self::Woodcutting,
        Self::Farming,
        Self::Fishing,
        Self::Other,
    ];
    const COUNT: usize = 5;

    fn index(self) -> usize {
        match self {
            Self::Mining => 0,
            Self::Woodcutting => 1,
            Self::Farming => 2,
            Self::Fishing => 3,
            Self::Other => 4,
        }
    }

    fn style(self) -> ProfessionStyle {
        match self {
            Self::Mining => ProfessionStyle {
                label: "Mining",
                color: "#c9a27d",
            },
            Self::Woodcutting => ProfessionStyle {
                label: "Woodcutting",
                color: "#50c878",
            },
            Self::Farming => ProfessionStyle {
                label: "Farming",
                color: "#f5c542",
            },
            Self::Fishing => ProfessionStyle {
                label: "Fishing",
                color: "#66c7f4",
            },
            Self::Other => ProfessionStyle {
                label: "Other",
                color: "#c7a3ff",
            },
        }
    }
}

fn resource_profession_kind(resource: &str) -> ProfessionKind {
    let resource = resource.trim().to_ascii_uppercase();
    if MINING_RESOURCES.contains(&resource.as_str()) {
        ProfessionKind::Mining
    } else if WOODCUTTING_RESOURCES.contains(&resource.as_str()) {
        ProfessionKind::Woodcutting
    } else if FARMING_RESOURCES.contains(&resource.as_str()) {
        ProfessionKind::Farming
    } else if FISHING_RESOURCES.contains(&resource.as_str()) {
        ProfessionKind::Fishing
    } else {
        ProfessionKind::Other
    }
}

/// `#rrggbb` as bytes; the overlay's colours are all of that form.
fn hex_rgb(color: &str) -> [u8; 3] {
    let channel = |at: usize| {
        color
            .get(at..at + 2)
            .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            .unwrap_or(0)
    };
    [channel(1), channel(3), channel(5)]
}

fn alpha_border(color: &str) -> String {
    match color {
        "#c9a27d" => "rgba(201,162,125,0.62)".to_string(),
        "#50c878" => "rgba(80,200,120,0.62)".to_string(),
        "#f5c542" => "rgba(245,197,66,0.62)".to_string(),
        "#66c7f4" => "rgba(102,199,244,0.62)".to_string(),
        "#b18cff" => "rgba(177,140,255,0.62)".to_string(),
        "#5bd6c8" => "rgba(91,214,200,0.62)".to_string(),
        _ => "rgba(199,163,255,0.62)".to_string(),
    }
}

const MINING_RESOURCES: &[&str] = &[
    "COPPER",
    "GRANITE",
    "GOLD",
    "SANDSTONE",
    "IRON",
    "SILVER",
    "COBALT",
    "KANDERSTONE",
    "DIAMOND",
    "MOLTEN",
    "TITANIUM",
    "VOIDSTONE",
    "DERNIC",
    "CINNABAR",
    "GYLIA",
    "DECAY",
    "HEATHER",
];

const WOODCUTTING_RESOURCES: &[&str] = &[
    "OAK", "BIRCH", "WILLOW", "ACACIA", "SPRUCE", "JUNGLE", "DARK", "LIGHT", "SKY", "MAPLE",
    "REDWOOD",
];

const FARMING_RESOURCES: &[&str] = &[
    "WHEAT", "BARLEY", "OAT", "MALT", "HOPS", "RYE", "MILLET", "RICE", "SORGUM", "SORGHUM", "HEMP",
];

const FISHING_RESOURCES: &[&str] = &[
    "GUDGEON", "TROUT", "SALMON", "CARP", "KOI", "PIRANHA", "AVO", "MAHSEER", "BASS", "STARFISH",
    "ICEFISH", "STURGEON",
];

#[cfg(test)]
mod tests {
    use sequoia_map_engine::map_markers::MarkerShape;
    use sequoia_shared::{
        GatheringNodeMarker, MapActivityMarker, MapIntelOverlay, MapPoint, WorldEventMarker,
    };

    use crate::viewport::Viewport;

    use super::{
        MapIntelModel, NodeIndex, closest_node, format_count, hex_rgb, resource_profession_kind,
        site_labels, title_label,
    };

    fn node_marker(x: f64, z: f64, resource: &str) -> GatheringNodeMarker {
        GatheringNodeMarker {
            location: MapPoint { x, y: 0.0, z },
            node_type: "NODE".to_string(),
            resource: resource.to_string(),
            level: Some(1),
            angle: None,
        }
    }

    fn point(x: f64, z: f64) -> MapPoint {
        MapPoint { x, y: 0.0, z }
    }

    fn payload() -> MapIntelOverlay {
        let mut corner = node_marker(300.0, 0.0, "OAK");
        corner.node_type = "CORNER".to_string();
        let mut wall = node_marker(10.0, 0.0, "COPPER");
        wall.node_type = "WALL".to_string();
        let site = |name: &str, x: f64| MapActivityMarker {
            name: name.to_string(),
            location: point(x, 40.0),
            ..Default::default()
        };
        MapIntelOverlay {
            generated_at: String::new(),
            source: String::new(),
            raids: vec![site("Raid", 100.0)],
            camps: vec![site("Camp", 200.0)],
            world_events: vec![WorldEventMarker {
                name: "Event".to_string(),
                locations: vec![point(0.0, 40.0), point(50.0, 40.0)],
                ..Default::default()
            }],
            gathering_nodes: vec![
                node_marker(0.0, 0.0, "COPPER"),
                corner,
                wall,
                node_marker(20.0, 0.0, "WHEAT"),
            ],
            gathering_resources: Vec::new(),
            gathering_node_types: Vec::new(),
        }
    }

    #[test]
    fn map_markers_carry_every_payload_marker() {
        let model = MapIntelModel::from_payload(payload());
        let markers = &model.markers;

        // Nodes area by area, each area by profession then shape.
        let nodes: Vec<_> = markers
            .nodes()
            .iter()
            .map(|marker| (marker.world, marker.shape, marker.rgb))
            .collect();
        assert_eq!(
            nodes,
            [
                ([0.0, 0.0], MarkerShape::Dot, hex_rgb("#c9a27d")),
                ([10.0, 0.0], MarkerShape::Wall, hex_rgb("#c9a27d")),
                ([20.0, 0.0], MarkerShape::Dot, hex_rgb("#f5c542")),
                ([300.0, 0.0], MarkerShape::Corner, hex_rgb("#50c878")),
            ]
        );
        // One summary per area and profession, at the nodes' mean position.
        let summaries: Vec<_> = markers
            .summaries()
            .iter()
            .map(|marker| (marker.world, marker.shape))
            .collect();
        assert_eq!(
            summaries,
            [
                ([5.0, 0.0], MarkerShape::Summary),
                ([20.0, 0.0], MarkerShape::Summary),
                ([300.0, 0.0], MarkerShape::Summary),
            ]
        );
        // Every event location, then raids, then camps.
        let sites: Vec<_> = markers
            .sites()
            .iter()
            .map(|marker| (marker.world[0], marker.shape, marker.rgb))
            .collect();
        assert_eq!(
            sites,
            [
                (0.0, MarkerShape::Event, hex_rgb("#f5c542")),
                (50.0, MarkerShape::Event, hex_rgb("#f5c542")),
                (100.0, MarkerShape::Raid, hex_rgb("#b18cff")),
                (200.0, MarkerShape::Camp, hex_rgb("#5bd6c8")),
            ]
        );
    }

    #[test]
    fn site_names_show_zoomed_in_and_on_screen() {
        let model = MapIntelModel::from_payload(payload());
        let at = |scale: f64| Viewport {
            offset_x: 0.0,
            offset_y: 0.0,
            scale,
        };
        assert!(site_labels(&model, &at(0.58), 400.0, 300.0).is_empty());

        let labels = site_labels(&model, &at(1.0), 120.0, 300.0);
        let names: Vec<_> = labels.iter().map(|label| (label.text, label.x)).collect();
        // The camp at x = 200 is off this 120 px wide screen.
        assert_eq!(names, [("Event", 9.0), ("Event", 59.0), ("Raid", 109.0)]);
        assert_eq!(labels[0].y, 33.0);
    }

    #[test]
    fn parses_overlay_colours() {
        assert_eq!(hex_rgb("#c9a27d"), [0xc9, 0xa2, 0x7d]);
        assert_eq!(hex_rgb("#5bd6c8"), [0x5b, 0xd6, 0xc8]);
    }

    #[test]
    fn formats_compact_counts() {
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(16_787), "16.8k");
    }

    #[test]
    fn classifies_common_profession_resources() {
        assert_eq!(resource_profession_kind("COPPER").style().label, "Mining");
        assert_eq!(resource_profession_kind("OAK").style().label, "Woodcutting");
        assert_eq!(resource_profession_kind("WHEAT").style().label, "Farming");
        assert_eq!(resource_profession_kind("BASS").style().label, "Fishing");
    }

    #[test]
    fn title_cases_api_labels() {
        assert_eq!(title_label("VERY HIGH"), "Very High");
        assert_eq!(title_label("node"), "Node");
    }

    #[test]
    fn node_index_filters_by_world_bounds() {
        let index = NodeIndex::from_markers(vec![
            node_marker(0.0, 0.0, "COPPER"),
            node_marker(600.0, 0.0, "OAK"),
        ]);
        let mut titles = Vec::new();

        index.for_each_in_world_bounds(-10.0, -10.0, 10.0, 10.0, |node| {
            titles.push(node.title.clone());
        });

        assert_eq!(titles, ["Copper Node"]);
    }

    #[test]
    fn node_index_summarizes_each_profession_per_bucket() {
        let index = NodeIndex::from_markers(vec![
            node_marker(0.0, 0.0, "COPPER"),
            node_marker(10.0, 0.0, "GRANITE"),
            node_marker(12.0, 0.0, "OAK"),
        ]);
        let labels = index
            .summaries
            .iter()
            .map(|summary| (summary.profession.style().label, summary.count))
            .collect::<Vec<_>>();

        assert_eq!(labels, [("Mining", 2), ("Woodcutting", 1)]);
    }

    #[test]
    fn clustered_node_hover_uses_summary_marker() {
        let index = NodeIndex::from_markers(vec![
            node_marker(0.0, 0.0, "COPPER"),
            node_marker(200.0, 0.0, "GRANITE"),
        ]);
        let viewport = Viewport {
            offset_x: 0.0,
            offset_y: 0.0,
            scale: 0.1,
        };

        let (_, hover) = closest_node(&index, &viewport, 10.0, 0.0).expect("summary hover");

        assert_eq!(hover.title, "Mining Nodes");
        assert_eq!(hover.meta, "2 grouped nodes");
    }
}
