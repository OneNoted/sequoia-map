//! The map canvas component: demand-driven repaints and DOM overlays over the renderer that
//! [`renderer_lifecycle`] starts, rebuilds and disposes. Until a renderer runs, the canvas
//! draws nothing rather than calling into a missing or dead context.

mod renderer_lifecycle;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use leptos::prelude::*;
use sequoia_map_engine::gesture::{Gestures, ScreenRect};
use sequoia_map_engine::minimap::MinimapLayout;
use sequoia_map_engine::scene::{Rebuild, SceneChange, ScenePlanner};
use sequoia_map_engine::spatial::SpatialGrid;
use web_sys::HtmlCanvasElement;

use renderer_lifecycle::{Attempt, RendererLifecycle};

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
    /// Counts changes to the territory map, so renderer caches derived from it alone know
    /// when they are stale.
    territory_revision: u64,
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
            territory_revision: 0,
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
    let select_box: RwSignal<Option<ScreenRect>> = RwSignal::new(None);
    let render_stats: Option<RwSignal<Option<RenderStats>>> =
        render_stats_enabled().then(|| RwSignal::new(None));
    let lifecycle = RendererLifecycle::new(state.clone(), canvas_ref);
    let pending_font: RwSignal<Option<(Attempt, String)>> = RwSignal::new(None);

    let scheduler = Rc::new(RenderScheduler::new({
        let state = state.clone();
        let lifecycle = lifecycle.clone();
        move || {
            // An unmounted canvas's frame must not touch disposed signals.
            if lifecycle.is_disposed() {
                return false;
            }
            if lifecycle.is_lost() {
                state.borrow_mut().renderer = None;
                return false;
            }
            let Some(canvas) = canvas_ref.get_untracked() else {
                return false;
            };
            let animating = render_frame(&map, &state, &canvas, render_stats);
            let request = state
                .borrow()
                .renderer
                .as_ref()
                .and_then(GpuRenderer::pending_text_font)
                .map(|font| (lifecycle.attempt(), font));
            if pending_font.get_untracked() != request {
                pending_font.set(request);
            }
            animating
        }
    }));

    // Each atlas can be baked before its face arrives, including a persisted readable
    // font or a switch during loading. Observe the renderer's actual pending face after
    // every paint, rather than waiting only for the default face at renderer startup.
    Effect::new({
        let state = state.clone();
        let scheduler = scheduler.clone();
        let lifecycle = lifecycle.clone();
        move || {
            let Some((attempt, font)) = pending_font.get() else {
                return;
            };
            let Some(fonts) = web_sys::window()
                .and_then(|window| window.document())
                .map(|document| document.fonts())
            else {
                return;
            };
            let state = state.clone();
            let scheduler = scheduler.clone();
            let lifecycle = lifecycle.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if wasm_bindgen_futures::JsFuture::from(fonts.load(&font))
                    .await
                    .is_err()
                    || lifecycle.attempt() != attempt
                {
                    return;
                }
                {
                    if let Some(renderer) = state.borrow_mut().renderer.as_mut() {
                        renderer.text_font_loaded(&font);
                    }
                }
                scheduler.mark_dirty();
            });
        }
    });

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
                state.territory_revision = state.territory_revision.wrapping_add(1);
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
                heat.scale.track();
            }
            if let Some(war_marks) = inputs.war_marks {
                war_marks.track();
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
        let lifecycle = lifecycle.clone();
        move || {
            lifecycle.track();
            map.camera.track();
            inputs.settings.track();
            inputs.clock_secs.track();
            if let Some(tick) = inputs.animation_tick {
                tick.track();
            }
            inputs.minimap_inset.track();
            map.tiles.track();
            map.markers.track();
            map.site_labels.track();
            map.teammates.track();
            map.war_plates.track();
            scheduler.mark_dirty();
        }
    });

    Effect::new({
        let lifecycle = lifecycle.clone();
        move || {
            if canvas_ref.get().is_some() {
                lifecycle.canvas_changed();
            }
        }
    });

    let failure = lifecycle.view_failure();
    let canvas_epoch = lifecycle.canvas_epoch();
    let input = MapInput {
        map,
        on_event,
        state,
        scheduler,
        select_box,
    };
    let parts = StoredValue::new_local((input, lifecycle));
    // A canvas keeps its first context type, so the lifecycle has the element replaced,
    // handlers and all, when another backend needs it. Keying it on the epoch makes that a
    // new element: an unkeyed rebuild would update the old one in place.
    let canvas = view! {
        <For each=move || [canvas_epoch.get()] key=|epoch| *epoch let:_epoch>
            {parts
                .with_value(|(input, lifecycle)| {
                    map_canvas_element(canvas_ref, input.clone(), lifecycle.clone())
                })}
        </For>
    };

    // Everything else (gesture state, pending frame) is dropped with the last handler and
    // effect that holds it.
    on_cleanup(move || {
        if let Some((_, lifecycle)) = parts.try_get_value() {
            lifecycle.dispose();
        }
    });

    view! {
        <div style="position: absolute; inset: 0;">
            {canvas}
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
            {failure}
            {move || {
                render_stats
                    .and_then(|stats| stats.get())
                    .map(|stats| view! { <RenderStatsPanel stats /> })
            }}
        </div>
    }
}

