//! The viewer's settings and their one owner, [`ViewerSettings`].
//!
//! Every change goes through the owner, which says what kind it is: the viewer's edit (one Undo
//! step, named for the setting), an adjustment under way (a slider held, recorded as one step
//! when it ends), a reset, or a correction the app makes on its own (no step; Undo and Redo
//! never bring the corrected value back). The owner also saves the settings, with the panels'
//! layout beside them.

mod fields;
mod history;
mod model;
mod saved;

use leptos::prelude::*;

pub(crate) use fields::*;
pub(crate) use model::*;
pub(crate) use saved::Layout;

use history::History;

/// The step Reset all records.
const RESET_ALL: &str = "Reset all";

/// The settings, their defaults and their history, in context for every control and reader.
#[derive(Clone, Copy)]
pub(crate) struct ViewerSettings {
    /// What is shown, including an adjustment under way.
    current: RwSignal<Settings>,
    /// What has been recorded; its current state is `current` without an adjustment's changes.
    history: RwSignal<History>,
    /// Adjustments under way. Undo and Redo wait for them.
    adjusting: RwSignal<u32>,
    /// What the last Undo or Redo changed, numbered so a stale dismissal leaves a newer one.
    notice: RwSignal<Option<(u32, String)>>,
}

impl ViewerSettings {
    /// The saved settings, and the layout saved with them for the app's panels. Call
    /// [`Self::persist`] once the layout's signals exist.
    pub(crate) fn load() -> (Self, Layout) {
        let (settings, layout) = saved::load();
        (Self::new(settings), layout)
    }

    fn new(settings: Settings) -> Self {
        Self {
            current: RwSignal::new(settings),
            history: RwSignal::new(History::new(settings)),
            adjusting: RwSignal::new(0),
            notice: RwSignal::new(None),
        }
    }

    /// Saves the settings and `layout` whenever either changes.
    pub(crate) fn persist(self, layout: impl Fn() -> Layout + 'static) {
        Effect::new(move || {
            let layout = layout();
            self.current.with(|settings| saved::save(settings, &layout));
        });
    }

    /// Reads the settings, tracked.
    pub(crate) fn with<R>(self, read: impl FnOnce(&Settings) -> R) -> R {
        self.current.with(read)
    }

    /// One setting, as a signal that changes only when the setting does.
    pub(crate) fn value<T>(self, setting: Setting<T>) -> Memo<T>
    where
        T: Clone + PartialEq + Send + Sync + 'static,
    {
        Memo::new(move |_| self.current.with(|settings| setting.get(settings)))
    }

    pub(crate) fn get_untracked<T>(self, setting: Setting<T>) -> T {
        self.current
            .with_untracked(|settings| setting.get(settings))
    }

