//! UI-independent map math, camera gestures, scene invalidation, territory state and label
//! layout shared by both browser clients. The browser and GPU side lives in the
//! `sequoia-browser-map` crate; everything here is host-testable.

pub mod animation;
pub mod claim_labels;
pub mod colors;
pub mod defense;
pub mod gesture;
pub mod label_layout;
pub mod minimap;
pub mod overlay_sizing;
pub mod scene;
pub mod settings;
pub mod spatial;
pub mod territory;
pub mod time_format;
pub mod viewport;
pub mod wheel;

pub mod icon_atlas;
