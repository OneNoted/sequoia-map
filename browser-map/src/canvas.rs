//! The map canvas component: renderer lifecycle, demand-driven repaints and DOM overlays.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use leptos::prelude::*;
use sequoia_map_engine::gesture::{Gestures, ScreenRect};
use sequoia_map_engine::minimap::MinimapLayout;
use sequoia_map_engine::scene::{Rebuild, SceneChange, ScenePlanner};
use sequoia_map_engine::spatial::SpatialGrid;
use web_sys::HtmlCanvasElement;

use crate::frame::{Frame, FrameMetrics, HeatColors, RenderCapabilities};
use crate::gpu::GpuRenderer;
use crate::input::MapInput;
use crate::render_loop::RenderScheduler;
use crate::tiles::tile_world_bounds;
use crate::{BrowserMap, MapEvent, icons};

/// Canvas size in physical pixels and the device pixel ratio it was sized for.
#[derive(Clone, Copy, Default)]
pub(crate) struct Surface {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) dpr: f32,
}

/// Non-reactive per-canvas state shared by the render loop and input handlers.
pub(crate) struct MapState {
    pub(crate) gestures: Gestures,
    pub(crate) grid: SpatialGrid,
    pub(crate) territory_bounds: Option<(f64, f64, f64, f64)>,
    /// The territory the current paint stroke last reached.
    pub(crate) stroke_hit: Option<String>,
    pub(crate) surface: Surface,
    planner: ScenePlanner,
    renderer: Option<GpuRenderer>,
    /// The camera has been fitted to the first territories that arrived.
    fitted: bool,
}

impl MapState {
    fn new() -> Self {
        Self {
            gestures: Gestures::default(),
            grid: SpatialGrid::build(&HashMap::new()),
            territory_bounds: None,
            stroke_hit: None,
            surface: Surface::default(),
            planner: ScenePlanner::default(),
            renderer: None,
            fitted: false,
        }
    }

    /// The minimap as currently drawn: over the loaded tiles, or the territories until then.
    pub(crate) fn minimap_layout(&self, map: &BrowserMap) -> Option<MinimapLayout> {
        let inset = map.inputs.minimap_inset.get_untracked()?;
        if self.surface.width == 0 {
            return None;
        }
        let world = map
            .tiles
            .with_untracked(|tiles| tile_world_bounds(tiles))
            .or(self.territory_bounds);
        MinimapLayout::new(
            self.surface.width,
            self.surface.height,
            self.surface.dpr,
            inset,
            world,
        )
    }
}

#[derive(Clone, Copy)]
struct RenderStats {
    capabilities: RenderCapabilities,
    metrics: FrameMetrics,
    rebuild: Rebuild,
    interacting: bool,
}