    /// The viewer sets `setting` to `value`: one Undo step.
    pub(crate) fn edit<T: Copy + 'static>(self, setting: Setting<T>, value: T) {
        self.change(setting.name, move |settings| setting.set(settings, value));
    }

    pub(crate) fn toggle(self, setting: Setting<bool>) {
        self.edit(setting, !self.get_untracked(setting));
    }

    /// A control starts an adjustment that may change its setting many times on the way.
    pub(crate) fn begin_adjustment(self) {
        self.adjusting.update(|count| *count += 1);
    }

    /// Shows `value` while the adjustment is under way, without recording it.
    pub(crate) fn preview<T>(self, setting: Setting<T>, value: T) {
        self.current.update(|settings| setting.set(settings, value));
    }

    /// The adjustment is complete: where it left `setting` becomes one step.
    pub(crate) fn end_adjustment<T: Copy + 'static>(self, setting: Setting<T>) {
        if self
            .adjusting
            .try_update(|count| *count = count.saturating_sub(1))
            .is_none()
        {
            return;
        }
        self.edit(setting, self.get_untracked(setting));
    }

    /// Whether every one of `settings` is at its default. Tracked.
    pub(crate) fn is_default(self, settings: &[&dyn Defaulted]) -> bool {
        let defaults = Settings::default();
        self.current.with(|current| {
            settings
                .iter()
                .all(|setting| setting.is_default(current, &defaults))
        })
    }

    /// Puts `settings` back to their defaults, one step named `step`, and leaves every other
    /// setting as it is.
    pub(crate) fn reset(self, step: &'static str, settings: &'static [&'static dyn Defaulted]) {
        let defaults = Settings::default();
        self.change(step, move |current| {
            for setting in settings {
                setting.restore(current, &defaults);
            }
        });
    }

    pub(crate) fn reset_one<T: PartialEq>(self, setting: Setting<T>) {
        let defaults = Settings::default();
        self.change(setting.name, move |current| {
            setting.restore(current, &defaults);
        });
    }

    /// Every setting back to its default; the layout stays.
    pub(crate) fn reset_all(self) {
        self.change(RESET_ALL, |current| *current = Settings::default());
    }

    /// A correction the app makes on its own, such as a saved season the server no longer has
    /// becoming Latest. It is made everywhere Undo and Redo can reach, as no one chose it.
    pub(crate) fn correct(self, fix: impl Fn(&mut Settings)) {
        if self
            .history
            .try_update_untracked(|history| history.correct(&fix))
            .unwrap_or(false)
        {
            self.history.notify();
        }
        self.update_current(fix);
    }

    /// Records the viewer's change as one step, and shows it over any adjustment under way.
    fn change(self, step: &'static str, apply: impl Fn(&mut Settings)) {
        if self
            .history
            .try_update_untracked(|history| {
                let mut next = *history.current();
                apply(&mut next);
                history.change(step, next)
            })
            .unwrap_or(false)
        {
            self.history.notify();
        }
        self.update_current(apply);
    }

    fn update_current(self, apply: impl Fn(&mut Settings)) {
        self.current.maybe_update(|current| {
            let before = *current;
            apply(current);
            *current != before
        });
    }

    pub(crate) fn undo(self) {
        self.step(History::undo_step, History::undo, "Undid");
    }

    pub(crate) fn redo(self) {
        self.step(History::redo_step, History::redo, "Redid");
    }

    fn step(
        self,
        name: fn(&History) -> Option<&'static str>,
        go: fn(&mut History) -> Option<&Settings>,
        verb: &str,
    ) {
        if self.adjusting.get_untracked() > 0 {
            return;
        }
        let Some(step) = self.history.with_untracked(name) else {
            return;
        };
        if let Some(state) = self
            .history
            .try_update(|history| go(history).copied())
            .flatten()
        {
            self.current.set(state);
            self.announce(format!("{verb} {step}"));
        }
    }

    /// What Undo would put back; none while an adjustment is under way.
    pub(crate) fn undo_step(self) -> Option<&'static str> {
        (self.adjusting.get() == 0)
            .then(|| self.history.with(History::undo_step))
            .flatten()
    }

    pub(crate) fn redo_step(self) -> Option<&'static str> {
        (self.adjusting.get() == 0)
            .then(|| self.history.with(History::redo_step))
            .flatten()
    }

    /// What the last Undo or Redo changed, and its number, until it is dismissed.
    pub(crate) fn notice(self) -> Option<(u32, String)> {
        self.notice.get()
    }

    /// Dismisses notice `serial`, unless a newer one has replaced it.
    pub(crate) fn dismiss_notice(self, serial: u32) {
        self.notice.maybe_update(|notice| {
            let current = notice.as_ref().is_some_and(|(shown, _)| *shown == serial);
            if current {
                *notice = None;
            }
            current
        });
    }

    fn announce(self, text: String) {
        self.notice.update(|notice| {
            let serial = notice
                .as_ref()
                .map_or(0, |(serial, _)| serial.wrapping_add(1));
            *notice = Some((serial, text));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh owner over `settings`, inside a reactive owner of its own.
    fn owner(settings: Settings) -> ViewerSettings {
        let root = Owner::new();
        root.set();
        std::mem::forget(root);
        ViewerSettings::new(settings)
    }

    fn now(settings: ViewerSettings) -> Settings {
        settings.current.get_untracked()
    }

    #[test]
    fn each_edit_is_one_step_named_for_its_setting() {
        let settings = owner(Settings::default());
        settings.edit(TERRITORY_NAMES, true);
        settings.edit(OVERLAY, Overlay::Defense);
        assert_eq!(settings.undo_step(), Some("Overlay"));
        settings.undo();
        assert_eq!(now(settings).overlay, Overlay::Off);
        assert!(now(settings).show_names);
        assert_eq!(
            settings.notice().map(|(_, text)| text).as_deref(),
            Some("Undid Overlay")
        );
        assert_eq!(settings.redo_step(), Some("Overlay"));
        settings.redo();
        assert_eq!(now(settings).overlay, Overlay::Defense);
    }

    #[test]
    fn an_adjustment_is_one_step_and_holds_undo_until_it_ends() {
        let settings = owner(Settings::default());
        settings.edit(TERRITORY_NAMES, true);
        settings.begin_adjustment();
        for opacity in [0.3, 0.5, 0.8] {
            settings.preview(TERRITORY_OPACITY, opacity);
        }
        assert_eq!(now(settings).territory_fill_opacity, 0.8);
        assert_eq!(settings.undo_step(), None);
        settings.undo();
        assert_eq!(now(settings).territory_fill_opacity, 0.8);
        settings.end_adjustment(TERRITORY_OPACITY);
        assert_eq!(settings.undo_step(), Some("Territory opacity"));
        settings.undo();
        assert_eq!(
            now(settings).territory_fill_opacity,
            DEFAULT_TERRITORY_OPACITY
        );
        assert!(now(settings).show_names);
    }

    #[test]
    fn an_edit_during_an_adjustment_keeps_both_apart() {
        let settings = owner(Settings::default());
        settings.begin_adjustment();
        settings.preview(TERRITORY_OPACITY, 0.6);
        settings.toggle(MINIMAP);
        assert_eq!(now(settings).territory_fill_opacity, 0.6);
        assert!(now(settings).show_minimap);
        settings.end_adjustment(TERRITORY_OPACITY);
        settings.undo();
        assert_eq!(
            now(settings).territory_fill_opacity,
            DEFAULT_TERRITORY_OPACITY
        );
        assert!(now(settings).show_minimap);
        settings.undo();
        assert!(!now(settings).show_minimap);
    }

    #[test]
    fn a_correction_is_no_step_and_undo_never_brings_it_back() {
        let settings = owner(Settings {
            heat_selected_season_id: Some(999),
            ..Settings::default()
        });
        settings.edit(TERRITORY_NAMES, true);
        settings.begin_adjustment();
        settings.preview(TERRITORY_OPACITY, 0.7);
        settings.correct(|s| s.heat_selected_season_id = None);
        assert_eq!(now(settings).territory_fill_opacity, 0.7);
        settings.end_adjustment(TERRITORY_OPACITY);
        assert_eq!(settings.undo_step(), Some("Territory opacity"));
        settings.undo();
        settings.undo();
        assert_eq!(settings.undo_step(), None);
        assert_eq!(now(settings), Settings::default());
    }

    #[test]
    fn a_pinned_season_resets_to_latest_and_latest_stays_latest() {
        let settings = owner(Settings::default());
        settings.edit(HEAT_SEASON, Some(31));
        settings.correct(|s| {
            if s.heat_selected_season_id == Some(999) {
                s.heat_selected_season_id = None;
            }
        });
        assert_eq!(now(settings).heat_selected_season_id, Some(31));
        settings.reset_one(HEAT_SEASON);
        assert_eq!(now(settings).heat_selected_season_id, None);
        assert_eq!(settings.undo_step(), Some("Season"));
    }

    #[test]
    fn a_reset_leaves_every_other_choice_and_reset_all_is_one_step() {
        let settings = owner(Settings::default());
        settings.edit(LABEL_SIZE, 1.5);
        settings.edit(ICON_SIZE, 1.2);
        settings.edit(TERRITORY_NAMES, true);
        assert!(!settings.is_default(LABEL_SIZES));
        settings.reset("Label sizes", LABEL_SIZES);
        assert!(settings.is_default(LABEL_SIZES));
        assert!(now(settings).show_names);
        assert_eq!(settings.undo_step(), Some("Label sizes"));
        settings.edit(CONNECTIONS, false);
        settings.reset_all();
        assert_eq!(now(settings), Settings::default());
        settings.undo();
        assert!(!now(settings).show_connections);
        assert!(now(settings).show_names);
    }

    #[test]
    fn a_shortcut_toggles_its_overlay_and_turns_off_the_others() {
        let settings = owner(Settings::default());
        let toggle =
            |overlay| settings.edit(OVERLAY, settings.get_untracked(OVERLAY).toggled(overlay));
        toggle(Overlay::Resources);
        toggle(Overlay::Intel);
        assert_eq!(now(settings).overlay, Overlay::Intel);
        toggle(Overlay::Intel);
        assert_eq!(now(settings).overlay, Overlay::Off);
    }
}
