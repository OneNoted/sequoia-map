use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, MouseEvent, PointerEvent, WheelEvent};

use crate::app::{
    AbbreviateNames, BoldConnections, ConnectionOpacityScale, ConnectionThicknessScale,
    CurrentMode, DetailReturnGuild, HeatEntriesByTerritory, HeatMaxTakeCount, HeatModeEnabled,
    HeatWindowLabel, HistoryTimestamp, Hovered, IsMobile, LabelScaleDynamic, LabelScaleIcons,
    LabelScaleMaster, LabelScaleStatic, LabelScaleStaticName, MapMode, NameColorSetting,
    PeekTerritory, ReadableFont, ResourceHighlight, Selected, ShowCompoundMapTime, ShowCountdown,
    ShowGranularMapTime, ShowMinimap, ShowNames, ShowResourceIcons, SidebarOpen, SidebarTransient,
    TagColorSetting, ThickCooldownBorders,
};
use crate::gpu::{GpuRenderer, RenderFrameInput};
use crate::icons::{self, ResourceAtlas};
use crate::render_loop::RenderScheduler;
use crate::renderer::{
    FrameMetrics, InvalidationReason, RenderCapabilities, SceneBuilder, SceneSummary,
};
use crate::spatial::SpatialGrid;
use crate::territory::ClientTerritoryMap;
use crate::tiles::{LoadedTile, TileQuality};
use crate::viewport::Viewport;

const INTERACTION_SETTLE_MS: f64 = 140.0;
const MINIMAP_W: f64 = 200.0;
const MINIMAP_H: f64 = 280.0;
const MINIMAP_MARGIN: f64 = 16.0;
const MINIMAP_HISTORY_BOTTOM: f64 = 68.0;
const DEFAULT_MINIMAP_WORLD: (f64, f64, f64, f64) = (-2200.0, -6600.0, 1600.0, 400.0);
const WHEEL_DELTA_MODE_PIXEL: u32 = 0;
const WHEEL_DELTA_MODE_LINE: u32 = 1;
const WHEEL_DELTA_MODE_PAGE: u32 = 2;
const TRACKPAD_BURST_GAP_MS: f64 = 45.0;
const TRACKPAD_STICKY_MS: f64 = 900.0;
const TRACKPAD_STICKY_CONTINUE_GAP_MS: f64 = 180.0;
const TRACKPAD_STICKY_PIXEL_DELTA_LIMIT: f64 = 96.0;
const TRACKPAD_SMALL_PIXEL_DELTA: f64 = 32.0;
const TRACKPAD_BURST_DELTA_LIMIT: f64 = 80.0;
const TRACKPAD_LINE_HEIGHT_PX: f64 = 18.0;
const TRACKPAD_PAGE_HEIGHT_FACTOR: f64 = 0.9;
const TRACKPAD_ZOOM_GAIN: f64 = 2.35;
const TRACKPAD_ZOOM_CLAMP: f64 = 280.0;
const PINCH_LINE_HEIGHT_PX: f64 = 24.0;
const PINCH_PAGE_HEIGHT_FACTOR: f64 = 1.0;
const PINCH_ZOOM_GAIN: f64 = 4.2;
const PINCH_ZOOM_CLAMP: f64 = 420.0;

#[derive(Clone, Copy, Debug)]
struct WheelSample {
    delta_x: f64,
    delta_y: f64,
    delta_mode: u32,
    timestamp_ms: f64,
}

#[derive(Clone, Copy, Debug)]
struct TrackpadWheelClassifier {
    last_event_ms: f64,
    rapid_streak: u8,
    trackpad_until_ms: f64,
}

impl Default for TrackpadWheelClassifier {
    fn default() -> Self {
        Self {
            last_event_ms: -1.0,
            rapid_streak: 0,
            trackpad_until_ms: 0.0,
        }
    }
}

impl TrackpadWheelClassifier {
    fn is_trackpad(&mut self, sample: WheelSample) -> bool {
        let elapsed_ms = if self.last_event_ms >= 0.0 && sample.timestamp_ms > self.last_event_ms {
            sample.timestamp_ms - self.last_event_ms
        } else {
            f64::INFINITY
        };

        let rapid = elapsed_ms <= TRACKPAD_BURST_GAP_MS;
        self.rapid_streak = if rapid {
            self.rapid_streak.saturating_add(1)
        } else {
            1
        };

        let abs_x = sample.delta_x.abs();
        let abs_y = sample.delta_y.abs();
        let fractional = has_fractional_component(abs_x) || has_fractional_component(abs_y);
        let small_pixel_delta = sample.delta_mode == WHEEL_DELTA_MODE_PIXEL
            && abs_y > 0.0
            && abs_y <= TRACKPAD_SMALL_PIXEL_DELTA;
        let mixed_axis_scroll = sample.delta_mode == WHEEL_DELTA_MODE_PIXEL
            && abs_x > 0.0
            && abs_y > 0.0
            && abs_x <= TRACKPAD_BURST_DELTA_LIMIT
            && abs_y <= TRACKPAD_BURST_DELTA_LIMIT;
        let bursty_precision_scroll = sample.delta_mode == WHEEL_DELTA_MODE_PIXEL
            && self.rapid_streak >= 4
            && abs_y > 0.0
            && abs_y <= TRACKPAD_BURST_DELTA_LIMIT;

        let has_direct_trackpad_signal =
            fractional || small_pixel_delta || mixed_axis_scroll || bursty_precision_scroll;
        let sticky_continuation = self.trackpad_until_ms > 0.0
            && sample.timestamp_ms <= self.trackpad_until_ms
            && sample.delta_mode == WHEEL_DELTA_MODE_PIXEL
            && elapsed_ms <= TRACKPAD_STICKY_CONTINUE_GAP_MS
            && abs_y > 0.0
            && abs_y <= TRACKPAD_STICKY_PIXEL_DELTA_LIMIT;
        let is_trackpad = has_direct_trackpad_signal || sticky_continuation;

        self.last_event_ms = sample.timestamp_ms;
        if is_trackpad {
            self.trackpad_until_ms = sample.timestamp_ms + TRACKPAD_STICKY_MS;
        }

        is_trackpad
    }
}

fn has_fractional_component(value: f64) -> bool {
    let rounded = value.round();
    (value - rounded).abs() > 0.01
}

