//! Undo and redo for the viewer's settings, and resetting any of them to its default.
//!
//! A [`Snapshot`] is the saved settings blob without the panel layout, keyed as it is saved,
//! so every control can name the settings it shows and Undo can name what it puts back. Every
//! change is recorded, whichever control, shortcut or reset made it.

use leptos::prelude::*;
use serde_json::{Map, Value};

/// The saved settings, keyed as saved, without [`NOT_SETTINGS`].
pub(crate) type Snapshot = Map<String, Value>;

/// Saved keys that are not settings: the blob's version, the panels' layout and the
/// leaderboard's sort, which have their own controls outside the menu. Undo leaves them alone.
pub(crate) const NOT_SETTINGS: &[&str] = &[
    "defaults_version",
    "sidebar_open",
    "sidebar_width",
    "war_panel_open",
    "war_panel_width",
    "leaderboard_sort_by_sr",
];

/// What each saved setting is called in the menu, for "Undo Territory opacity". A saved object
/// holding several controls has one entry per field.
pub(crate) const LABELS: &[(&str, &str)] = &[
    ("show_connections", "Connections"),
    ("abbreviate_names", "Abbreviate Names"),
    ("show_countdown", "Cooldown Countdown"),
    ("granular_map_time", "Time held"),
    ("compound_map_time", "Time held"),
    ("show_names", "Territory Names"),
    ("show_claim_labels", "When zoomed out"),
    ("show_far_zoom_territory_tags", "When zoomed out"),
    ("thick_cooldown_borders", "Thick Cooldown Borders"),
    ("bold_connections", "Bold Connections"),
    ("connection_style", "Connection style"),
    ("connection_opacity_scale", "Connection opacity"),
    ("connection_solid_opacity", "Connection opacity"),
    ("connection_thickness_scale", "Connection thickness"),
    ("show_war_queue", "War Panel"),
    ("show_war_markers", "War Markers"),
    ("activity_include_unconfirmed", "Unconfirmed Activity"),
    ("show_player_heads", "Teammate Heads"),
    ("player_head_render_head", "Show Faces"),
    ("player_head_render_label", "Teammate names"),
    ("player_head_size", "Head size"),
    ("resource_highlight", "Overlay"),
    ("defense_highlight", "Overlay"),
    ("map_intel_enabled", "Overlay"),
    ("resource_highlight_opacity", "Resource opacity"),
    ("territory_fill_opacity", "Territory opacity"),
    ("show_resource_icons", "Resource Icons"),
    ("show_territory_ornaments", "Corner Ornaments"),
    ("hq_crown_style", "HQ crown"),
    ("cooldown_display", "Cooldown"),
    ("hq_crown_scale", "Crown size"),
    ("war_outline.style", "War outline"),
    ("war_outline.color", "Outline color"),
    ("war_outline.progress", "Queue Progress"),
    ("war_outline.width", "Outline width"),
    ("war_outline.opacity", "Outline opacity"),
    ("manual_sr_scalar", "Manual SR Scalar"),
    ("auto_sr_scalar_enabled", "Estimate SR Scalar"),
    ("show_leaderboard_sr_gain", "SR Rate"),
    ("show_leaderboard_sr_value", "Season Rating"),
    ("show_leaderboard_territory_count", "Territories"),
    ("show_leaderboard_online", "Online Members"),
    ("heat_mode_enabled", "Heat Map"),
    ("heat_live_source", "Heat period"),
    ("heat_history_basis", "Heat period"),
    ("heat_selected_season_id", "Season"),
    ("readable_font", "Readable Font"),
    ("show_minimap", "Minimap"),
    ("name_color", "Territory name color"),
    ("tag_color", "Guild tag color"),
    ("label_scale_master", "Label size"),
    ("label_scale_static", "Guild tag size"),
    ("label_scale_static_name", "Territory name size"),
    ("label_scale_dynamic", "Timer size"),
    ("label_scale_icons", "Icon size"),
    ("show_debug_info", "Diagnostics"),
];

/// The settings in a saved blob.
pub(crate) fn snapshot(saved: Value) -> Snapshot {
    let Value::Object(mut map) = saved else {
        return Map::new();
    };
    for key in NOT_SETTINGS {
        map.remove(*key);
    }
    map
}

