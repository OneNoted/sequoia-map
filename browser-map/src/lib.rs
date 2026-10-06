//! The interactive territory map shared by the live/history map and the claims editor.
//!
//! A host describes what to show with [`MapInputs`], creates a [`BrowserMap`] and mounts a
//! [`MapCanvas`]. The map owns the camera and pointer gestures, map imagery (tiles and the
//! icon atlas), the GPU renderer and its caches. The host owns product state and reacts to
//! [`MapEvent`]s.

mod assets;
mod camera;
mod canvas;
mod frame;
#[cfg(target_arch = "wasm32")]
mod gpu;
#[cfg(not(target_arch = "wasm32"))]
#[path = "gpu/native.rs"]
mod gpu;
mod icons;
mod input;
pub mod live_feed;
pub mod render_loop;
mod tiles;

use std::collections::{HashMap, HashSet};

use leptos::prelude::*;
use sequoia_map_engine::settings::RenderSettings;
use sequoia_map_engine::territory::ClientTerritoryMap;
use sequoia_map_engine::viewport::Viewport;

pub use assets::versioned_app_asset_url;
pub use camera::MapCamera;
pub use canvas::MapCanvas;
pub use icons::ATLAS_PATH;

/// What the host wants drawn. Everything is reactive; the map repaints on change.
#[derive(Clone, Copy)]
pub struct MapInputs {
    pub territories: Signal<ClientTerritoryMap>,
    pub selected: Signal<Option<String>>,
    pub settings: Signal<RenderSettings>,
    /// Seconds territory timers count against: now, or a history timestamp.
    pub clock_secs: Signal<i64>,
    pub heat: Option<HeatLayer>,
    /// Territories to outline as being at war.
    pub wars: Option<Signal<HashSet<String>>>,
    /// Gap below the minimap in CSS pixels, or `None` to hide it.
    pub minimap_inset: Signal<Option<f32>>,
    /// What primary-button presses on the map do.
    pub edit: Signal<EditMode>,
}

/// Territory recolouring by how often each was taken.
#[derive(Clone, Copy)]
pub struct HeatLayer {
    pub enabled: Signal<bool>,
    pub take_counts: Signal<HashMap<String, u64>>,
    pub max_take_count: Signal<u64>,
}

/// Editing behaviour of primary presses. Presses that do not edit pan the map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EditMode {
    /// Pan and tap only.
    #[default]
    Navigate,
    /// Presses on a territory paint across every territory they pass.
    Stroke,
    /// Presses drag out a selection rectangle.
    Select,
    /// Presses on a territory pick it.
    Pick,
}

/// What happened on the map, for the host to act on.
#[derive(Clone, Debug, PartialEq)]
pub enum MapEvent {
    /// The pointer moved onto another territory, or off all of them.
    Hover(Option<String>),
    /// A tap or click, outside the minimap.
    Tap {
        territory: Option<String>,
        shift: bool,
    },
    /// A paint stroke reached a territory.
    Stroke(String),
    /// A pick press landed on a territory.
    Pick { territory: String, shift: bool },
    /// A selection rectangle covered these territories, sorted by name.
    BoxSelect {
        territories: Vec<String>,
        shift: bool,
    },
    /// A selection press released without dragging.
    SelectTap {
        territory: Option<String>,
        shift: bool,
    },
}

/// Handle to one map: its camera, hover and pointer state, and imagery. Cheap to copy.
#[derive(Clone, Copy)]
pub struct BrowserMap {
    camera: MapCamera,
    hovered: RwSignal<Option<String>>,
    pointer: RwSignal<(f64, f64)>,
    tiles: RwSignal<Vec<tiles::LoadedTile>>,
    icons: RwSignal<Option<icons::ResourceAtlas>>,
    inputs: MapInputs,
}

impl BrowserMap {
    pub fn new(inputs: MapInputs, initial_view: Viewport) -> Self {
        Self {
            camera: MapCamera::new(initial_view),
            hovered: RwSignal::new(None),
            pointer: RwSignal::new((0.0, 0.0)),
            tiles: RwSignal::new(Vec::new()),
            icons: RwSignal::new(None),
            inputs,
        }
    }

    pub fn camera(&self) -> MapCamera {
        self.camera
    }

    /// The territory under the pointer. The map sets it; hosts may clear it.
    pub fn hovered(&self) -> RwSignal<Option<String>> {
        self.hovered
    }

    /// Last pointer position over the map, in canvas CSS pixels.
    pub fn pointer(&self) -> Signal<(f64, f64)> {
        self.pointer.into()
    }

    /// Starts loading map imagery, nearest the current view first.
    pub fn load_tiles(&self) {
        let (width, height) = self.camera.canvas_size();
        tiles::fetch_tiles(
            self.tiles,
            tiles::TileFetchContext::new(self.camera.get_untracked(), width, height),
        );
    }
}
