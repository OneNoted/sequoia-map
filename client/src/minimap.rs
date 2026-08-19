use std::cell::{Cell, RefCell};
use std::rc::Rc;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, MouseEvent};

use crate::app::{
    CurrentMode, HeatEntriesByTerritory, HeatMaxTakeCount, HeatModeEnabled, IsMobile, MapMode,
    Selected, SidebarOpen, canvas_dimensions,
};
use crate::heat::heat_color_for_count;
use crate::canvas::render_scale;
use crate::render_loop::RenderScheduler;
use crate::territory::ClientTerritoryMap;
use crate::tiles::LoadedTile;
use crate::viewport::Viewport;

const MINIMAP_W: f64 = 200.0;
const MINIMAP_H: f64 = 280.0;

// World bounds (Wynncraft territory range)
const WORLD_MIN_X: f64 = -2200.0;
const WORLD_MAX_X: f64 = 1600.0;
const WORLD_MIN_Y: f64 = -6600.0;
const WORLD_MAX_Y: f64 = 400.0;

/// Offscreen canvas cache for the territory layer (tiles + territories + selection).
/// Only redrawn when territories/tiles/selection change, not on every viewport pan/zoom.
struct OffscreenCache {
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
}

impl OffscreenCache {
    fn new() -> Option<Self> {
        let document = web_sys::window()?.document()?;
        let canvas = document
            .create_element("canvas")
            .ok()?
            .dyn_into::<HtmlCanvasElement>()
            .ok()?;
        let scale = render_scale();
        canvas.set_width((MINIMAP_W * scale) as u32);
        canvas.set_height((MINIMAP_H * scale) as u32);
        let ctx = canvas
            .get_context("2d")
            .ok()??
            .dyn_into::<CanvasRenderingContext2d>()
            .ok()?;
        ctx.scale(scale, scale).ok();
        Some(Self { canvas, ctx })
    }

    fn redraw(
        &self,
        territories: &ClientTerritoryMap,
        tiles: &[LoadedTile],
        selected: &Option<String>,
        heat_mode_enabled: bool,
        heat_entries: &std::collections::HashMap<String, u64>,
        heat_max_take_count: u64,
    ) {
        let world_w = WORLD_MAX_X - WORLD_MIN_X;
        let world_h = WORLD_MAX_Y - WORLD_MIN_Y;
        let ctx = &self.ctx;

        // Clear
        ctx.set_fill_style_str("#13161f");
        ctx.fill_rect(0.0, 0.0, MINIMAP_W, MINIMAP_H);

        // Draw map tile images
        for tile in tiles {
            let x1 = tile.x1.min(tile.x2) as f64;
            let z1 = tile.z1.min(tile.z2) as f64;
            // Tile bounds are inclusive — add 1 to get exclusive far edge
            let x2 = tile.x1.max(tile.x2) as f64 + 1.0;
            let z2 = tile.z1.max(tile.z2) as f64 + 1.0;

            let mx = ((x1 - WORLD_MIN_X) / world_w) * MINIMAP_W;
            let my = ((z1 - WORLD_MIN_Y) / world_h) * MINIMAP_H;
            let mw = ((x2 - x1) / world_w) * MINIMAP_W;
            let mh = ((z2 - z1) / world_h) * MINIMAP_H;

            ctx.draw_image_with_html_image_element_and_dw_and_dh(&tile.image, mx, my, mw, mh)
                .ok();
        }

        // Dark wash over tiles
        if !tiles.is_empty() {
            ctx.set_fill_style_str("rgba(12, 14, 23, 0.22)");
            ctx.fill_rect(0.0, 0.0, MINIMAP_W, MINIMAP_H);
        }

        // Draw territories
        for (name, ct) in territories {
            let loc = &ct.territory.location;
            let x = ((loc.left() as f64 - WORLD_MIN_X) / world_w) * MINIMAP_W;
            let y = ((loc.top() as f64 - WORLD_MIN_Y) / world_h) * MINIMAP_H;
            let w = (loc.width() as f64 / world_w) * MINIMAP_W;
            let h = (loc.height() as f64 / world_h) * MINIMAP_H;

            if heat_mode_enabled {
                let take_count = heat_entries.get(name).copied().unwrap_or(0);
                let (r, g, b) = heat_color_for_count(take_count, heat_max_take_count);
                ctx.set_fill_style_str(&format!("rgba({r}, {g}, {b}, 0.45)"));
            } else {
                ctx.set_fill_style_str(&ct.cached_colors.minimap_fill);
            }
            ctx.fill_rect(x, y, w.max(1.0), h.max(1.0));
        }

        // Draw selected territory highlight
        if let Some(sel_name) = selected
            && let Some(ct) = territories.get(sel_name)
        {
            let loc = &ct.territory.location;
            let x = ((loc.left() as f64 - WORLD_MIN_X) / world_w) * MINIMAP_W;
            let y = ((loc.top() as f64 - WORLD_MIN_Y) / world_h) * MINIMAP_H;
            let w = (loc.width() as f64 / world_w) * MINIMAP_W;
            let h = (loc.height() as f64 / world_h) * MINIMAP_H;

            ctx.set_fill_style_str("rgba(168, 85, 247, 0.4)");
            ctx.fill_rect(x, y, w.max(2.0), h.max(2.0));
            ctx.set_stroke_style_str("rgba(168, 85, 247, 0.9)");
            ctx.set_line_width(1.5);
            ctx.stroke_rect(x, y, w.max(2.0), h.max(2.0));
        }
    }
}

