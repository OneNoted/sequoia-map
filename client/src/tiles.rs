#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;

use js_sys::Reflect;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::HtmlImageElement;

use crate::assets::versioned_app_asset_url;
use crate::viewport::Viewport;

const HQ_CONCURRENCY: usize = 6;
const LQ_CONCURRENCY: usize = 6;
const HQ_UPGRADE_CONCURRENCY: usize = 2;
const NEAR_PREFETCH_PADDING_WORLD: f64 = 1024.0;
const ONLOAD_HANDLE_KEY: &str = "__sequoiaTileOnload";
const ONERROR_HANDLE_KEY: &str = "__sequoiaTileOnerror";

type IdleCallback = Rc<dyn Fn()>;
type SharedIdleCallback = Rc<RefCell<Option<IdleCallback>>>;
type WorldBounds = (f64, f64, f64, f64);
type SharedRequestedJobs = Rc<RefCell<HashSet<(usize, TileQuality)>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TileQuality {
    Low,
    High,
}

#[derive(Debug, Clone)]
pub struct TileFetchContext {
    viewport: Viewport,
    canvas_width: f64,
    canvas_height: f64,
}

impl TileFetchContext {
    pub fn new(viewport: Viewport, canvas_width: f64, canvas_height: f64) -> Self {
        Self {
            viewport,
            canvas_width: canvas_width.max(1.0),
            canvas_height: canvas_height.max(1.0),
        }
    }

    fn viewport_bounds(&self) -> WorldBounds {
        let (wx1, wz1) = self.viewport.screen_to_world(0.0, 0.0);
        let (wx2, wz2) = self
            .viewport
            .screen_to_world(self.canvas_width, self.canvas_height);
        (wx1.min(wx2), wz1.min(wz2), wx1.max(wx2), wz1.max(wz2))
    }
}

/// A loaded map tile image with its world coordinate bounds.
#[derive(Clone)]
pub struct LoadedTile {
    pub id: usize,
    pub quality: TileQuality,
    pub image: HtmlImageElement,
    pub x1: i32,
    pub z1: i32,
    pub x2: i32,
    pub z2: i32,
}

/// Static tile definitions: (filename, start_x, start_z, end_x, end_z).
/// Coordinates from wynnmap (Zatzou/wynnmap) — Main grid + Realm of Light.
const TILES: &[(&str, i32, i32, i32, i32)] = &[
    ("main-1-1.webp", -2560, -6144, -1025, -5121),
    ("main-1-2.webp", -1024, -6144, 1023, -5121),
    ("main-1-3.webp", 1024, -6144, 2047, -5121),
    ("main-2-1.webp", -2560, -5120, -1025, -4097),
    ("main-2-2.webp", -1024, -5120, 1023, -4097),
    ("main-2-3.webp", 1024, -5120, 2047, -4097),
    ("main-3-1.webp", -2560, -4096, -1025, -3073),
    ("main-3-2.webp", -1024, -4096, 1023, -3073),
    ("main-3-3.webp", 1024, -4096, 2047, -3073),
    ("main-4-1.webp", -2560, -3072, -1025, -2049),
    ("main-4-2.webp", -1024, -3072, 1023, -2049),
    ("main-4-3.webp", 1024, -3072, 2047, -2049),
    ("main-5-1.webp", -2560, -2048, -1025, -1025),
    ("main-5-2.webp", -1024, -2048, 1023, -1025),
    ("main-5-3.webp", 1024, -2048, 2047, -1025),
    ("main-6-1.webp", -2560, -1024, -1025, -1),
    ("main-6-2.webp", -1024, -1024, 1023, -1),
    ("main-6-3.webp", 1024, -1024, 2047, -1),
    ("realm-of-light.webp", -1536, -6656, -513, -5633),
];