/// The map's `<canvas>`, with its input and graphics-context handlers.
fn map_canvas_element(
    canvas_ref: NodeRef<leptos::html::Canvas>,
    input: MapInput,
    lifecycle: Rc<RendererLifecycle>,
) -> impl IntoView {
    let on_context_lost = {
        let lifecycle = lifecycle.clone();
        move |event: web_sys::Event| lifecycle.context_lost(&event)
    };
    let on_context_restored = {
        let lifecycle = lifecycle.clone();
        move |_: web_sys::Event| lifecycle.context_restored()
    };
    let on_context_creation_error =
        move |event: web_sys::Event| lifecycle.context_creation_error(&event);
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
            on:webglcontextlost=on_context_lost
            on:webglcontextrestored=on_context_restored
            on:webglcontextcreationerror=on_context_creation_error
        />
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
    let mut state = state.borrow_mut();
    // The canvas's own CSS size, fractional where the layout is: rounding it first would
    // stretch the backing store against the pointer coordinates by up to a pixel.
    let rect = canvas.get_bounding_client_rect();
    let css_width = rect.width().max(1.0);
    let css_height = rect.height().max(1.0);
    // Fewer backing pixels per CSS pixel rather than a surface the device cannot hold.
    let max_side = state.renderer.as_ref().map_or(f64::INFINITY, |renderer| {
        f64::from(renderer.max_surface_side())
    });
    let dpr = window
        .device_pixel_ratio()
        .max(1.0)
        .min(max_side / css_width)
        .min(max_side / css_height);
    let width = (css_width * dpr).round().max(1.0) as u32;
    let height = (css_height * dpr).round().max(1.0) as u32;
    if canvas.width() != width {
        canvas.set_width(width);
    }
    if canvas.height() != height {
        canvas.set_height(height);
    }
    map.camera.set_canvas_size((css_width, css_height));

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
        territory_revision,
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
    let heat_scale = heat.map(|heat| heat.scale.get_untracked());
    let site_labels = map.site_labels.get_untracked();
    let teammates = map.teammates.get_untracked();
    let war_plates = map.war_plates.get_untracked();

    let outcome = inputs.territories.with_untracked(|territories| {
        map.tiles.with_untracked(|tiles| {
            map.markers.with_untracked(|markers| {
                with_optional(heat.map(|heat| heat.take_counts), |take_counts| {
                    with_optional(inputs.war_marks, |war_marks| {
                        renderer.render(
                            &Frame {
                                camera: &camera,
                                territories,
                                territory_revision: *territory_revision,
                                hovered: hovered.as_deref(),
                                selected: selected.as_deref(),
                                settings: &settings,
                                heat: take_counts
                                    .zip(heat_scale.as_deref())
                                    .map(|(take_counts, scale)| HeatColors { take_counts, scale }),
                                war_marks,
                                territory_bounds: *territory_bounds,
                                tiles,
                                icons: icons.as_ref(),
                                markers: markers.as_ref(),
                                site_labels: site_labels.as_ref(),
                                war_plates: war_plates.as_ref(),
                                teammates: teammates.as_ref(),
                                minimap,
                                clock_secs,
                                now_ms,
                            },
                            rebuild,
                        )
                    })
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