/// One setting's value. `war_outline.width` reaches into the outline, which is saved whole.
fn value_at<'a>(snapshot: &'a Snapshot, key: &str) -> Option<&'a Value> {
    match key.split_once('.') {
        Some((outer, inner)) => snapshot.get(outer)?.get(inner),
        None => snapshot.get(key),
    }
}

fn set_value_at(snapshot: &mut Snapshot, key: &str, value: Value) {
    match key.split_once('.') {
        Some((outer, inner)) => {
            if let Some(Value::Object(object)) = snapshot.get_mut(outer) {
                object.insert(inner.to_owned(), value);
            }
        }
        None => {
            snapshot.insert(key.to_owned(), value);
        }
    }
}

/// `current` with `keys` put back to their values in `defaults`.
pub(crate) fn with_defaults(mut current: Snapshot, defaults: &Snapshot, keys: &[&str]) -> Snapshot {
    for key in keys {
        debug_assert!(
            value_at(defaults, key).is_some(),
            "not a saved setting: {key}"
        );
        if let Some(default) = value_at(defaults, key) {
            set_value_at(&mut current, key, default.clone());
        }
    }
    current
}

/// Whether any of `keys` holds something other than its default.
pub(crate) fn differs(current: &Snapshot, defaults: &Snapshot, keys: &[&str]) -> bool {
    keys.iter().any(|key| {
        debug_assert!(
            value_at(defaults, key).is_some(),
            "not a saved setting: {key}"
        );
        value_at(current, key) != value_at(defaults, key)
    })
}