#[component]
pub fn MapCanvas(map: BrowserMap, #[prop(into)] on_event: Callback<MapEvent>) -> impl IntoView {
    let inputs = map.inputs;
    let canvas_ref = NodeRef::<leptos::html::Canvas>::new();
    let state = Rc::new(RefCell::new(MapState::new()));
    let gpu_error: RwSignal<Option<String>> = RwSignal::new(None);
    let select_box: RwSignal<Option<ScreenRect>> = RwSignal::new(None);
    let render_stats: Option<RwSignal<Option<RenderStats>>> =
        render_stats_enabled().then(|| RwSignal::new(None));
    // Set on unmount: a frame or renderer init still in flight must not touch disposed
    // signals or revive the renderer.
    let disposed = Arc::new(AtomicBool::new(false));

    let scheduler = Rc::new(RenderScheduler::new({
        let state = state.clone();
        let disposed = disposed.clone();
        move || {
            if disposed.load(Ordering::Relaxed) {
                return false;
            }
            let Some(canvas) = canvas_ref.get_untracked() else {
                return false;
            };
            render_frame(&map, &state, &canvas, render_stats)
        }
    }));

    // HQ crowns come from the icon atlas, so it loads even with resource icons off.
    if map.icons.with_untracked(Option::is_none) {
        icons::load_resource_atlas(map.icons);
    }

    // Hit-testing grid, territory bounds and the initial fit follow the territory map.
    Effect::new({
        let state = state.clone();
        let scheduler = scheduler.clone();
        move || {
            let fit = inputs.territories.with(|territories| {
                let grid = SpatialGrid::build(territories);
                let bounds = grid.world_bounds();
                let mut state = state.borrow_mut();
                state.grid = grid;
                state.territory_bounds = bounds;
                state.planner.invalidate(SceneChange::Territories);
                let fit = bounds.filter(|_| !state.fitted);
                state.fitted |= fit.is_some();
                fit
            });
            if let Some(bounds) = fit {
                map.camera.fit(bounds);
            }
            scheduler.mark_dirty();
        }
    });

    Effect::new({
        let state = state.clone();
        let scheduler = scheduler.clone();
        move || {
            map.hovered.track();
            inputs.selected.track();
            state
                .borrow_mut()
                .planner
                .invalidate(SceneChange::Highlight);
            scheduler.mark_dirty();
        }
    });

    Effect::new({
        let state = state.clone();
        let scheduler = scheduler.clone();
        move || {
            if let Some(heat) = inputs.heat {
                heat.enabled.track();
                heat.take_counts.track();
                heat.max_take_count.track();
            }
            if let Some(wars) = inputs.wars {
                wars.track();
            }
            state.borrow_mut().planner.invalidate(SceneChange::Overlay);
            scheduler.mark_dirty();
        }
    });

    Effect::new({
        let state = state.clone();
        let scheduler = scheduler.clone();
        move || {
            if map.icons.with(Option::is_some) {
                state.borrow_mut().planner.invalidate(SceneChange::Icons);
                scheduler.mark_dirty();
            }
        }
    });

    // Everything else only needs a repaint; the scene planner decides what is stale. The
    // animation tick never reaches the planner, so its frames only advance shader time.
    Effect::new({
        let scheduler = scheduler.clone();
        move || {
            map.camera.track();
            inputs.settings.track();
            inputs.clock_secs.track();
            if let Some(tick) = inputs.animation_tick {
                tick.track();
            }
            inputs.minimap_inset.track();
            map.tiles.track();
            scheduler.mark_dirty();
        }
    });

    Effect::new({
        let state = state.clone();
        let scheduler = scheduler.clone();
        let disposed = disposed.clone();
        move |started: Option<bool>| {
            if started == Some(true) {
                return true;
            }
            let Some(canvas) = canvas_ref.get() else {
                return false;
            };
            let state = state.clone();
            let scheduler = scheduler.clone();
            let disposed = disposed.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match GpuRenderer::init(canvas).await {
                    Ok(renderer) => {
                        if !disposed.load(Ordering::Relaxed) {
                            state.borrow_mut().renderer = Some(renderer);
                            scheduler.mark_dirty();
                        }
                    }
                    Err(error) => {
                        web_sys::console::error_1(
                            &format!("wgpu init failed (fail-closed): {error}").into(),
                        );
                        if !disposed.load(Ordering::Relaxed) {
                            gpu_error.set(Some(error));
                        }
                    }
                }
            });
            true
        }
    });

    // Everything else (renderer, gesture state, pending frame) is dropped with the last
    // handler and effect that holds it.
    on_cleanup(move || disposed.store(true, Ordering::Relaxed));

    let input = MapInput {
        map,
        on_event,
        state,
        scheduler,
        select_box,
    };

    let on_pointer_down = {
        let input = input.clone();
        move |event| input.pointer_down(&event)
    };
    let on_pointer_move = {
        let input = input.clone();
        move |event| input.pointer_move(&event)
    };
    let on_pointer_up = {
        let input = input.clone();
        move |event| input.pointer_up(&event)
    };
    let on_pointer_cancel = {
        let input = input.clone();
        move |event| input.pointer_cancel(&event)
    };
    let on_lost_capture = {
        let input = input.clone();
        move |event| input.pointer_cancel(&event)
    };
    let on_pointer_leave = {
        let input = input.clone();
        move |_| input.pointer_leave()
    };
    let on_wheel = move |event| input.wheel(&event);

    view! {
        <div style="position: absolute; inset: 0;">
            <canvas
                node_ref=canvas_ref
                style="position: absolute; inset: 0; width: 100%; height: 100%; touch-action: none; user-select: none; cursor: grab;"
                on:pointerdown=on_pointer_down
                on:pointermove=on_pointer_move
                on:pointerup=on_pointer_up
                on:pointercancel=on_pointer_cancel
                on:lostpointercapture=on_lost_capture
                on:pointerleave=on_pointer_leave
                on:wheel=on_wheel
                on:contextmenu=move |event| event.prevent_default()
            />
            {move || {
                select_box.get().map(|rect| {
                    let width = rect.width().max(1.0);
                    let height = rect.height().max(1.0);
                    let (left, top) = (rect.left, rect.top);
                    view! {
                        <div
                            style=format!(
                                "position: absolute; left: {left}px; top: {top}px; width: {width}px; height: {height}px; z-index: 26; pointer-events: none; border: 1px dashed rgba(245,197,66,0.94); background: rgba(245,197,66,0.14); box-shadow: 0 0 0 1px rgba(12,14,23,0.38) inset;"
                            )
                        ></div>
                    }
                })
            }}
            {move || gpu_error.get().map(|message| view! { <GpuErrorPanel message /> })}
            {move || {
                render_stats
                    .and_then(|stats| stats.get())
                    .map(|stats| view! { <RenderStatsPanel stats /> })
            }}
        </div>
    }
}