#[component]
pub fn Minimap() -> impl IntoView {
    let IsMobile(is_mobile) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let viewport: RwSignal<Viewport> = expect_context();
    let loaded_tiles: RwSignal<Vec<LoadedTile>> = expect_context();
    let Selected(selected) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let CurrentMode(map_mode) = expect_context();
    let HeatModeEnabled(heat_mode_enabled) = expect_context();
    let HeatEntriesByTerritory(heat_entries_by_territory) = expect_context();
    let HeatMaxTakeCount(heat_max_take_count) = expect_context();

    let canvas_ref = NodeRef::<leptos::html::Canvas>::new();

    // Cached Canvas 2D context for minimap
    let cached_ctx: Rc<RefCell<Option<CanvasRenderingContext2d>>> = Rc::new(RefCell::new(None));

    // Offscreen canvas for territory layer (redrawn only on state changes)
    let offscreen: Rc<RefCell<Option<OffscreenCache>>> = Rc::new(RefCell::new(None));
    let offscreen_dirty: Rc<Cell<bool>> = Rc::new(Cell::new(true));
    let offscreen_render = offscreen.clone();
    let offscreen_dirty_render = offscreen_dirty.clone();

    // Render minimap via rAF batching
    let scheduler = RenderScheduler::new(move || {
        let Some(canvas) = canvas_ref.get_untracked() else {
            return false;
        };
        let canvas: &HtmlCanvasElement = &canvas;

        // Only reset canvas dimensions if they differ (supersampled)
        let scale = render_scale();
        let expected_w = (MINIMAP_W * scale) as u32;
        let expected_h = (MINIMAP_H * scale) as u32;
        if canvas.width() != expected_w || canvas.height() != expected_h {
            canvas.set_width(expected_w);
            canvas.set_height(expected_h);
            *cached_ctx.borrow_mut() = None;
        }

        let ctx = {
            let mut ctx_cache = cached_ctx.borrow_mut();
            if ctx_cache.is_none() {
                let Some(ctx) = canvas
                    .get_context("2d")
                    .ok()
                    .flatten()
                    .and_then(|ctx| ctx.dyn_into::<CanvasRenderingContext2d>().ok())
                else {
                    return false;
                };
                ctx.scale(scale, scale).ok();
                *ctx_cache = Some(ctx);
            }
            let Some(ctx) = ctx_cache.clone() else {
                return false;
            };
            ctx
        };

        // Ensure offscreen cache exists
        let mut offscreen_ref = offscreen_render.borrow_mut();
        if offscreen_ref.is_none() {
            *offscreen_ref = OffscreenCache::new();
            offscreen_dirty_render.set(true);
        }
        let Some(ref cache) = *offscreen_ref else {
            return false;
        };

        // Redraw offscreen territory layer only when dirty
        if offscreen_dirty_render.get() {
            offscreen_dirty_render.set(false);
            let sel = selected.get_untracked();
            let heat_mode = heat_mode_enabled.get_untracked();
            let heat_max = heat_max_take_count.get_untracked();
            territories.with_untracked(|terr| {
                loaded_tiles.with_untracked(|tiles| {
                    heat_entries_by_territory.with_untracked(|heat_entries| {
                        cache.redraw(terr, tiles, &sel, heat_mode, heat_entries, heat_max);
                    });
                });
            });
        }

        // Blit cached territory layer (explicit size so ctx.scale doesn't double it)
        ctx.draw_image_with_html_canvas_element_and_dw_and_dh(
            &cache.canvas,
            0.0,
            0.0,
            MINIMAP_W,
            MINIMAP_H,
        )
        .ok();

        // Draw viewport indicator on top
        let vp = viewport.get_untracked();
        render_viewport_indicator(&ctx, &vp, 0.0);

        false
    });
    let scheduler = Rc::new(scheduler);

    // State effect: territory/tile/selection changes invalidate offscreen cache
    let sched_state = scheduler.clone();
    let offscreen_dirty_state = offscreen_dirty.clone();
    Effect::new(move || {
        territories.track();
        loaded_tiles.track();
        selected.track();
        heat_mode_enabled.track();
        heat_entries_by_territory.track();
        heat_max_take_count.track();
        offscreen_dirty_state.set(true);
        sched_state.mark_dirty();
    });

    // Viewport effect: pan/zoom or sidebar toggle needs viewport indicator redrawn
    let sched_vp = scheduler.clone();
    Effect::new(move || {
        viewport.track();
        sidebar_open.track();
        sched_vp.mark_dirty();
    });

    // Click to navigate
    let on_click = move |e: MouseEvent| {
        let x = e.offset_x() as f64;
        let y = e.offset_y() as f64;

        let world_w = WORLD_MAX_X - WORLD_MIN_X;
        let world_h = WORLD_MAX_Y - WORLD_MIN_Y;

        let wx = WORLD_MIN_X + (x / MINIMAP_W) * world_w;
        let wy = WORLD_MIN_Y + (y / MINIMAP_H) * world_h;

        let (canvas_w, canvas_h) = canvas_dimensions();

        viewport.update(|vp| {
            vp.offset_x = canvas_w / 2.0 - wx * vp.scale;
            vp.offset_y = canvas_h / 2.0 - wy * vp.scale;
        });
    };

    view! {
        <div
            style:display=move || if is_mobile.get() { "none" } else { "block" }
            style:bottom=move || if map_mode.get() == MapMode::History { "68px" } else { "16px" }
            style="position: absolute; left: 16px; z-index: 5; background: var(--color-deep); border: 1px solid var(--color-border-accent); border-radius: 4px; box-shadow: 0 4px 20px rgba(0,0,0,0.6), 0 0 1px rgba(168,85,247,0.15), inset 0 0 0 1px rgba(255,255,255,0.03); overflow: hidden;"
        >
            // MAP label
            <div style="position: absolute; top: 6px; left: 8px; z-index: 1; font-family: var(--font-display); font-size: 0.62rem; color: rgba(245,197,66,0.5); letter-spacing: 0.1em; pointer-events: none;">"MAP"</div>
            // Gold corner marks — top-left
            <div style="position: absolute; top: 0; left: 0; width: 8px; height: 1px; background: rgba(245,197,66,0.3); pointer-events: none;" />
            <div style="position: absolute; top: 0; left: 0; width: 1px; height: 8px; background: rgba(245,197,66,0.3); pointer-events: none;" />
            // Gold corner marks — top-right
            <div style="position: absolute; top: 0; right: 0; width: 8px; height: 1px; background: rgba(245,197,66,0.3); pointer-events: none;" />
            <div style="position: absolute; top: 0; right: 0; width: 1px; height: 8px; background: rgba(245,197,66,0.3); pointer-events: none;" />
            // Gold corner marks — bottom-left
            <div style="position: absolute; bottom: 0; left: 0; width: 8px; height: 1px; background: rgba(245,197,66,0.3); pointer-events: none;" />
            <div style="position: absolute; bottom: 0; left: 0; width: 1px; height: 8px; background: rgba(245,197,66,0.3); pointer-events: none;" />
            // Gold corner marks — bottom-right
            <div style="position: absolute; bottom: 0; right: 0; width: 8px; height: 1px; background: rgba(245,197,66,0.3); pointer-events: none;" />
            <div style="position: absolute; bottom: 0; right: 0; width: 1px; height: 8px; background: rgba(245,197,66,0.3); pointer-events: none;" />
            <canvas
                node_ref=canvas_ref
                on:click=on_click
                style="cursor: pointer; display: block; width: 200px; height: 280px;"
            />
        </div>
    }
}