/// The settings that differ, each control's own: a saved object such as the war outline holds
/// several controls, so it is compared field by field (`war_outline.width`).
fn changed_keys(from: &Snapshot, to: &Snapshot) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for key in from.keys().chain(to.keys()) {
        match (from.get(key), to.get(key)) {
            (Some(Value::Object(a)), Some(Value::Object(b))) => {
                for inner in a.keys().chain(b.keys()) {
                    if a.get(inner) != b.get(inner) {
                        keys.push(format!("{key}.{inner}"));
                    }
                }
            }
            (a, b) if a != b => keys.push(key.clone()),
            _ => {}
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

/// "Territory opacity", "Overlay and Minimap", or "Connections and 4 more".
fn describe(keys: &[String]) -> String {
    let mut labels: Vec<&str> = Vec::new();
    for key in keys {
        let label = LABELS
            .iter()
            .find(|(saved, _)| saved == key)
            .map_or("a setting", |(_, label)| label);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    match labels.as_slice() {
        [] => "settings".to_owned(),
        [one] => (*one).to_owned(),
        [one, two] => format!("{one} and {two}"),
        [first, rest @ ..] => format!("{first} and {} more", rest.len()),
    }
}

/// Steps kept for undo.
const DEPTH: usize = 100;

/// The settings as they have been, oldest first, and which of them are current. Undo and
/// Redo move along it; a change drops what lies ahead and becomes the newest.
pub(crate) struct History {
    states: Vec<Snapshot>,
    at: usize,
}

impl History {
    pub(crate) fn new(current: Snapshot) -> Self {
        Self {
            states: vec![current],
            at: 0,
        }
    }

    fn current(&self) -> &Snapshot {
        &self.states[self.at]
    }

    /// A change of settings, one step.
    pub(crate) fn change(&mut self, next: Snapshot) {
        if next == *self.current() {
            return;
        }
        self.states.truncate(self.at + 1);
        self.states.push(next);
        if self.states.len() > DEPTH + 1 {
            self.states.remove(0);
        }
        self.at = self.states.len() - 1;
    }

    /// Takes a correction no one chose to one setting, such as a saved season the server no
    /// longer has, as if it had always been so: wherever a state holds the corrected value it
    /// holds `to` instead, so neither Undo nor Redo brings the value back. Steps left with
    /// nothing to change go. Every other setting, including one being adjusted right now, is
    /// left as recorded.
    pub(crate) fn absorb(&mut self, key: &str, to: Value) {
        let Some(from) = value_at(self.current(), key).cloned() else {
            return;
        };
        if from == to {
            return;
        }
        for state in &mut self.states {
            if value_at(state, key) == Some(&from) {
                set_value_at(state, key, to.clone());
            }
        }
        let current = self.at;
        let mut kept: Vec<Snapshot> = Vec::with_capacity(self.states.len());
        for (index, state) in std::mem::take(&mut self.states).into_iter().enumerate() {
            if kept.last() != Some(&state) {
                kept.push(state);
            }
            if index == current {
                self.at = kept.len() - 1;
            }
        }
        self.states = kept;
    }

    /// The settings before the current ones, which become current.
    pub(crate) fn undo(&mut self) -> Option<Snapshot> {
        self.at = self.at.checked_sub(1)?;
        Some(self.current().clone())
    }

    /// The settings the last Undo left, which become current again.
    pub(crate) fn redo(&mut self) -> Option<Snapshot> {
        if self.at + 1 >= self.states.len() {
            return None;
        }
        self.at += 1;
        Some(self.current().clone())
    }

    /// What Undo would put back, if anything.
    pub(crate) fn undo_label(&self) -> Option<String> {
        let before = self.states.get(self.at.checked_sub(1)?)?;
        Some(describe(&changed_keys(before, self.current())))
    }

    pub(crate) fn redo_label(&self) -> Option<String> {
        let after = self.states.get(self.at + 1)?;
        Some(describe(&changed_keys(self.current(), after)))
    }
}

/// How long the note saying what Undo or Redo changed stays.
const NOTICE_FOR: std::time::Duration = std::time::Duration::from_secs(4);

/// The menu's settings in context: their history, their defaults, and the one way to change
/// all of them at once.
#[derive(Clone, Copy)]
pub(crate) struct SettingsStore {
    current: Memo<Snapshot>,
    defaults: StoredValue<Snapshot>,
    apply: Callback<Snapshot>,
    history: RwSignal<History>,
    /// Set while a change is waiting to be recorded.
    pending: StoredValue<bool>,
    /// Adjustments under way, such as a slider held by the pointer or a key; what they change
    /// is recorded as one step when the last ends. Undo and Redo wait for it.
    adjusting: RwSignal<u32>,
    /// What the last Undo or Redo changed, for a few seconds.
    notice: RwSignal<Option<String>>,
    notice_serial: StoredValue<u32>,
}

impl SettingsStore {
    /// `current` reads the settings, `apply` writes all of them.
    pub(crate) fn new(
        current: Memo<Snapshot>,
        defaults: Snapshot,
        apply: Callback<Snapshot>,
    ) -> Self {
        let store = Self {
            current,
            defaults: StoredValue::new(defaults),
            apply,
            history: RwSignal::new(History::new(current.get_untracked())),
            pending: StoredValue::new(false),
            adjusting: RwSignal::new(0),
            notice: RwSignal::new(None),
            notice_serial: StoredValue::new(0),
        };
        // A change is recorded once the effects it sets off have settled, so one click is one
        // step even where it moves several settings (an overlay turning another off).
        Effect::new(move || {
            store.current.track();
            if !store.pending.get_value() {
                store.pending.set_value(true);
                set_timeout(move || store.settle(), std::time::Duration::ZERO);
            }
        });
        store
    }

    fn settle(self) {
        if self.pending.try_get_value() != Some(true) || self.adjusting.get_untracked() > 0 {
            return;
        }
        self.pending.set_value(false);
        let next = self.current.get_untracked();
        self.history.update(|history| history.change(next));
    }

    /// A control starts an adjustment that may change its setting many times on the way.
    pub(crate) fn begin_adjustment(self) {
        self.adjusting.update(|count| *count += 1);
    }

    /// The adjustment is complete; all it changed becomes one step.
    pub(crate) fn end_adjustment(self) {
        if self
            .adjusting
            .try_update(|count| *count = count.saturating_sub(1))
            .is_none()
        {
            return;
        }
        self.pending.set_value(true);
        self.settle();
    }

    /// Writes `next` as a step of its own.
    fn apply_alone(self, next: Snapshot) {
        self.settle();
        self.apply.run(next);
        let applied = self.current.get_untracked();
        self.history.update(|history| history.change(applied));
    }

    /// Whether any of `keys` is off its default.
    pub(crate) fn differs(self, keys: &[&str]) -> bool {
        self.current.with(|current| {
            self.defaults
                .with_value(|defaults| differs(current, defaults, keys))
        })
    }

    /// Puts `keys` back to their defaults and leaves every other setting as it is.
    pub(crate) fn reset(self, keys: &[&str]) {
        let next = self
            .defaults
            .with_value(|defaults| with_defaults(self.current.get_untracked(), defaults, keys));
        self.apply_alone(next);
    }

    /// Every setting in the menu back to its default.
    pub(crate) fn reset_all(self) {
        self.apply_alone(self.defaults.get_value());
    }

    /// Takes `key`'s value as it is now as never chosen, without a step: a correction the app
    /// made on its own.
    pub(crate) fn absorb(self, key: &str) {
        let Some(to) = self
            .current
            .with_untracked(|now| value_at(now, key).cloned())
        else {
            return;
        };
        self.history.update(|history| history.absorb(key, to));
    }

    pub(crate) fn undo(self) {
        if self.adjusting.get_untracked() > 0 {
            return;
        }
        self.settle();
        let label = self.history.with_untracked(History::undo_label);
        if let Some(state) = self.history.try_update(History::undo).flatten() {
            self.apply.run(state);
            self.announce(format!("Undid {}", label.unwrap_or_default()));
        }
    }

    pub(crate) fn redo(self) {
        if self.adjusting.get_untracked() > 0 {
            return;
        }
        self.settle();
        let label = self.history.with_untracked(History::redo_label);
        if let Some(state) = self.history.try_update(History::redo).flatten() {
            self.apply.run(state);
            self.announce(format!("Redid {}", label.unwrap_or_default()));
        }
    }

    /// What Undo would put back; none while an adjustment is under way.
    pub(crate) fn undo_label(self) -> Option<String> {
        (self.adjusting.get() == 0)
            .then(|| self.history.with(History::undo_label))
            .flatten()
    }

    pub(crate) fn redo_label(self) -> Option<String> {
        (self.adjusting.get() == 0)
            .then(|| self.history.with(History::redo_label))
            .flatten()
    }

    pub(crate) fn notice(self) -> Option<String> {
        self.notice.get()
    }

    fn announce(self, text: String) {
        self.notice.set(Some(text));
        let serial = self.notice_serial.get_value().wrapping_add(1);
        self.notice_serial.set_value(serial);
        set_timeout(
            move || {
                if self.notice_serial.try_get_value() == Some(serial) {
                    self.notice.set(None);
                }
            },
            NOTICE_FOR,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{History, LABELS, NOT_SETTINGS, Snapshot, describe, differs, with_defaults};
    use serde_json::json;

    fn snap(value: serde_json::Value) -> Snapshot {
        super::snapshot(value)
    }

    #[test]
    fn a_snapshot_leaves_out_the_layout() {
        let saved =
            snap(json!({ "show_names": true, "sidebar_open": true, "sidebar_width": 400.0 }));
        assert_eq!(saved, snap(json!({ "show_names": true })));
        for key in NOT_SETTINGS {
            assert!(!LABELS.iter().any(|(saved, _)| saved == key), "{key}");
        }
    }

    #[test]
    fn undo_and_redo_walk_the_steps_and_a_new_change_drops_the_redo() {
        let a = snap(json!({ "show_names": false, "show_minimap": false }));
        let b = snap(json!({ "show_names": true, "show_minimap": false }));
        let c = snap(json!({ "show_names": true, "show_minimap": true }));
        let mut history = History::new(a.clone());
        assert_eq!(history.undo_label(), None);
        history.change(b.clone());
        history.change(c.clone());
        assert_eq!(history.undo_label().as_deref(), Some("Minimap"));
        assert_eq!(history.undo(), Some(b.clone()));
        assert_eq!(history.redo_label().as_deref(), Some("Minimap"));
        assert_eq!(history.undo(), Some(a.clone()));
        assert_eq!(history.undo(), None);
        assert_eq!(history.redo(), Some(b.clone()));
        history.change(a.clone());
        assert_eq!(history.redo_label(), None);
        assert_eq!(history.undo(), Some(b));
    }

    #[test]
    fn every_change_is_its_own_step_however_quick() {
        let at = |opacity: f64| snap(json!({ "territory_fill_opacity": opacity }));
        let mut history = History::new(at(0.26));
        history.change(at(0.5));
        history.change(at(0.7));
        assert_eq!(history.undo(), Some(at(0.5)));
        assert_eq!(history.undo(), Some(at(0.26)));
    }

    #[test]
    fn controls_saved_in_one_object_are_undone_one_at_a_time() {
        let outline = |width: f64, opacity: f64| {
            snap(json!({ "war_outline": { "style": "Solid", "width": width, "opacity": opacity } }))
        };
        let mut history = History::new(outline(1.0, 0.88));
        history.change(outline(1.5, 0.88));
        history.change(outline(1.5, 0.5));
        assert_eq!(history.undo_label().as_deref(), Some("Outline opacity"));
        assert_eq!(history.undo(), Some(outline(1.5, 0.88)));
        assert_eq!(history.undo_label().as_deref(), Some("Outline width"));
        assert_eq!(history.undo(), Some(outline(1.0, 0.88)));
    }

    #[test]
    fn a_correction_holds_in_every_state_undo_or_redo_can_reach() {
        let at = |season: serde_json::Value, names: bool| {
            snap(
                json!({ "heat_selected_season_id": season, "show_names": names, "heat_mode_enabled": false }),
            )
        };
        let mut history = History::new(at(json!(999), false));
        history.change(at(json!(999), true));
        history.change(at(json!(999), false));
        history.undo();
        history.absorb("heat_selected_season_id", json!(null));
        assert_eq!(history.redo(), Some(at(json!(null), false)));
        history.undo();
        assert_eq!(history.undo(), Some(at(json!(null), false)));
        assert_eq!(history.undo(), None);
    }

    #[test]
    fn a_correction_leaves_other_choices_and_drops_steps_it_empties() {
        let at = |season: serde_json::Value| snap(json!({ "heat_selected_season_id": season }));
        let mut history = History::new(at(json!(30)));
        history.change(at(json!(999)));
        history.absorb("heat_selected_season_id", json!(null));
        assert_eq!(history.undo_label().as_deref(), Some("Season"));
        assert_eq!(history.undo(), Some(at(json!(30))));

        let mut history = History::new(at(json!(999)));
        history.change(at(json!(null)));
        history.undo();
        history.absorb("heat_selected_season_id", json!(null));
        assert_eq!(history.undo_label(), None);
        assert_eq!(history.redo_label(), None);
    }

    #[test]
    fn a_correction_leaves_an_unrecorded_adjustment_to_be_undone() {
        let at = |season: serde_json::Value, opacity: f64| {
            snap(json!({ "heat_selected_season_id": season, "territory_fill_opacity": opacity }))
        };
        let mut history = History::new(at(json!(999), 0.26));
        // The season is corrected while opacity is mid-drag; the drag then ends at 0.8.
        history.absorb("heat_selected_season_id", json!(null));
        history.change(at(json!(null), 0.8));
        assert_eq!(history.undo_label().as_deref(), Some("Territory opacity"));
        assert_eq!(history.undo(), Some(at(json!(null), 0.26)));
    }

    #[test]
    fn history_keeps_its_depth() {
        let mut history = History::new(snap(json!({ "player_head_size": 0.0 })));
        for size in 1..=150 {
            history.change(snap(json!({ "player_head_size": f64::from(size) })));
        }
        let mut steps = 0;
        while history.undo().is_some() {
            steps += 1;
        }
        assert_eq!(steps, super::DEPTH);
    }

    #[test]
    fn resetting_one_setting_leaves_the_others_and_reaches_into_the_outline() {
        let defaults = snap(json!({
            "show_names": false,
            "show_minimap": false,
            "war_outline": { "style": "Solid", "width": 1.0 },
        }));
        let current = snap(json!({
            "show_names": true,
            "show_minimap": true,
            "war_outline": { "style": "Dashed", "width": 1.5 },
        }));
        assert!(differs(&current, &defaults, &["war_outline.width"]));
        let reset = with_defaults(current, &defaults, &["show_names", "war_outline.width"]);
        assert_eq!(
            reset,
            snap(json!({
                "show_names": false,
                "show_minimap": true,
                "war_outline": { "style": "Dashed", "width": 1.0 },
            }))
        );
        assert!(!differs(
            &reset,
            &defaults,
            &["show_names", "war_outline.width"]
        ));
        assert!(differs(&reset, &defaults, &["war_outline.style"]));
    }

    #[test]
    fn steps_are_named_as_the_menu_names_them() {
        let keys = |keys: &[&str]| keys.iter().map(|key| (*key).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            describe(&keys(&["defense_highlight", "resource_highlight"])),
            "Overlay"
        );
        assert_eq!(
            describe(&keys(&["show_minimap", "show_names"])),
            "Minimap and Territory Names"
        );
        assert_eq!(
            describe(&keys(&["show_connections", "show_minimap", "show_names"])),
            "Connections and 2 more"
        );
    }
}