fn normalize_trackpad_zoom_delta(sample: WheelSample, viewport_height: f64) -> f64 {
    let raw_pixels = match sample.delta_mode {
        WHEEL_DELTA_MODE_PIXEL => sample.delta_y,
        WHEEL_DELTA_MODE_LINE => sample.delta_y * TRACKPAD_LINE_HEIGHT_PX,
        WHEEL_DELTA_MODE_PAGE => {
            sample.delta_y * viewport_height.max(1.0) * TRACKPAD_PAGE_HEIGHT_FACTOR
        }
        _ => sample.delta_y,
    };
    (raw_pixels * TRACKPAD_ZOOM_GAIN).clamp(-TRACKPAD_ZOOM_CLAMP, TRACKPAD_ZOOM_CLAMP)
}

fn normalize_pinch_zoom_delta(sample: WheelSample, viewport_height: f64) -> f64 {
    let raw_pixels = match sample.delta_mode {
        WHEEL_DELTA_MODE_PIXEL => sample.delta_y,
        WHEEL_DELTA_MODE_LINE => sample.delta_y * PINCH_LINE_HEIGHT_PX,
        WHEEL_DELTA_MODE_PAGE => {
            sample.delta_y * viewport_height.max(1.0) * PINCH_PAGE_HEIGHT_FACTOR
        }
        _ => sample.delta_y,
    };
    (raw_pixels * PINCH_ZOOM_GAIN).clamp(-PINCH_ZOOM_CLAMP, PINCH_ZOOM_CLAMP)
}

fn has_trackpad_like_ctrl_pinch_signal(sample: WheelSample) -> bool {
    let abs_x = sample.delta_x.abs();
    let abs_y = sample.delta_y.abs();
    let fractional = has_fractional_component(abs_x) || has_fractional_component(abs_y);
    let small_or_mid_pixel_delta = sample.delta_mode == WHEEL_DELTA_MODE_PIXEL
        && abs_y > 0.0
        && abs_y <= TRACKPAD_STICKY_PIXEL_DELTA_LIMIT;
    let mixed_axis_scroll = sample.delta_mode == WHEEL_DELTA_MODE_PIXEL
        && abs_x > 0.0
        && abs_y > 0.0
        && abs_x <= TRACKPAD_BURST_DELTA_LIMIT
        && abs_y <= TRACKPAD_BURST_DELTA_LIMIT;
    fractional || small_or_mid_pixel_delta || mixed_axis_scroll
}

fn normalize_wheel_zoom_delta(
    sample: WheelSample,
    viewport_height: f64,
    ctrl_pinch: bool,
    classifier: &mut TrackpadWheelClassifier,
) -> f64 {
    let is_trackpad = classifier.is_trackpad(sample);
    if ctrl_pinch && (is_trackpad || has_trackpad_like_ctrl_pinch_signal(sample)) {
        normalize_pinch_zoom_delta(sample, viewport_height)
    } else if is_trackpad {
        normalize_trackpad_zoom_delta(sample, viewport_height)
    } else {
        sample.delta_y
    }
}

fn tile_upload_signature(tiles: &[LoadedTile]) -> u64 {
    tiles.iter().fold(0u64, |acc, tile| {
        let quality_bits = match tile.quality {
            TileQuality::Low => 1u64,
            TileQuality::High => 2u64,
        };
        acc.wrapping_mul(1_099_511_628_211)
            .wrapping_add(((tile.id as u64) << 2) ^ quality_bits)
    })
}

pub fn gpu_render_scale(_css_width: u32, _css_height: u32) -> f64 {
    let dpr = web_sys::window()
        .map(|w| w.device_pixel_ratio())
        .unwrap_or(1.0);
    dpr.max(1.0)
}

#[inline]
fn minimap_rect(_canvas_w: f64, canvas_h: f64, history_mode: bool) -> (f64, f64, f64, f64) {
    let bottom = if history_mode {
        MINIMAP_HISTORY_BOTTOM
    } else {
        MINIMAP_MARGIN
    };
    (
        MINIMAP_MARGIN,
        (canvas_h - MINIMAP_H - bottom).max(0.0),
        MINIMAP_W,
        MINIMAP_H,
    )
}

#[inline]
fn diagnostics_token(message: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in message.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("GPUX-{:016x}", hash)
}

fn render_stats_enabled() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    js_sys::Reflect::get(
        window.as_ref(),
        &wasm_bindgen::JsValue::from_str("__SEQUOIA_RENDER_STATS__"),
    )
    .ok()
    .and_then(|v| v.as_bool())
    .unwrap_or(false)
}

fn pointer_canvas_coords(event: &PointerEvent) -> (f64, f64) {
    (event.offset_x() as f64, event.offset_y() as f64)
}

fn mouse_canvas_coords(event: &MouseEvent) -> (f64, f64) {
    (event.offset_x() as f64, event.offset_y() as f64)
}

fn wheel_canvas_coords(event: &WheelEvent) -> (f64, f64) {
    (event.offset_x() as f64, event.offset_y() as f64)
}

fn wheel_canvas_size(event: &WheelEvent) -> (f64, f64) {
    event
        .target()
        .and_then(|t| t.dyn_into::<HtmlCanvasElement>().ok())
        .map(|canvas| (canvas.client_width() as f64, canvas.client_height() as f64))
        .unwrap_or((1200.0, 800.0))
}

fn pointer_canvas_size(event: &PointerEvent) -> (f64, f64) {
    event
        .target()
        .and_then(|t| t.dyn_into::<HtmlCanvasElement>().ok())
        .map(|canvas| (canvas.client_width() as f64, canvas.client_height() as f64))
        .unwrap_or((1200.0, 800.0))
}

fn mouse_canvas_size(event: &MouseEvent) -> (f64, f64) {
    event
        .target()
        .and_then(|t| t.dyn_into::<HtmlCanvasElement>().ok())
        .map(|canvas| (canvas.client_width() as f64, canvas.client_height() as f64))
        .unwrap_or((1200.0, 800.0))
}