/// Sizes the canvas, plans the frame and draws it. Returns whether to keep animating.
fn render_frame(
    map: &BrowserMap,
    state: &RefCell<MapState>,
    canvas: &HtmlCanvasElement,
    render_stats: Option<RwSignal<Option<RenderStats>>>,
) -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    if canvas.parent_element().is_none() {
        return false;
    }
    // The canvas's own CSS size, fractional where the layout is: rounding it first would
    // stretch the backing store against the pointer coordinates by up to a pixel.
    let rect = canvas.get_bounding_client_rect();
    let css_width = rect.width().max(1.0);
    let css_height = rect.height().max(1.0);
    let dpr = window.device_pixel_ratio().max(1.0);
    let width = (css_width * dpr).round().max(1.0) as u32;
    let height = (css_height * dpr).round().max(1.0) as u32;
    if canvas.width() != width {
        canvas.set_width(width);
    }
    if canvas.height() != height {
        canvas.set_height(height);
    }
    map.camera.set_canvas_size((css_width, css_height));

    let mut state = state.borrow_mut();
    state.surface = Surface {
        width,
        height,
        dpr: dpr as f32,
    };
    let minimap = state.minimap_layout(map);
    let MapState {
        gestures,
        planner,
        renderer,
        territory_bounds,
        ..
    } = &mut *state;
    let Some(renderer) = renderer.as_mut() else {
        return false;
    };
    renderer.resize(width, height, dpr as f32);

    let inputs = map.inputs;
    let now_ms = js_sys::Date::now();
    // Gesture times are event time stamps, on the `performance.now()` clock.
    let interacting = gestures.is_interacting(
        window
            .performance()
            .map_or(f64::INFINITY, |performance| performance.now()),
    );
    let camera = map.camera.get_untracked();
    let clock_secs = inputs.clock_secs.get_untracked();
    let settings = inputs.settings.get_untracked();
    let rebuild = planner.plan(&camera, &settings, clock_secs, interacting);
    let hovered = map.hovered.get_untracked();
    let selected = inputs.selected.get_untracked();
    let icons = map.icons.get_untracked();
    let heat = inputs.heat.filter(|heat| heat.enabled.get_untracked());
    let max_take_count = heat.map_or(0, |heat| heat.max_take_count.get_untracked());

    let outcome = inputs.territories.with_untracked(|territories| {
        map.tiles.with_untracked(|tiles| {
            with_optional(heat.map(|heat| heat.take_counts), |take_counts| {
                with_optional(inputs.wars, |wars| {
                    renderer.render(
                        &Frame {
                            camera: &camera,
                            territories,
                            hovered: hovered.as_deref(),
                            selected: selected.as_deref(),
                            settings: &settings,
                            heat: take_counts.map(|take_counts| HeatColors {
                                take_counts,
                                max_take_count,
                            }),
                            wars,
                            territory_bounds: *territory_bounds,
                            tiles,
                            icons: icons.as_ref(),
                            minimap,
                            clock_secs,
                            now_ms,
                        },
                        rebuild,
                    )
                })
            })
        })
    });
    planner.schedule(outcome.next_refresh);

    if let Some(stats) = render_stats {
        stats.set(Some(RenderStats {
            capabilities: renderer.capabilities(),
            metrics: renderer.frame_metrics(),
            rebuild,
            interacting,
        }));
    }
    outcome.animating
}

