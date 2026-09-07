//! UI-independent map math, territory state and label layout shared by both clients.
//! The GPU renderer lives in `client/src/gpu`; these helpers are host-testable.

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
