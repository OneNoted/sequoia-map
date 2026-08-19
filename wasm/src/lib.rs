//! `sequoia-map-engine` — the rendering and math core of the Sequoia map.
//!
//! This crate owns everything that is not UI: the wgpu renderer, the scene
//! model, the viewport/projection math, hit-testing, and label layout. It is
//! consumed two ways:
//!
//! * as a plain `rlib` by the Leptos clients during the migration, and
//! * as a `cdylib` with the `js-api` feature, exposing a `#[wasm_bindgen]`
//!   facade to the Svelte frontend.
//!
//! Modules here must stay free of any UI-framework dependency. Anything that
//! needs the DOM belongs behind `cfg(target_arch = "wasm32")`; anything that
//! merely computes should stay host-testable so `cargo test` keeps covering it.

pub mod animation;
pub mod claim_labels;
pub mod colors;
pub mod defense;
pub mod label_layout;
pub mod overlay_sizing;
pub mod settings;
pub mod spatial;
pub mod territory;
pub mod time_format;
pub mod viewport;