#[derive(Clone, Copy)]
struct LoadJob {
    id: usize,
    filename: &'static str,
    quality: TileQuality,
    x1: i32,
    z1: i32,
    x2: i32,
    z2: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupTileMode {
    LowOnly,
    HighOnly,
    ProgressiveLowToHigh,
}

fn select_startup_tile_mode(
    save_data: bool,
    effective_type: Option<&str>,
    downlink_mbps: f64,
) -> StartupTileMode {
    if save_data {
        return StartupTileMode::LowOnly;
    }

    match effective_type {
        Some("slow-2g" | "2g" | "3g") => StartupTileMode::LowOnly,
        Some("4g") => {
            if downlink_mbps >= 8.0 {
                StartupTileMode::HighOnly
            } else {
                StartupTileMode::ProgressiveLowToHigh
            }
        }
        // If NetworkInformation is unavailable or unknown, avoid getting stuck in
        // low-res tiles forever: start low and upgrade to high in background.
        _ => StartupTileMode::ProgressiveLowToHigh,
    }
}

struct LoadPhase {
    jobs: VecDeque<LoadJob>,
    max_concurrency: usize,
}

#[derive(Default)]
struct JobBuckets {
    visible: Vec<LoadJob>,
    near_visible: Vec<LoadJob>,
    far: Vec<LoadJob>,
}

/// Load local map tile images from static assets.
pub fn fetch_tiles(tiles_signal: RwSignal<Vec<LoadedTile>>, context: TileFetchContext) {
    tiles_signal.set(Vec::new());

    let startup_mode = detect_startup_tile_mode();
    let low_jobs = make_job_buckets(TileQuality::Low, &context);
    let high_jobs = make_job_buckets(TileQuality::High, &context);
    let phases = match startup_mode {
        StartupTileMode::LowOnly => vec![
            LoadPhase {
                jobs: low_jobs.visible.into(),
                max_concurrency: LQ_CONCURRENCY,
            },
            LoadPhase {
                jobs: low_jobs.near_visible.into(),
                max_concurrency: LQ_CONCURRENCY,
            },
            LoadPhase {
                jobs: low_jobs.far.into(),
                max_concurrency: LQ_CONCURRENCY,
            },
        ],
        StartupTileMode::HighOnly => {
            vec![
                LoadPhase {
                    jobs: high_jobs.visible.into(),
                    max_concurrency: HQ_CONCURRENCY,
                },
                LoadPhase {
                    jobs: high_jobs.near_visible.into(),
                    max_concurrency: HQ_CONCURRENCY,
                },
                LoadPhase {
                    jobs: high_jobs.far.into(),
                    max_concurrency: HQ_CONCURRENCY,
                },
            ]
        }
        StartupTileMode::ProgressiveLowToHigh => {
            vec![
                LoadPhase {
                    jobs: low_jobs.visible.into(),
                    max_concurrency: LQ_CONCURRENCY,
                },
                LoadPhase {
                    jobs: low_jobs.near_visible.into(),
                    max_concurrency: LQ_CONCURRENCY,
                },
                LoadPhase {
                    jobs: low_jobs.far.into(),
                    max_concurrency: LQ_CONCURRENCY,
                },
                LoadPhase {
                    jobs: high_jobs.visible.into(),
                    max_concurrency: HQ_UPGRADE_CONCURRENCY,
                },
                LoadPhase {
                    jobs: high_jobs.near_visible.into(),
                    max_concurrency: HQ_UPGRADE_CONCURRENCY,
                },
                LoadPhase {
                    jobs: high_jobs.far.into(),
                    max_concurrency: HQ_UPGRADE_CONCURRENCY,
                },
            ]
        }
    };

    start_phased_queues(tiles_signal, phases);
}

fn detect_startup_tile_mode() -> StartupTileMode {
    let Some(window) = web_sys::window() else {
        return StartupTileMode::ProgressiveLowToHigh;
    };

    let Ok(navigator) = Reflect::get(window.as_ref(), &JsValue::from_str("navigator")) else {
        return StartupTileMode::ProgressiveLowToHigh;
    };
    let Ok(connection) = Reflect::get(&navigator, &JsValue::from_str("connection")) else {
        return StartupTileMode::ProgressiveLowToHigh;
    };

    let save_data = Reflect::get(&connection, &JsValue::from_str("saveData"))
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if save_data {
        return StartupTileMode::LowOnly;
    }

    let effective_type = Reflect::get(&connection, &JsValue::from_str("effectiveType"))
        .ok()
        .and_then(|value| value.as_string())
        .map(|value| value.to_ascii_lowercase());

    let downlink_mbps = Reflect::get(&connection, &JsValue::from_str("downlink"))
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);