#[component]
pub fn MapCanvas() -> impl IntoView {
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let viewport: RwSignal<Viewport> = expect_context();
    let Hovered(hovered) = expect_context();
    let Selected(selected) = expect_context();
    let CurrentMode(map_mode) = expect_context();
    let HistoryTimestamp(history_timestamp) = expect_context();
    let IsMobile(is_mobile) = expect_context();
    let PeekTerritory(peek_territory) = expect_context();
    let DetailReturnGuild(detail_return_guild) = expect_context();
    let mouse_pos: RwSignal<(f64, f64)> = expect_context();
    let loaded_tiles: RwSignal<Vec<LoadedTile>> = expect_context();
    let loaded_icons: RwSignal<Option<ResourceAtlas>> = expect_context();
    let tick: RwSignal<i64> = expect_context();
    let show_connections: RwSignal<bool> = expect_context();
    let AbbreviateNames(abbreviate_names) = expect_context();
    let ShowCountdown(show_countdown) = expect_context();
    let ShowGranularMapTime(show_granular_map_time) = expect_context();
    let ShowCompoundMapTime(show_compound_map_time) = expect_context();
    let ShowNames(show_names) = expect_context();
    let ThickCooldownBorders(thick_cooldown_borders) = expect_context();
    let BoldConnections(bold_connections) = expect_context();
    let ConnectionOpacityScale(connection_opacity_scale) = expect_context();
    let ConnectionThicknessScale(connection_thickness_scale) = expect_context();
    let ResourceHighlight(resource_highlight) = expect_context();
    let ShowResourceIcons(show_resource_icons) = expect_context();
    let ReadableFont(readable_font) = expect_context();
    let NameColorSetting(name_color) = expect_context();
    let TagColorSetting(tag_color) = expect_context();
    let ShowMinimap(show_minimap_setting) = expect_context();
    let HeatModeEnabled(heat_mode_enabled) = expect_context();
    let HeatEntriesByTerritory(heat_entries_by_territory) = expect_context();
    let HeatMaxTakeCount(heat_max_take_count) = expect_context();
    let HeatWindowLabel(heat_window_label) = expect_context();
    let LabelScaleMaster(label_scale_master) = expect_context();
    let LabelScaleStatic(label_scale_static_tag) = expect_context();
    let LabelScaleStaticName(label_scale_static_name) = expect_context();
    let LabelScaleDynamic(label_scale_dynamic) = expect_context();
    let LabelScaleIcons(label_scale_icons) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let SidebarTransient(sidebar_transient) = expect_context();

    let canvas_ref = NodeRef::<leptos::html::Canvas>::new();
    let icon_atlas_requested = Rc::new(Cell::new(false));

    // Input state
    let is_dragging = Rc::new(Cell::new(false));
    let drag_pointer_id = Rc::new(Cell::new(None::<i32>));
    let drag_moved = Rc::new(Cell::new(false));
    let last_x = Rc::new(Cell::new(0.0f64));
    let last_y = Rc::new(Cell::new(0.0f64));

    let active_pointers: Rc<RefCell<HashMap<i32, (f64, f64)>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let pinch_last_dist = Rc::new(Cell::new(0.0f64));
    let pinch_last_cx = Rc::new(Cell::new(0.0f64));
    let pinch_last_cy = Rc::new(Cell::new(0.0f64));
    let wheel_classifier = Rc::new(RefCell::new(TrackpadWheelClassifier::default()));

    let interaction_deadline = Rc::new(Cell::new(0.0f64));

    // Spatial hit-test grid and world bounds
    type WorldBounds = Option<(f64, f64, f64, f64)>;
    let spatial_grid: Rc<RefCell<SpatialGrid>> =
        Rc::new(RefCell::new(SpatialGrid::build(&HashMap::new())));
    let world_bounds: Rc<Cell<WorldBounds>> = Rc::new(Cell::new(None));

    // GPU renderer state
    let gpu: Rc<RefCell<Option<GpuRenderer>>> = Rc::new(RefCell::new(None));
    let gpu_init_started = Rc::new(Cell::new(false));
    let gpu_error: RwSignal<Option<String>> = RwSignal::new(None);

    let fitted = Rc::new(Cell::new(false));
    let last_tile_count: Rc<Cell<usize>> = Rc::new(Cell::new(0));
    let last_tile_signature: Rc<Cell<u64>> = Rc::new(Cell::new(0));
    let scene_builder: Rc<RefCell<SceneBuilder>> = Rc::new(RefCell::new(SceneBuilder::default()));
    let renderer_capabilities: RwSignal<Option<RenderCapabilities>> = RwSignal::new(None);
    let frame_metrics: RwSignal<FrameMetrics> = RwSignal::new(FrameMetrics::default());
    let scene_summary: RwSignal<SceneSummary> = RwSignal::new(SceneSummary::default());
    let show_render_stats = render_stats_enabled();

    let scheduler = RenderScheduler::new({
        let gpu = gpu.clone();
        let world_bounds = world_bounds.clone();
        let is_dragging = is_dragging.clone();
        let interaction_deadline = interaction_deadline.clone();
        let active_pointers = active_pointers.clone();
        let last_tile_count = last_tile_count.clone();
        let last_tile_signature = last_tile_signature.clone();
        let scene_builder = scene_builder.clone();

        move || {
            let Some(canvas) = canvas_ref.get_untracked() else {
                return false;
            };
            let canvas: &HtmlCanvasElement = &canvas;

            let Some(parent) = canvas.parent_element() else {
                return false;
            };
            let css_w = parent.client_width().max(1) as u32;
            let css_h = parent.client_height().max(1) as u32;

            let scale = gpu_render_scale(css_w, css_h);
            let pixel_w = ((css_w as f64) * scale).round().max(1.0) as u32;
            let pixel_h = ((css_h as f64) * scale).round().max(1.0) as u32;
            if canvas.width() != pixel_w {
                canvas.set_width(pixel_w);
            }
            if canvas.height() != pixel_h {
                canvas.set_height(pixel_h);
            }

            let now = js_sys::Date::now();
            let interaction_active = is_dragging.get()
                || now < interaction_deadline.get()
                || active_pointers.borrow().len() > 1;

            let mut renderer_ref = gpu.borrow_mut();
            let Some(renderer) = renderer_ref.as_mut() else {
                return false;
            };
            if show_render_stats {
                renderer_capabilities.set(Some(renderer.capabilities()));
            }

            renderer.resize(pixel_w, pixel_h, scale as f32);
            renderer.thick_cooldown_borders = thick_cooldown_borders.get_untracked();
            renderer.resource_highlight = resource_highlight.get_untracked();
            renderer.use_static_gpu_labels = true;
            renderer.use_full_gpu_text = true;
            renderer.static_show_names = show_names.get_untracked();
            renderer.static_abbreviate_names = abbreviate_names.get_untracked();
            renderer.static_name_color = name_color.get_untracked();
            renderer.static_tag_color = tag_color.get_untracked();
            renderer.show_connections = show_connections.get_untracked();
            renderer.bold_connections = bold_connections.get_untracked();
            renderer.connection_opacity_scale = connection_opacity_scale.get_untracked() as f32;
            renderer.connection_thickness_scale = connection_thickness_scale.get_untracked() as f32;
            let new_readable = readable_font.get_untracked();
            if renderer.use_readable_font != new_readable {
                renderer.use_readable_font = new_readable;
                renderer.rebuild_text_renderer();
            }
            renderer.dynamic_show_countdown = show_countdown.get_untracked();
            renderer.dynamic_show_granular_map_time = show_granular_map_time.get_untracked();
            renderer.dynamic_show_compound_map_time = show_compound_map_time.get_untracked();
            renderer.dynamic_show_resource_icons = show_resource_icons.get_untracked();
            renderer.label_scale_master = label_scale_master.get_untracked() as f32;
            renderer.label_scale_static_tag = label_scale_static_tag.get_untracked() as f32;
            renderer.label_scale_static_name = label_scale_static_name.get_untracked() as f32;
            renderer.label_scale_dynamic = label_scale_dynamic.get_untracked() as f32;
            renderer.label_scale_icons = label_scale_icons.get_untracked() as f32;

            let hovered_name = hovered.get_untracked();
            let selected_name = selected.get_untracked();
            let icon_set = loaded_icons.get_untracked();
            let mode_now = map_mode.get_untracked();
            let reference_time_secs = if mode_now == MapMode::History {
                history_timestamp
                    .get_untracked()
                    .unwrap_or_else(|| chrono::Utc::now().timestamp())
            } else {
                tick.get_untracked()
            };

            let show_mini = !is_mobile.get_untracked() && show_minimap_setting.get_untracked();
            let history_mode = mode_now == MapMode::History;
            let heat_mode = heat_mode_enabled.get_untracked();
            let heat_max = heat_max_take_count.get_untracked();
            let bounds = world_bounds.get();
            let vp_now = viewport.get_untracked();

            loaded_tiles.with_untracked(|tiles| {
                let tile_count = tiles.len();
                let tile_sig = tile_upload_signature(tiles);
                if tile_count != last_tile_count.get() || tile_sig != last_tile_signature.get() {
                    renderer.upload_tiles(tiles);
                    // Tile coverage changes affect geometry/text/icon/connection culling.
                    renderer.mark_dirty(InvalidationReason::Geometry);
                    renderer.mark_dirty(InvalidationReason::StaticLabel);
                    renderer.mark_dirty(InvalidationReason::DynamicLabel);
                    renderer.mark_dirty(InvalidationReason::Resources);
                    last_tile_count.set(tile_count);
                    last_tile_signature.set(tile_sig);
                }
            });

            territories.with_untracked(|territory_map| {
                loaded_tiles.with_untracked(|tiles| {
                    heat_entries_by_territory.with_untracked(|heat_entries| {
                        let frame_input = {
                            let mut builder = scene_builder.borrow_mut();
                            builder.build(RenderFrameInput {
                                vp: &vp_now,
                                territories: territory_map,
                                hovered: &hovered_name,
                                selected: &selected_name,
                                tiles,
                                world_bounds: bounds,
                                now,
                                reference_time_secs,
                                interaction_active,
                                icons: &icon_set,
                                show_minimap: show_mini,
                                history_mode,
                                heat_mode_enabled: heat_mode,
                                heat_entries,
                                heat_max_take_count: heat_max,
                            })
                        };
                        let keep_animating = renderer.render(frame_input);
                        if show_render_stats {
                            frame_metrics.set(renderer.frame_metrics());
                            scene_summary.set(scene_builder.borrow().latest_summary());
                        }
                        keep_animating
                    })
                })
            })
        }
    });
    let scheduler = Rc::new(scheduler);

    // Ensure icon atlas is loaded so HQ crown rendering works even when
    // resource icons are disabled in settings.
    Effect::new({
        let icon_atlas_requested = icon_atlas_requested.clone();
        move || {
            let already_loaded = loaded_icons.with(|icons| icons.is_some());
            if already_loaded || icon_atlas_requested.get() {
                return;
            }
            icon_atlas_requested.set(true);
            icons::load_resource_atlas(loaded_icons);
        }
    });

    // Keep hit-test grid and world bounds updated.
    Effect::new({
        let spatial_grid = spatial_grid.clone();
        let world_bounds = world_bounds.clone();
        let fitted = fitted.clone();
        let scheduler = scheduler.clone();
        let gpu = gpu.clone();
        move || {
            territories.with(|territory_map| {
                let grid = SpatialGrid::build(territory_map);
                let bounds = grid.world_bounds();
                *spatial_grid.borrow_mut() = grid;
                world_bounds.set(bounds);

                if !fitted.get()
                    && let Some((min_x, min_y, max_x, max_y)) = bounds
                {
                    let canvas_w = web_sys::window()
                        .and_then(|w| w.inner_width().ok())
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1200.0);
                    let canvas_h = web_sys::window()
                        .and_then(|w| w.inner_height().ok())
                        .and_then(|v| v.as_f64())
                        .unwrap_or(800.0);
                    viewport
                        .update(|vp| vp.fit_bounds(min_x, min_y, max_x, max_y, canvas_w, canvas_h));
                    fitted.set(true);
                }
            });

            if let Some(renderer) = gpu.borrow_mut().as_mut() {
                renderer.mark_dirty(InvalidationReason::Geometry);
                renderer.mark_dirty(InvalidationReason::StaticLabel);
                renderer.mark_dirty(InvalidationReason::DynamicLabel);
                renderer.mark_dirty(InvalidationReason::Resources);
            }
            scheduler.mark_dirty();
        }
    });

    // Repaint on view-state changes.
    Effect::new({
        let scheduler = scheduler.clone();
        let gpu = gpu.clone();
        move || {
            hovered.track();
            selected.track();
            if let Some(renderer) = gpu.borrow_mut().as_mut() {
                renderer.mark_dirty(InvalidationReason::Geometry);
            }
            scheduler.mark_dirty();
        }
    });

    Effect::new({
        let scheduler = scheduler.clone();
        let gpu = gpu.clone();
        move || {
            label_scale_master.track();
            label_scale_static_tag.track();
            label_scale_static_name.track();
            label_scale_dynamic.track();
            label_scale_icons.track();
            if let Some(renderer) = gpu.borrow_mut().as_mut() {
                renderer.mark_dirty(InvalidationReason::StaticLabel);
                renderer.mark_dirty(InvalidationReason::DynamicLabel);
                renderer.mark_dirty(InvalidationReason::Resources);
            }
            scheduler.mark_dirty();
        }
    });

    Effect::new({
        let scheduler = scheduler.clone();
        let gpu = gpu.clone();
        move || {
            show_names.track();
            abbreviate_names.track();
            show_countdown.track();
            show_granular_map_time.track();
            show_compound_map_time.track();
            show_connections.track();
            bold_connections.track();
            connection_opacity_scale.track();
            connection_thickness_scale.track();
            readable_font.track();
            name_color.track();
            tag_color.track();
            resource_highlight.track();
            show_resource_icons.track();
            thick_cooldown_borders.track();
            heat_mode_enabled.track();
            heat_entries_by_territory.track();
            heat_max_take_count.track();
            if let Some(renderer) = gpu.borrow_mut().as_mut() {
                renderer.mark_dirty(InvalidationReason::Geometry);
                renderer.mark_dirty(InvalidationReason::StaticLabel);
                renderer.mark_dirty(InvalidationReason::DynamicLabel);
                renderer.mark_dirty(InvalidationReason::Resources);
            }
            scheduler.mark_dirty();
        }
    });

    Effect::new({
        let scheduler = scheduler.clone();
        let gpu = gpu.clone();
        move || {
            viewport.track();
            map_mode.track();
            history_timestamp.track();
            tick.track();
            is_mobile.track();
            show_minimap_setting.track();
            loaded_tiles.track();
            loaded_icons.track();
            if let Some(renderer) = gpu.borrow_mut().as_mut() {
                renderer.mark_dirty(InvalidationReason::Viewport);
            }
            scheduler.mark_dirty();
        }
    });

    // Initialize GPU renderer once the canvas is mounted.
    Effect::new({
        let gpu = gpu.clone();
        let gpu_init_started = gpu_init_started.clone();
        let scheduler = scheduler.clone();
        move || {
            let Some(canvas) = canvas_ref.get() else {
                return;
            };
            if gpu_init_started.get() {
                return;
            }
            gpu_init_started.set(true);

            wasm_bindgen_futures::spawn_local({
                let gpu = gpu.clone();
                let scheduler = scheduler.clone();
                async move {
                    match GpuRenderer::init(canvas).await {
                        Ok(mut renderer) => {
                            renderer.use_full_gpu_text = true;
                            renderer.use_static_gpu_labels = true;
                            renderer.static_name_color = name_color.get_untracked();
                            renderer.label_scale_master = label_scale_master.get_untracked() as f32;
                            renderer.label_scale_static_tag =
                                label_scale_static_tag.get_untracked() as f32;
                            renderer.label_scale_static_name =
                                label_scale_static_name.get_untracked() as f32;
                            renderer.label_scale_dynamic =
                                label_scale_dynamic.get_untracked() as f32;
                            renderer.label_scale_icons = label_scale_icons.get_untracked() as f32;
                            renderer.mark_dirty(InvalidationReason::Geometry);
                            renderer.mark_dirty(InvalidationReason::StaticLabel);
                            renderer.mark_dirty(InvalidationReason::DynamicLabel);
                            renderer.mark_dirty(InvalidationReason::Resources);
                            *gpu.borrow_mut() = Some(renderer);
                            scheduler.mark_dirty();
                        }
                        Err(e) => {
                            web_sys::console::error_1(
                                &format!("wgpu init failed (fail-closed): {e}").into(),
                            );
                            gpu_error.set(Some(e));
                        }
                    }
                }
            });
        }
    });

    let update_hover_from_screen = {
        let spatial_grid = spatial_grid.clone();
        let scheduler = scheduler.clone();
        move |sx: f64, sy: f64| {
            let vp = viewport.get_untracked();
            let (wx, wy) = vp.screen_to_world(sx, sy);
            let hit = spatial_grid.borrow().find_at(wx, wy);
            if hovered.get_untracked() != hit {
                hovered.set(hit.clone());
                if is_mobile.get_untracked() {
                    peek_territory.set(hit);
                }
                scheduler.mark_dirty();
            }
        }
    };

    let jump_from_minimap: Rc<dyn Fn(f64, f64, f64, f64, bool) -> bool> = Rc::new({
        let world_bounds = world_bounds.clone();
        move |sx: f64, sy: f64, canvas_w: f64, canvas_h: f64, history_mode: bool| -> bool {
            if is_mobile.get_untracked() || !show_minimap_setting.get_untracked() {
                return false;
            }
            let (mx, my, mw, mh) = minimap_rect(canvas_w, canvas_h, history_mode);
            if sx < mx || sx > mx + mw || sy < my || sy > my + mh {
                return false;
            }

            let (world_min_x, world_min_y, world_max_x, world_max_y) =
                world_bounds.get().unwrap_or(DEFAULT_MINIMAP_WORLD);
            let world_x =
                world_min_x + ((sx - mx) / mw).clamp(0.0, 1.0) * (world_max_x - world_min_x);
            let world_y =
                world_min_y + ((sy - my) / mh).clamp(0.0, 1.0) * (world_max_y - world_min_y);
            viewport.update(|vp| {
                vp.offset_x = canvas_w * 0.5 - world_x * vp.scale;
                vp.offset_y = canvas_h * 0.5 - world_y * vp.scale;
            });
            true
        }
    });

    let on_pointer_down = {
        let active_pointers = active_pointers.clone();
        let drag_pointer_id = drag_pointer_id.clone();
        let is_dragging = is_dragging.clone();
        let drag_moved = drag_moved.clone();
        let last_x = last_x.clone();
        let last_y = last_y.clone();
        let pinch_last_dist = pinch_last_dist.clone();
        let pinch_last_cx = pinch_last_cx.clone();
        let pinch_last_cy = pinch_last_cy.clone();
        let interaction_deadline = interaction_deadline.clone();
        let scheduler = scheduler.clone();
        let jump_from_minimap = jump_from_minimap.clone();

        move |event: PointerEvent| {
            event.prevent_default();
            let (sx, sy) = pointer_canvas_coords(&event);
            let (canvas_w, canvas_h) = pointer_canvas_size(&event);
            mouse_pos.set((sx, sy));

            let pointer_id = event.pointer_id();
            active_pointers.borrow_mut().insert(pointer_id, (sx, sy));

            if active_pointers.borrow().len() == 2 {
                let pointers_ref = active_pointers.borrow();
                let mut points = pointers_ref.values().copied();
                if let (Some((ax, ay)), Some((bx, by))) = (points.next(), points.next()) {
                    let dx = bx - ax;
                    let dy = by - ay;
                    pinch_last_dist.set((dx * dx + dy * dy).sqrt());
                    pinch_last_cx.set((ax + bx) * 0.5);
                    pinch_last_cy.set((ay + by) * 0.5);
                }
                is_dragging.set(false);
                drag_pointer_id.set(None);
                scheduler.mark_dirty();
                return;
            }

            let history_mode = map_mode.get_untracked() == MapMode::History;
            if event.button() == 0 && jump_from_minimap(sx, sy, canvas_w, canvas_h, history_mode) {
                interaction_deadline.set(js_sys::Date::now() + INTERACTION_SETTLE_MS);
                scheduler.mark_dirty();
                return;
            }

            drag_pointer_id.set(Some(pointer_id));
            is_dragging.set(true);
            drag_moved.set(false);
            last_x.set(sx);
            last_y.set(sy);
            interaction_deadline.set(js_sys::Date::now() + INTERACTION_SETTLE_MS);

            if let Some(canvas) = event
                .target()
                .and_then(|t| t.dyn_into::<HtmlCanvasElement>().ok())
            {
                let _ = canvas.set_pointer_capture(pointer_id);
            }
            scheduler.mark_dirty();
        }
    };

    let on_pointer_move = {
        let active_pointers = active_pointers.clone();
        let drag_pointer_id = drag_pointer_id.clone();
        let is_dragging = is_dragging.clone();
        let drag_moved = drag_moved.clone();
        let last_x = last_x.clone();
        let last_y = last_y.clone();
        let pinch_last_dist = pinch_last_dist.clone();
        let pinch_last_cx = pinch_last_cx.clone();
        let pinch_last_cy = pinch_last_cy.clone();
        let interaction_deadline = interaction_deadline.clone();
        let scheduler = scheduler.clone();

        move |event: PointerEvent| {
            let (sx, sy) = pointer_canvas_coords(&event);
            mouse_pos.set((sx, sy));

            let pointer_id = event.pointer_id();
            if let Some(point) = active_pointers.borrow_mut().get_mut(&pointer_id) {
                *point = (sx, sy);
            }

            if active_pointers.borrow().len() >= 2 {
                let pointers_ref = active_pointers.borrow();
                let mut points = pointers_ref.values().copied();
                if let (Some((ax, ay)), Some((bx, by))) = (points.next(), points.next()) {
                    let cx = (ax + bx) * 0.5;
                    let cy = (ay + by) * 0.5;
                    let dx = bx - ax;
                    let dy = by - ay;
                    let dist = (dx * dx + dy * dy).sqrt();
                    let prev_dist = pinch_last_dist.get();
                    let prev_cx = pinch_last_cx.get();
                    let prev_cy = pinch_last_cy.get();

                    if prev_dist > 0.0 {
                        let zoom_delta = (prev_dist - dist) * 2.2;
                        viewport.update(|vp| {
                            vp.pan(cx - prev_cx, cy - prev_cy);
                            vp.zoom_at(zoom_delta, cx, cy);
                        });
                        drag_moved.set(true);
                    }

                    pinch_last_dist.set(dist);
                    pinch_last_cx.set(cx);
                    pinch_last_cy.set(cy);
                    interaction_deadline.set(js_sys::Date::now() + INTERACTION_SETTLE_MS);
                    scheduler.mark_dirty();
                }
                return;
            }

            if is_dragging.get() && drag_pointer_id.get() == Some(pointer_id) {
                let dx = sx - last_x.get();
                let dy = sy - last_y.get();
                if dx.abs() > 0.0 || dy.abs() > 0.0 {
                    viewport.update(|vp| vp.pan(dx, dy));
                    last_x.set(sx);
                    last_y.set(sy);
                    drag_moved.set(true);
                    interaction_deadline.set(js_sys::Date::now() + INTERACTION_SETTLE_MS);
                    scheduler.mark_dirty();
                }
                return;
            }

            update_hover_from_screen(sx, sy);
        }
    };

    let handle_pointer_end: Rc<dyn Fn(PointerEvent)> = Rc::new({
        let active_pointers = active_pointers.clone();
        let drag_pointer_id = drag_pointer_id.clone();
        let is_dragging = is_dragging.clone();
        let pinch_last_dist = pinch_last_dist.clone();
        let interaction_deadline = interaction_deadline.clone();
        let scheduler = scheduler.clone();

        move |event: PointerEvent| {
            let pointer_id = event.pointer_id();
            active_pointers.borrow_mut().remove(&pointer_id);

            if active_pointers.borrow().len() < 2 {
                pinch_last_dist.set(0.0);
            }

            if drag_pointer_id.get() == Some(pointer_id) {
                drag_pointer_id.set(None);
                is_dragging.set(false);
                if let Some(canvas) = event
                    .target()
                    .and_then(|t| t.dyn_into::<HtmlCanvasElement>().ok())
                {
                    let _ = canvas.release_pointer_capture(pointer_id);
                }
            }

            interaction_deadline.set(js_sys::Date::now() + INTERACTION_SETTLE_MS);
            scheduler.mark_dirty();
        }
    });

    let on_pointer_up = {
        let handle_pointer_end = handle_pointer_end.clone();
        move |event: PointerEvent| handle_pointer_end(event)
    };

    let on_pointer_cancel = {
        let handle_pointer_end = handle_pointer_end.clone();
        move |event: PointerEvent| handle_pointer_end(event)
    };

    let on_click = {
        let drag_moved = drag_moved.clone();
        let scheduler = scheduler.clone();
        let jump_from_minimap = jump_from_minimap.clone();
        move |event: MouseEvent| {
            let (sx, sy) = mouse_canvas_coords(&event);
            let (canvas_w, canvas_h) = mouse_canvas_size(&event);

            if drag_moved.get() {
                drag_moved.set(false);
                return;
            }

            let history_mode = map_mode.get_untracked() == MapMode::History;
            if jump_from_minimap(sx, sy, canvas_w, canvas_h, history_mode) {
                scheduler.mark_dirty();
                return;
            }

            let vp = viewport.get_untracked();
            let (wx, wy) = vp.screen_to_world(sx, sy);
            let hit = spatial_grid.borrow().find_at(wx, wy);
            if hit.is_some() {
                if !sidebar_open.get_untracked() {
                    sidebar_open.set(true);
                    sidebar_transient.set(true);
                }
            } else if sidebar_transient.get_untracked() {
                sidebar_open.set(false);
                sidebar_transient.set(false);
            }
            detail_return_guild.set(None);
            selected.set(hit.clone());
            if is_mobile.get_untracked() {
                peek_territory.set(hit);
            }
            scheduler.mark_dirty();
        }
    };

    let on_wheel = {
        let interaction_deadline = interaction_deadline.clone();
        let scheduler = scheduler.clone();
        let gpu = gpu.clone();
        let wheel_classifier = wheel_classifier.clone();
        move |event: WheelEvent| {
            event.prevent_default();
            let (sx, sy) = wheel_canvas_coords(&event);
            let (_, canvas_h) = wheel_canvas_size(&event);
            mouse_pos.set((sx, sy));
            let now = js_sys::Date::now();
            let sample = WheelSample {
                delta_x: event.delta_x(),
                delta_y: event.delta_y(),
                delta_mode: event.delta_mode(),
                timestamp_ms: now,
            };
            let ctrl_pinch = event.ctrl_key();
            let zoom_delta = normalize_wheel_zoom_delta(
                sample,
                canvas_h,
                ctrl_pinch,
                &mut wheel_classifier.borrow_mut(),
            );
            viewport.update(|vp| vp.zoom_at(zoom_delta, sx, sy));
            if let Some(renderer) = gpu.borrow_mut().as_mut() {
                renderer.mark_dirty(InvalidationReason::Viewport);
            }
            interaction_deadline.set(now + INTERACTION_SETTLE_MS);
            scheduler.mark_dirty();
        }
    };

    let on_pointer_leave = {
        let active_pointers = active_pointers.clone();
        let is_dragging = is_dragging.clone();
        let scheduler = scheduler.clone();
        move |_| {
            if !is_dragging.get() && active_pointers.borrow().is_empty() {
                hovered.set(None);
                if is_mobile.get_untracked() {
                    peek_territory.set(None);
                }
                scheduler.mark_dirty();
            }
        }
    };

    view! {
        <div style="position: absolute; inset: 0;">
            <canvas
                node_ref=canvas_ref
                style="position: absolute; inset: 0; width: 100%; height: 100%; touch-action: none; user-select: none; cursor: grab;"
                on:pointerdown=on_pointer_down
                on:pointermove=on_pointer_move
                on:pointerup=on_pointer_up
                on:pointercancel=on_pointer_cancel
                on:pointerleave=on_pointer_leave
                on:wheel=on_wheel
                on:click=on_click
                on:contextmenu=move |event| event.prevent_default()
            />
            {move || {
                gpu_error.get().map(|message| {
                    let token = diagnostics_token(&message);
                    view! {
                        <div style="position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; background: rgba(12, 14, 23, 0.96); z-index: 30;">
                            <div style="max-width: 640px; margin: 0 24px; border: 1px solid #3a3f5c; background: #13161f; box-shadow: 0 24px 64px rgba(0,0,0,0.55); border-radius: 8px; padding: 22px 20px;">
                                <div style="font-family: 'Silkscreen', monospace; color: #f5c542; letter-spacing: 0.08em; font-size: 0.78rem; text-transform: uppercase; margin-bottom: 8px;">
                                    "Unsupported GPU Configuration"
                                </div>
                                <div style="font-family: 'Inter', system-ui, sans-serif; color: #e2e0d8; line-height: 1.45; font-size: 0.92rem;">
                                    "The map renderer requires wgpu/WebGL2 and does not provide a Canvas2D fallback."
                                </div>
                                <div style="margin-top: 10px; font-family: 'JetBrains Mono', monospace; font-size: 0.74rem; color: #9a9590; word-break: break-word;">
                                    {message}
                                </div>
                                <div style="margin-top: 12px; font-family: 'JetBrains Mono', monospace; font-size: 0.7rem; color: #5a5860;">
                                    "Diagnostics token: "
                                    <span style="color: #f5c542;">{token}</span>
                                </div>
                            </div>
                        </div>
                    }
                })
            }}
            {move || {
                if !show_render_stats {
                    return ().into_any();
                }
                let caps = renderer_capabilities.get();
                let metrics = frame_metrics.get();
                let scene = scene_summary.get();
                let caps_str = caps
                    .map(|c| {
                        format!(
                            "webgl2={} msdf={} dynamic={} fallback={}",
                            c.webgl2, c.gpu_text_msdf, c.gpu_dynamic_labels, c.compatibility_fallback
                        )
                    })
                    .unwrap_or_else(|| "renderer=initializing".to_string());
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
                let scene_line = format!(
                    "scene: terr={} tiles={} hovered={} selected={} interact={} mini={} history={} heat={} hmax={} t={}",
                    scene.territory_count,
                    scene.tile_count,
                    scene.has_hovered,
                    scene.has_selected,
                    scene.interaction_active,
                    scene.show_minimap,
                    scene.history_mode,
                    scene.heat_mode_enabled,
                    scene.heat_max_take_count,
                    scene.reference_time_secs
                );
                view! {
                    <div style="position: absolute; top: 10px; left: 10px; z-index: 25; pointer-events: none; background: rgba(8,10,18,0.78); border: 1px solid rgba(245,197,66,0.35); border-radius: 6px; padding: 6px 8px; color: #e2e0d8; font-family: 'JetBrains Mono', monospace; font-size: 0.66rem; line-height: 1.35;">
                        <div>{summary}</div>
                        <div style="color: #c9c3b8;">{scene_line}</div>
                        <div style="color: #9a9590;">{caps_str}</div>
                    </div>
                }
                .into_any()
            }}
            {move || {
                if !heat_mode_enabled.get() {
                    return ().into_any();
                }
                let max_count = heat_max_take_count.get();
                let label = heat_window_label.get();
                view! {
                    <div style="position: absolute; top: 16px; left: 16px; z-index: 22; pointer-events: none; background: rgba(10,12,20,0.82); border: 1px solid rgba(245,197,66,0.25); border-radius: 6px; padding: 8px 10px; min-width: 172px;">
                        <div style="font-family: 'Silkscreen', monospace; font-size: 0.62rem; letter-spacing: 0.08em; text-transform: uppercase; color: #f5c542; margin-bottom: 5px;">"Heat"</div>
                        <div style="height: 8px; border-radius: 0; background: linear-gradient(90deg, #1e50dc 0%, #28c8f0 25%, #f5dc46 50%, #f58c32 75%, #dc2823 100%);" />
                        <div style="margin-top: 6px; display: flex; justify-content: space-between; font-family: 'JetBrains Mono', monospace; font-size: 0.62rem; color: #9a9590;">
                            <span>"Low"</span>
                            <span>{format!("Max {max_count}")}</span>
                        </div>
                        <div style="margin-top: 4px; font-family: 'JetBrains Mono', monospace; font-size: 0.6rem; color: #6f748f; line-height: 1.25;">
                            {label}
                        </div>
                    </div>
                }.into_any()
            }}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PINCH_LINE_HEIGHT_PX, PINCH_ZOOM_GAIN, TRACKPAD_LINE_HEIGHT_PX,
        TRACKPAD_PAGE_HEIGHT_FACTOR, TRACKPAD_ZOOM_CLAMP, TRACKPAD_ZOOM_GAIN,
        TrackpadWheelClassifier, WHEEL_DELTA_MODE_LINE, WHEEL_DELTA_MODE_PAGE,
        WHEEL_DELTA_MODE_PIXEL, WheelSample, normalize_pinch_zoom_delta,
        normalize_trackpad_zoom_delta, normalize_wheel_zoom_delta,
    };

    fn sample(delta_y: f64, delta_mode: u32, timestamp_ms: f64) -> WheelSample {
        WheelSample {
            delta_x: 0.0,
            delta_y,
            delta_mode,
            timestamp_ms,
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        let diff = (actual - expected).abs();
        assert!(
            diff < 1e-9,
            "expected {expected}, got {actual} (diff: {diff})"
        );
    }

    #[test]
    fn keeps_discrete_mouse_wheel_on_legacy_path() {
        let mut classifier = TrackpadWheelClassifier::default();
        let first = sample(100.0, WHEEL_DELTA_MODE_PIXEL, 0.0);
        let second = sample(100.0, WHEEL_DELTA_MODE_PIXEL, 130.0);

        assert!(!classifier.is_trackpad(first));
        assert!(!classifier.is_trackpad(second));
    }

    #[test]
    fn detects_fractional_line_input_as_trackpad() {
        let mut classifier = TrackpadWheelClassifier::default();
        let s = sample(0.35, WHEEL_DELTA_MODE_LINE, 0.0);

        assert!(classifier.is_trackpad(s));
    }

    #[test]
    fn detects_windows_precision_trackpad_bursts() {
        let mut classifier = TrackpadWheelClassifier::default();
        let stream = [
            sample(60.0, WHEEL_DELTA_MODE_PIXEL, 0.0),
            sample(58.0, WHEEL_DELTA_MODE_PIXEL, 16.0),
            sample(61.0, WHEEL_DELTA_MODE_PIXEL, 32.0),
            sample(57.0, WHEEL_DELTA_MODE_PIXEL, 48.0),
        ];

        assert!(!classifier.is_trackpad(stream[0]));
        assert!(!classifier.is_trackpad(stream[1]));
        assert!(!classifier.is_trackpad(stream[2]));
        assert!(classifier.is_trackpad(stream[3]));
    }

    #[test]
    fn keeps_trackpad_classification_during_momentum_tail() {
        let mut classifier = TrackpadWheelClassifier::default();
        assert!(classifier.is_trackpad(sample(0.5, WHEEL_DELTA_MODE_LINE, 10.0)));

        let momentum_tail = sample(90.0, WHEEL_DELTA_MODE_PIXEL, 120.0);
        assert!(classifier.is_trackpad(momentum_tail));
    }

    #[test]
    fn sticky_window_does_not_reclassify_mouse_line_wheel() {
        let mut classifier = TrackpadWheelClassifier::default();
        assert!(classifier.is_trackpad(sample(0.5, WHEEL_DELTA_MODE_LINE, 10.0)));

        let mouse_wheel_event = sample(3.0, WHEEL_DELTA_MODE_LINE, 80.0);
        assert!(!classifier.is_trackpad(mouse_wheel_event));
    }

    #[test]
    fn sticky_window_does_not_reclassify_large_pixel_mouse_ticks() {
        let mut classifier = TrackpadWheelClassifier::default();
        assert!(classifier.is_trackpad(sample(0.4, WHEEL_DELTA_MODE_LINE, 10.0)));

        let mouse_wheel_event = sample(120.0, WHEEL_DELTA_MODE_PIXEL, 90.0);
        assert!(!classifier.is_trackpad(mouse_wheel_event));
    }

    #[test]
    fn normalizes_line_deltas_to_pixel_zoom_rate() {
        let s = sample(0.5, WHEEL_DELTA_MODE_LINE, 0.0);
        let normalized = normalize_trackpad_zoom_delta(s, 800.0);
        let expected = 0.5 * TRACKPAD_LINE_HEIGHT_PX * TRACKPAD_ZOOM_GAIN;
        assert_close(normalized, expected);
    }

    #[test]
    fn clamps_large_page_deltas() {
        let s = sample(1.0, WHEEL_DELTA_MODE_PAGE, 0.0);
        let normalized = normalize_trackpad_zoom_delta(s, 1000.0);
        let unclamped = 1000.0 * TRACKPAD_PAGE_HEIGHT_FACTOR * TRACKPAD_ZOOM_GAIN;
        assert!(unclamped > TRACKPAD_ZOOM_CLAMP);
        assert_close(normalized, TRACKPAD_ZOOM_CLAMP);
    }

    #[test]
    fn ctrl_wheel_uses_pinch_normalization() {
        let mut classifier = TrackpadWheelClassifier::default();
        let s = sample(0.5, WHEEL_DELTA_MODE_LINE, 0.0);
        let normalized = normalize_wheel_zoom_delta(s, 800.0, true, &mut classifier);
        let expected = 0.5 * PINCH_LINE_HEIGHT_PX * PINCH_ZOOM_GAIN;
        assert_close(normalized, expected);
    }

    #[test]
    fn ctrl_line_mode_mouse_wheel_keeps_legacy_delta() {
        let mut classifier = TrackpadWheelClassifier::default();
        let s = sample(3.0, WHEEL_DELTA_MODE_LINE, 0.0);
        let normalized = normalize_wheel_zoom_delta(s, 800.0, true, &mut classifier);
        assert_close(normalized, 3.0);
    }

    #[test]
    fn ctrl_large_pixel_mouse_wheel_keeps_legacy_delta() {
        let mut classifier = TrackpadWheelClassifier::default();
        let s = sample(120.0, WHEEL_DELTA_MODE_PIXEL, 0.0);
        let normalized = normalize_wheel_zoom_delta(s, 800.0, true, &mut classifier);
        assert_close(normalized, 120.0);
    }

    #[test]
    fn discrete_mouse_wheel_keeps_legacy_delta_without_ctrl() {
        let mut classifier = TrackpadWheelClassifier::default();
        let s = sample(120.0, WHEEL_DELTA_MODE_LINE, 0.0);
        let normalized = normalize_wheel_zoom_delta(s, 800.0, false, &mut classifier);
        assert_close(normalized, 120.0);
    }

    #[test]
    fn pinch_normalization_scales_line_mode_aggressively() {
        let s = sample(-0.75, WHEEL_DELTA_MODE_LINE, 0.0);
        let normalized = normalize_pinch_zoom_delta(s, 1000.0);
        let expected = -0.75 * PINCH_LINE_HEIGHT_PX * PINCH_ZOOM_GAIN;
        assert_close(normalized, expected);
    }
}