fn with_optional<T: Send + Sync + 'static, R>(
    signal: Option<Signal<T>>,
    read: impl FnOnce(Option<&T>) -> R,
) -> R {
    match signal {
        Some(signal) => signal.with_untracked(|value| read(Some(value))),
        None => read(None),
    }
}

fn render_stats_enabled() -> bool {
    web_sys::window()
        .and_then(|window| {
            js_sys::Reflect::get(
                window.as_ref(),
                &wasm_bindgen::JsValue::from_str("__SEQUOIA_RENDER_STATS__"),
            )
            .ok()
        })
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn diagnostics_token(message: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in message.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("GPUX-{hash:016x}")
}

#[component]
fn GpuErrorPanel(message: String) -> impl IntoView {
    let token = diagnostics_token(&message);
    view! {
        <div style="position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; background: rgba(12, 14, 23, 0.96); z-index: 30;">
            <div style="max-width: 640px; margin: 0 24px; border: 1px solid var(--color-border-accent); background: var(--color-deep); box-shadow: 0 24px 64px rgba(0,0,0,0.55); border-radius: 8px; padding: 22px 20px;">
                <div style="font-family: var(--font-display); color: var(--color-gold); letter-spacing: 0.08em; font-size: 0.78rem; text-transform: uppercase; margin-bottom: 8px;">
                    "Unsupported GPU Configuration"
                </div>
                <div style="font-family: var(--font-body); color: var(--color-text-primary); line-height: 1.45; font-size: 0.92rem;">
                    "The map renderer requires wgpu/WebGL2 and does not provide a Canvas2D fallback."
                </div>
                <div style="margin-top: 10px; font-family: var(--font-mono); font-size: 0.74rem; color: var(--color-text-secondary); word-break: break-word;">
                    {message}
                </div>
                <div style="margin-top: 12px; font-family: var(--font-mono); font-size: 0.7rem; color: var(--color-text-dim);">
                    "Diagnostics token: "
                    <span style="color: var(--color-gold);">{token}</span>
                </div>
            </div>
        </div>
    }
}

#[component]
fn RenderStatsPanel(stats: RenderStats) -> impl IntoView {
    let RenderStats {
        capabilities: caps,
        metrics,
        rebuild,
        interacting,
    } = stats;
    let summary = format!(
        "fps={:.1} cpu={:.2}ms draws={} tiles={} upload={}KB scale={:.2} terr={} text={}",
        metrics.fps_estimate,
        metrics.frame_cpu_ms,
        metrics.draw_calls,
        metrics.tile_draw_calls,
        metrics.bytes_uploaded as f64 / 1024.0,
        metrics.resolution_scale,
        metrics.territory_instances,
        metrics.text_instances
    );
    let rebuilt = format!(
        "rebuilt: terr={} conn={} static={} dynamic={} icons={} interact={}",
        rebuild.territories,
        rebuild.connections,
        rebuild.static_labels,
        rebuild.dynamic_labels,
        rebuild.icons,
        interacting
    );
    let caps = format!(
        "webgl2={} msdf={} dynamic={} fallback={}",
        caps.webgl2, caps.gpu_text_msdf, caps.gpu_dynamic_labels, caps.compatibility_fallback
    );
    view! {
        <div style="position: absolute; top: calc(var(--nav-height, 0px) + 10px); left: 10px; z-index: 25; pointer-events: none; background: rgba(8,10,18,0.78); border: 1px solid rgba(245,197,66,0.35); border-radius: 6px; padding: 6px 8px; color: var(--color-text-primary); font-family: var(--font-mono); font-size: 0.66rem; line-height: 1.35;">
            <div>{summary}</div>
            <div style="color: #c9c3b8;">{rebuilt}</div>
            <div style="color: var(--color-text-secondary);">{caps}</div>
        </div>
    }
}