    select_startup_tile_mode(save_data, effective_type.as_deref(), downlink_mbps)
}

fn start_phased_queues(tiles_signal: RwSignal<Vec<LoadedTile>>, phases: Vec<LoadPhase>) {
    let phases: VecDeque<_> = phases
        .into_iter()
        .filter(|phase| !phase.jobs.is_empty())
        .collect();
    if phases.is_empty() {
        return;
    }
    let phase_queue = Rc::new(RefCell::new(phases));
    let requested_jobs: SharedRequestedJobs = Rc::new(RefCell::new(HashSet::new()));
    start_next_phase(tiles_signal, phase_queue, requested_jobs);
}

fn start_next_phase(
    tiles_signal: RwSignal<Vec<LoadedTile>>,
    phase_queue: Rc<RefCell<VecDeque<LoadPhase>>>,
    requested_jobs: SharedRequestedJobs,
) {
    let Some(phase) = phase_queue.borrow_mut().pop_front() else {
        return;
    };

    let phase_queue_next = phase_queue.clone();
    let requested_jobs_next = requested_jobs.clone();
    let on_idle: IdleCallback = Rc::new(move || {
        start_next_phase(
            tiles_signal,
            phase_queue_next.clone(),
            requested_jobs_next.clone(),
        );
    });

    start_queue(
        tiles_signal,
        phase.jobs,
        phase.max_concurrency,
        Some(on_idle),
        requested_jobs,
    );
}

fn make_job_buckets(quality: TileQuality, context: &TileFetchContext) -> JobBuckets {
    let mut buckets = JobBuckets::default();
    let viewport_bounds = context.viewport_bounds();
    let near_bounds = expand_bounds(viewport_bounds, NEAR_PREFETCH_PADDING_WORLD);
    let viewport_center = bounds_center(viewport_bounds);

    let mut jobs: Vec<_> = TILES
        .iter()
        .enumerate()
        .map(|(id, &(filename, x1, z1, x2, z2))| LoadJob {
            id,
            filename,
            quality,
            x1,
            z1,
            x2,
            z2,
        })
        .collect();

    for job in jobs.drain(..) {
        if intersects_bounds(&job, viewport_bounds) {
            buckets.visible.push(job);
            continue;
        }
        if intersects_bounds(&job, near_bounds) {
            buckets.near_visible.push(job);
            continue;
        }
        buckets.far.push(job);
    }

    buckets.visible.sort_by(|a, b| {
        distance_sq_to_point(job_center(a), viewport_center)
            .total_cmp(&distance_sq_to_point(job_center(b), viewport_center))
            .then_with(|| a.id.cmp(&b.id))
    });
    buckets.near_visible.sort_by(|a, b| {
        distance_sq_to_bounds(job_center(a), viewport_bounds)
            .total_cmp(&distance_sq_to_bounds(job_center(b), viewport_bounds))
            .then_with(|| {
                distance_sq_to_point(job_center(a), viewport_center)
                    .total_cmp(&distance_sq_to_point(job_center(b), viewport_center))
            })
            .then_with(|| a.id.cmp(&b.id))
    });
    buckets.far.sort_by(|a, b| {
        distance_sq_to_bounds(job_center(a), viewport_bounds)
            .total_cmp(&distance_sq_to_bounds(job_center(b), viewport_bounds))
            .then_with(|| {
                distance_sq_to_point(job_center(a), viewport_center)
                    .total_cmp(&distance_sq_to_point(job_center(b), viewport_center))
            })
            .then_with(|| a.id.cmp(&b.id))
    });

    buckets
}

fn intersects_bounds(job: &LoadJob, bounds: WorldBounds) -> bool {
    let (x1, z1, x2, z2) = job_bounds(job);
    x1 < bounds.2 && x2 > bounds.0 && z1 < bounds.3 && z2 > bounds.1
}

fn bounds_center(bounds: WorldBounds) -> (f64, f64) {
    ((bounds.0 + bounds.2) * 0.5, (bounds.1 + bounds.3) * 0.5)
}

