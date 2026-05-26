use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;
use sequoia_shared::{
    GatheringNodeMarker, MapActivityMarker, MapIntelOverlay as MapIntelPayload, WorldEventMarker,
};
use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

use crate::app::{IsMobile, MapIntelModeEnabled, SidebarOpen, SidebarWidth, canvas_dimensions};
use crate::render_loop::RenderScheduler;
use crate::viewport::Viewport;

const NODE_MIN_RADIUS: f64 = 1.25;
const NODE_MAX_RADIUS: f64 = 3.25;
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

#[component]
pub(crate) fn MapIntelOverlay() -> impl IntoView {
    let MapIntelModeEnabled(enabled) = expect_context();
    let viewport: RwSignal<Viewport> = expect_context();
    let mouse_pos: RwSignal<(f64, f64)> = expect_context();
    let IsMobile(is_mobile) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let SidebarWidth(sidebar_width) = expect_context();

    let data: RwSignal<Option<MapIntelPayload>> = RwSignal::new(None);
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
                Ok(payload) => data.set(Some(payload)),
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

    let scheduler = Rc::new(RenderScheduler::new({
        let cached_ctx = cached_ctx.clone();
        move || {
            let Some(canvas) = canvas_ref.get_untracked() else {
                return false;
            };
            let canvas: &HtmlCanvasElement = &canvas;
            let Some((ctx, width, height)) = canvas_context(canvas, &cached_ctx) else {
                return false;
            };

            ctx.clear_rect(0.0, 0.0, width, height);
            if enabled.get_untracked() {
                let vp = viewport.get_untracked();
                data.with_untracked(|payload| {
                    if let Some(payload) = payload.as_ref() {
                        draw_payload(&ctx, &vp, payload, width, height);
                    }
                });
            }
            false
        }
    }));

    Effect::new({
        let scheduler = scheduler.clone();
        move || {
            enabled.track();
            viewport.track();
            data.track();
            scheduler.mark_dirty();
        }
    });

    let hover = Memo::new(move |_| {
        if !enabled.get() {
            return None;
        }
        let (sx, sy) = mouse_pos.get();
        let vp = viewport.get();
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
                style="position: absolute; top: 16px; z-index: 9; pointer-events: none; width: 218px; padding: 9px 10px; border: 1px solid rgba(58,63,92,0.78); border-radius: 4px; background: rgba(19,22,31,0.92); box-shadow: 0 8px 24px rgba(0,0,0,0.34);"
            >
                <div style="display: flex; align-items: center; justify-content: space-between; gap: 8px; margin-bottom: 7px;">
                    <span style="font-family: 'Silkscreen', monospace; font-size: 0.64rem; letter-spacing: 0.12em; text-transform: uppercase; color: #9a9590;">
                        "Map Intel"
                    </span>
                    <span style="font-family: 'JetBrains Mono', monospace; font-size: 0.62rem; color: #6f748f;">
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
                        <div style="font-family: 'Silkscreen', monospace; font-size: 0.7rem; color: #e2e0d8; line-height: 1.25;">
                            {info.title}
                        </div>
                        <div style="font-family: 'JetBrains Mono', monospace; font-size: 0.62rem; color: #9a9590; margin-top: 3px; line-height: 1.25;">
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
            <span style="font-family: 'JetBrains Mono', monospace; font-size: 0.61rem; color: #d8d5cb; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;">
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
            <div style="font-family: 'JetBrains Mono', monospace; font-size: 0.68rem; color: #f5c542; line-height: 1.2; font-variant-numeric: tabular-nums;">
                {move || value()}
            </div>
            <div style="font-family: 'JetBrains Mono', monospace; font-size: 0.53rem; color: #6f748f; text-transform: uppercase; line-height: 1.2; overflow: hidden; text-overflow: ellipsis;">
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

fn canvas_context(
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

fn draw_payload(
    ctx: &CanvasRenderingContext2d,
    viewport: &Viewport,
    payload: &MapIntelPayload,
    width: f64,
    height: f64,
) {
    draw_nodes(ctx, viewport, &payload.gathering_nodes, width, height);
    draw_world_events(ctx, viewport, &payload.world_events, width, height);
    draw_activities(
        ctx,
        viewport,
        &payload.raids,
        MarkerKind::Raid,
        width,
        height,
    );
    draw_activities(
        ctx,
        viewport,
        &payload.camps,
        MarkerKind::Camp,
        width,
        height,
    );
}

fn draw_nodes(
    ctx: &CanvasRenderingContext2d,
    viewport: &Viewport,
    nodes: &[GatheringNodeMarker],
    width: f64,
    height: f64,
) {
    let radius = node_radius(viewport.scale);
    for node in nodes {
        let (sx, sy) = viewport.world_to_screen(node.location.x, node.location.z);
        if !in_screen_bounds(sx, sy, width, height, 18.0) {
            continue;
        }

        let color = resource_color(&node.resource);
        ctx.set_fill_style_str(color);
        match node.node_type.as_str() {
            "CORNER" => ctx.fill_rect(sx - radius, sy - radius, radius * 2.0, radius * 2.0),
            "WALL" => {
                ctx.fill_rect(sx - radius, sy - radius * 0.45, radius * 2.0, radius * 0.9);
                ctx.fill_rect(sx - radius * 0.45, sy - radius, radius * 0.9, radius * 2.0);
            }
            _ => {
                ctx.begin_path();
                ctx.arc(sx, sy, radius, 0.0, std::f64::consts::TAU).ok();
                ctx.fill();
            }
        }
    }
}

fn draw_world_events(
    ctx: &CanvasRenderingContext2d,
    viewport: &Viewport,
    events: &[WorldEventMarker],
    width: f64,
    height: f64,
) {
    for event in events {
        for location in &event.locations {
            let (sx, sy) = viewport.world_to_screen(location.x, location.z);
            if !in_screen_bounds(sx, sy, width, height, 24.0) {
                continue;
            }
            draw_diamond(ctx, sx, sy, 6.0, "#f5c542", "rgba(12,14,23,0.9)");
            if viewport.scale > 0.58 {
                draw_label(ctx, sx + 9.0, sy - 7.0, &event.name, "#f5c542");
            }
        }
    }
}

#[derive(Clone, Copy)]
enum MarkerKind {
    Raid,
    Camp,
}

fn draw_activities(
    ctx: &CanvasRenderingContext2d,
    viewport: &Viewport,
    entries: &[MapActivityMarker],
    kind: MarkerKind,
    width: f64,
    height: f64,
) {
    for entry in entries {
        let (sx, sy) = viewport.world_to_screen(entry.location.x, entry.location.z);
        if !in_screen_bounds(sx, sy, width, height, 24.0) {
            continue;
        }
        match kind {
            MarkerKind::Raid => {
                draw_square(ctx, sx, sy, 6.0, "#b18cff", "rgba(12,14,23,0.9)");
                if viewport.scale > 0.58 {
                    draw_label(ctx, sx + 9.0, sy - 7.0, &entry.name, "#b18cff");
                }
            }
            MarkerKind::Camp => {
                draw_triangle(ctx, sx, sy, 7.0, "#5bd6c8", "rgba(12,14,23,0.9)");
                if viewport.scale > 0.58 {
                    draw_label(ctx, sx + 9.0, sy - 7.0, &entry.name, "#5bd6c8");
                }
            }
        }
    }
}

fn draw_diamond(
    ctx: &CanvasRenderingContext2d,
    x: f64,
    y: f64,
    radius: f64,
    fill: &str,
    stroke: &str,
) {
    ctx.begin_path();
    ctx.move_to(x, y - radius);
    ctx.line_to(x + radius, y);
    ctx.line_to(x, y + radius);
    ctx.line_to(x - radius, y);
    ctx.close_path();
    ctx.set_fill_style_str(fill);
    ctx.fill();
    ctx.set_stroke_style_str(stroke);
    ctx.set_line_width(2.0);
    ctx.stroke();
}

fn draw_square(
    ctx: &CanvasRenderingContext2d,
    x: f64,
    y: f64,
    radius: f64,
    fill: &str,
    stroke: &str,
) {
    ctx.set_fill_style_str(fill);
    ctx.fill_rect(x - radius, y - radius, radius * 2.0, radius * 2.0);
    ctx.set_stroke_style_str(stroke);
    ctx.set_line_width(2.0);
    ctx.stroke_rect(x - radius, y - radius, radius * 2.0, radius * 2.0);
}

fn draw_triangle(
    ctx: &CanvasRenderingContext2d,
    x: f64,
    y: f64,
    radius: f64,
    fill: &str,
    stroke: &str,
) {
    ctx.begin_path();
    ctx.move_to(x, y - radius);
    ctx.line_to(x + radius, y + radius * 0.85);
    ctx.line_to(x - radius, y + radius * 0.85);
    ctx.close_path();
    ctx.set_fill_style_str(fill);
    ctx.fill();
    ctx.set_stroke_style_str(stroke);
    ctx.set_line_width(2.0);
    ctx.stroke();
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

fn closest_hover(
    payload: &MapIntelPayload,
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
    events: &[WorldEventMarker],
    viewport: &Viewport,
    sx: f64,
    sy: f64,
) -> Option<(f64, IntelHover)> {
    let mut best = None;
    for event in events {
        for location in &event.locations {
            let (mx, my) = viewport.world_to_screen(location.x, location.z);
            let dist = distance_sq(sx, sy, mx, my);
            if dist <= 14.0 * 14.0 && best.as_ref().is_none_or(|(current, _)| dist < *current) {
                best = Some((
                    dist,
                    IntelHover {
                        screen_x: mx,
                        screen_y: my,
                        title: event.name.clone(),
                        meta: event_meta(event),
                        color: "#f5c542",
                    },
                ));
            }
        }
    }
    best
}

fn closest_activity(
    entries: &[MapActivityMarker],
    kind: MarkerKind,
    viewport: &Viewport,
    sx: f64,
    sy: f64,
) -> Option<(f64, IntelHover)> {
    let mut best = None;
    for entry in entries {
        let (mx, my) = viewport.world_to_screen(entry.location.x, entry.location.z);
        let dist = distance_sq(sx, sy, mx, my);
        if dist <= 14.0 * 14.0 && best.as_ref().is_none_or(|(current, _)| dist < *current) {
            let (kind_label, color) = match kind {
                MarkerKind::Raid => ("Raid", "#b18cff"),
                MarkerKind::Camp => ("Camp", "#5bd6c8"),
            };
            best = Some((
                dist,
                IntelHover {
                    screen_x: mx,
                    screen_y: my,
                    title: entry.name.clone(),
                    meta: activity_meta(kind_label, entry),
                    color,
                },
            ));
        }
    }
    best
}

fn closest_node(
    nodes: &[GatheringNodeMarker],
    viewport: &Viewport,
    sx: f64,
    sy: f64,
) -> Option<(f64, IntelHover)> {
    let threshold = (node_radius(viewport.scale) + 4.5).max(6.5);
    let threshold_sq = threshold * threshold;
    let mut best = None;
    for node in nodes {
        let (mx, my) = viewport.world_to_screen(node.location.x, node.location.z);
        let dist = distance_sq(sx, sy, mx, my);
        if dist <= threshold_sq && best.as_ref().is_none_or(|(current, _)| dist < *current) {
            let profession = resource_profession(&node.resource);
            best = Some((
                dist,
                IntelHover {
                    screen_x: mx,
                    screen_y: my,
                    title: format!("{} Node", title_label(&node.resource)),
                    meta: node_meta(profession.label, node),
                    color: profession.color,
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

fn node_radius(scale: f64) -> f64 {
    (1.4 + scale * 0.9).clamp(NODE_MIN_RADIUS, NODE_MAX_RADIUS)
}

fn in_screen_bounds(x: f64, y: f64, width: f64, height: f64, margin: f64) -> bool {
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

fn node_meta(profession: &str, node: &GatheringNodeMarker) -> String {
    format!(
        "{} / {} / {}",
        profession,
        level_label(node.level),
        title_label(&node.node_type)
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
    payload: &Option<MapIntelPayload>,
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
        .map(|payload| format_count(total_markers(payload)))
        .unwrap_or_else(|| "-".to_string())
}

fn total_markers(payload: &MapIntelPayload) -> usize {
    payload.gathering_nodes.len()
        + payload.world_events.len()
        + payload.raids.len()
        + payload.camps.len()
}

fn format_count(value: usize) -> String {
    if value >= 1000 {
        format!("{:.1}k", value as f64 / 1000.0)
    } else {
        value.to_string()
    }
}

fn resource_color(resource: &str) -> &'static str {
    resource_profession(resource).color
}

#[derive(Clone, Copy)]
struct ProfessionStyle {
    label: &'static str,
    color: &'static str,
}

fn resource_profession(resource: &str) -> ProfessionStyle {
    let resource = resource.trim().to_ascii_uppercase();
    if MINING_RESOURCES.contains(&resource.as_str()) {
        ProfessionStyle {
            label: "Mining",
            color: "#c9a27d",
        }
    } else if WOODCUTTING_RESOURCES.contains(&resource.as_str()) {
        ProfessionStyle {
            label: "Woodcutting",
            color: "#50c878",
        }
    } else if FARMING_RESOURCES.contains(&resource.as_str()) {
        ProfessionStyle {
            label: "Farming",
            color: "#f5c542",
        }
    } else if FISHING_RESOURCES.contains(&resource.as_str()) {
        ProfessionStyle {
            label: "Fishing",
            color: "#66c7f4",
        }
    } else {
        ProfessionStyle {
            label: "Other",
            color: "#c7a3ff",
        }
    }
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
    use super::{format_count, resource_profession, title_label};

    #[test]
    fn formats_compact_counts() {
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(16_787), "16.8k");
    }

    #[test]
    fn classifies_common_profession_resources() {
        assert_eq!(resource_profession("COPPER").label, "Mining");
        assert_eq!(resource_profession("OAK").label, "Woodcutting");
        assert_eq!(resource_profession("WHEAT").label, "Farming");
        assert_eq!(resource_profession("BASS").label, "Fishing");
    }

    #[test]
    fn title_cases_api_labels() {
        assert_eq!(title_label("VERY HIGH"), "Very High");
        assert_eq!(title_label("node"), "Node");
    }
}