/// Draw only the viewport indicator rectangle (called every frame).
fn render_viewport_indicator(ctx: &CanvasRenderingContext2d, viewport: &Viewport, sidebar_w: f64) {
    let world_w = WORLD_MAX_X - WORLD_MIN_X;
    let world_h = WORLD_MAX_Y - WORLD_MIN_Y;

    let Some(window) = web_sys::window() else {
        return;
    };
    let canvas_w = window
        .inner_width()
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(1200.0)
        - sidebar_w;
    let canvas_h = window
        .inner_height()
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(800.0);

    let (tl_wx, tl_wy) = viewport.screen_to_world(0.0, 0.0);
    let (br_wx, br_wy) = viewport.screen_to_world(canvas_w, canvas_h);

    let vp_x = ((tl_wx - WORLD_MIN_X) / world_w) * MINIMAP_W;
    let vp_y = ((tl_wy - WORLD_MIN_Y) / world_h) * MINIMAP_H;
    let vp_w = ((br_wx - tl_wx) / world_w) * MINIMAP_W;
    let vp_h = ((br_wy - tl_wy) / world_h) * MINIMAP_H;

    // Viewport rectangle — warmer gold with subtle glow
    ctx.set_shadow_color("rgba(245, 197, 66, 0.3)");
    ctx.set_shadow_blur(6.0);
    ctx.set_stroke_style_str("rgba(245, 197, 66, 0.8)");
    ctx.set_line_width(1.5);
    ctx.stroke_rect(vp_x, vp_y, vp_w, vp_h);
    ctx.set_shadow_blur(0.0);
    ctx.set_shadow_color("transparent");
}