fn expand_bounds(bounds: WorldBounds, padding: f64) -> WorldBounds {
    (
        bounds.0 - padding,
        bounds.1 - padding,
        bounds.2 + padding,
        bounds.3 + padding,
    )
}

fn job_bounds(job: &LoadJob) -> WorldBounds {
    let x1 = job.x1.min(job.x2) as f64;
    let z1 = job.z1.min(job.z2) as f64;
    let x2 = job.x1.max(job.x2) as f64 + 1.0;
    let z2 = job.z1.max(job.z2) as f64 + 1.0;
    (x1, z1, x2, z2)
}

fn job_center(job: &LoadJob) -> (f64, f64) {
    let (x1, z1, x2, z2) = job_bounds(job);
    ((x1 + x2) * 0.5, (z1 + z2) * 0.5)
}

fn distance_sq_to_point(a: (f64, f64), b: (f64, f64)) -> f64 {
    let dx = a.0 - b.0;
    let dz = a.1 - b.1;
    dx * dx + dz * dz
}

fn distance_sq_to_bounds(point: (f64, f64), bounds: WorldBounds) -> f64 {
    let dx = if point.0 < bounds.0 {
        bounds.0 - point.0
    } else if point.0 > bounds.2 {
        point.0 - bounds.2
    } else {
        0.0
    };
    let dz = if point.1 < bounds.1 {
        bounds.1 - point.1
    } else if point.1 > bounds.3 {
        point.1 - bounds.3
    } else {
        0.0
    };
    dx * dx + dz * dz
}

fn start_queue(
    tiles_signal: RwSignal<Vec<LoadedTile>>,
    jobs: VecDeque<LoadJob>,
    max_concurrency: usize,
    on_idle: Option<IdleCallback>,
    requested_jobs: SharedRequestedJobs,
) {
    if jobs.is_empty() {
        if let Some(cb) = on_idle {
            cb();
        }
        return;
    }

    let queue = Rc::new(RefCell::new(jobs));
    let in_flight = Rc::new(Cell::new(0usize));
    let on_idle = Rc::new(RefCell::new(on_idle));
    pump_queue(
        tiles_signal,
        queue,
        in_flight,
        max_concurrency,
        on_idle,
        requested_jobs,
    );
}

fn pump_queue(
    tiles_signal: RwSignal<Vec<LoadedTile>>,
    queue: Rc<RefCell<VecDeque<LoadJob>>>,
    in_flight: Rc<Cell<usize>>,
    max_concurrency: usize,
    on_idle: SharedIdleCallback,
    requested_jobs: SharedRequestedJobs,
) {
    while in_flight.get() < max_concurrency {
        let Some(job) = queue.borrow_mut().pop_front() else {
            break;
        };
        if should_skip_job(tiles_signal, &job, &requested_jobs) {
            continue;
        }
        in_flight.set(in_flight.get() + 1);

        let queue_next = queue.clone();
        let in_flight_next = in_flight.clone();
        let on_idle_next = on_idle.clone();
        let requested_jobs_next = requested_jobs.clone();
        let on_done: IdleCallback = Rc::new(move || {
            in_flight_next.set(in_flight_next.get().saturating_sub(1));
            pump_queue(
                tiles_signal,
                queue_next.clone(),
                in_flight_next.clone(),
                max_concurrency,
                on_idle_next.clone(),
                requested_jobs_next.clone(),
            );
        });

        load_tile_job(tiles_signal, job, on_done);
    }

    if queue.borrow().is_empty()
        && in_flight.get() == 0
        && let Some(cb) = on_idle.borrow_mut().take()
    {
        cb();
    }
}

fn should_skip_job(
    tiles_signal: RwSignal<Vec<LoadedTile>>,
    job: &LoadJob,
    requested_jobs: &SharedRequestedJobs,
) -> bool {
    let key = (job.id, job.quality);
    if requested_jobs.borrow().contains(&key) {
        return true;
    }

    let already_loaded = tiles_signal.with_untracked(|loaded| {
        loaded
            .iter()
            .any(|tile| tile.id == job.id && tile.quality >= job.quality)
    });
    if already_loaded {
        return true;
    }

    requested_jobs.borrow_mut().insert(key);
    false
}

fn load_tile_job(tiles_signal: RwSignal<Vec<LoadedTile>>, job: LoadJob, on_done: Rc<dyn Fn()>) {
    let src = tile_src(job.filename, job.quality);
    let img = match HtmlImageElement::new() {
        Ok(img) => img,
        Err(_) => {
            on_done();
            return;
        }
    };

    let img_for_load = img.clone();
    let on_done_load = on_done.clone();
    let onload = Closure::<dyn FnMut()>::new(move || {
        clear_image_handlers(&img_for_load);

        let img_for_decode = img_for_load.clone();
        let on_done_load = on_done_load.clone();

        wasm_bindgen_futures::spawn_local(async move {
            let _ = JsFuture::from(img_for_decode.decode()).await;
            upsert_tile(
                tiles_signal,
                LoadedTile {
                    id: job.id,
                    quality: job.quality,
                    image: img_for_decode,
                    x1: job.x1,
                    z1: job.z1,
                    x2: job.x2,
                    z2: job.z2,
                },
            );
            on_done_load();
        });
    });

    let img_for_error = img.clone();
    let on_done_error = on_done.clone();
    let onerror = Closure::<dyn FnMut()>::new(move || {
        clear_image_handlers(&img_for_error);
        on_done_error();
    });

    let onload_js = onload.into_js_value();
    let onerror_js = onerror.into_js_value();
    img.set_onload(Some(onload_js.unchecked_ref()));
    img.set_onerror(Some(onerror_js.unchecked_ref()));
    let _ = Reflect::set(
        img.as_ref(),
        &JsValue::from_str(ONLOAD_HANDLE_KEY),
        &onload_js,
    );
    let _ = Reflect::set(
        img.as_ref(),
        &JsValue::from_str(ONERROR_HANDLE_KEY),
        &onerror_js,
    );
    img.set_src(&src);
}

fn clear_image_handlers(img: &HtmlImageElement) {
    img.set_onload(None);
    img.set_onerror(None);
    let _ = Reflect::delete_property(img.as_ref(), &JsValue::from_str(ONLOAD_HANDLE_KEY));
    let _ = Reflect::delete_property(img.as_ref(), &JsValue::from_str(ONERROR_HANDLE_KEY));
}

fn tile_src(filename: &str, quality: TileQuality) -> String {
    match quality {
        TileQuality::Low => versioned_app_asset_url(&format!("tiles/lq/{filename}")),
        TileQuality::High => versioned_app_asset_url(&format!("tiles/{filename}")),
    }
}

fn upsert_tile(tiles_signal: RwSignal<Vec<LoadedTile>>, incoming: LoadedTile) {
    tiles_signal.update(|loaded| {
        if let Some(existing) = loaded.iter_mut().find(|tile| tile.id == incoming.id) {
            if incoming.quality >= existing.quality {
                *existing = incoming;
            }
            return;
        }

        loaded.push(incoming);
        loaded.sort_by_key(|tile| tile.id);
    });
}

#[cfg(test)]
mod tests {
    use super::{StartupTileMode, select_startup_tile_mode};

    #[test]
    fn startup_mode_honors_data_saver() {
        assert_eq!(
            select_startup_tile_mode(true, Some("4g"), 100.0),
            StartupTileMode::LowOnly
        );
    }

    #[test]
    fn startup_mode_uses_low_only_on_slow_links() {
        assert_eq!(
            select_startup_tile_mode(false, Some("3g"), 20.0),
            StartupTileMode::LowOnly
        );
    }

    #[test]
    fn startup_mode_uses_high_only_on_fast_4g() {
        assert_eq!(
            select_startup_tile_mode(false, Some("4g"), 9.0),
            StartupTileMode::HighOnly
        );
    }

    #[test]
    fn startup_mode_uses_progressive_when_network_info_is_unknown() {
        assert_eq!(
            select_startup_tile_mode(false, None, 0.0),
            StartupTileMode::ProgressiveLowToHigh
        );
        assert_eq!(
            select_startup_tile_mode(false, Some("wifi"), 100.0),
            StartupTileMode::ProgressiveLowToHigh
        );
    }
}
