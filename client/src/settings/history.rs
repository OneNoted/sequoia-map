//! The settings as they have been, for Undo and Redo.

use super::model::Settings;

/// Steps kept for undo.
pub(super) const DEPTH: usize = 100;

/// The settings as they have been, oldest first, and which of them are current. Undo and Redo
/// move along it; a change drops what lies ahead and becomes the newest. Each step between two
/// states is named for what the viewer changed, as the menu names it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct History {
    states: Vec<Settings>,
    /// `steps[i]` names the change from `states[i]` to `states[i + 1]`.
    steps: Vec<&'static str>,
    at: usize,
}

impl History {
    pub(super) fn new(current: Settings) -> Self {
        Self {
            states: vec![current],
            steps: Vec::new(),
            at: 0,
        }
    }

    pub(super) fn current(&self) -> &Settings {
        &self.states[self.at]
    }

    /// The viewer's change, one step named `step`. A change that changes nothing is none, and
    /// says so.
    pub(super) fn change(&mut self, step: &'static str, next: Settings) -> bool {
        if next == *self.current() {
            return false;
        }
        self.states.truncate(self.at + 1);
        self.steps.truncate(self.at);
        self.states.push(next);
        self.steps.push(step);
        if self.states.len() > DEPTH + 1 {
            self.states.remove(0);
            self.steps.remove(0);
        }
        self.at = self.states.len() - 1;
        true
    }

    /// A correction no one chose, such as a saved season the server no longer has, made as if
    /// it had always been so: every state Undo or Redo can reach takes it, so neither brings the
    /// corrected value back. A step it leaves changing nothing goes. Says whether it changed
    /// anything.
    pub(super) fn correct(&mut self, fix: &impl Fn(&mut Settings)) -> bool {
        let mut changed = false;
        for state in &mut self.states {
            let before = *state;
            fix(state);
            changed |= *state != before;
        }
        let mut index = 1;
        while index < self.states.len() {
            if self.states[index] == self.states[index - 1] {
                self.states.remove(index);
                self.steps.remove(index - 1);
                if self.at >= index {
                    self.at -= 1;
                }
            } else {
                index += 1;
            }
        }
        changed
    }

    /// The settings before the current ones, which become current.
    pub(super) fn undo(&mut self) -> Option<&Settings> {
        self.at = self.at.checked_sub(1)?;
        Some(self.current())
    }

    /// The settings the last Undo left, which become current again.
    pub(super) fn redo(&mut self) -> Option<&Settings> {
        if self.at + 1 >= self.states.len() {
            return None;
        }
        self.at += 1;
        Some(self.current())
    }

    /// What Undo would put back, if anything.
    pub(super) fn undo_step(&self) -> Option<&'static str> {
        self.steps.get(self.at.checked_sub(1)?).copied()
    }

    pub(super) fn redo_step(&self) -> Option<&'static str> {
        self.steps.get(self.at).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::{DEPTH, History};
    use crate::settings::model::Settings;

    fn names(on: bool) -> Settings {
        Settings {
            show_names: on,
            ..Settings::default()
        }
    }

    fn opacity(value: f64) -> Settings {
        Settings {
            territory_fill_opacity: value,
            ..Settings::default()
        }
    }

    #[test]
    fn undo_and_redo_walk_the_steps_and_a_new_change_drops_the_redo() {
        let a = Settings::default();
        let b = names(true);
        let c = Settings {
            show_minimap: true,
            ..b
        };
        let mut history = History::new(a);
        assert_eq!(history.undo_step(), None);
        history.change("Territory Names", b);
        history.change("Minimap", c);
        assert_eq!(history.undo_step(), Some("Minimap"));
        assert_eq!(history.undo(), Some(&b));
        assert_eq!(history.redo_step(), Some("Minimap"));
        assert_eq!(history.undo(), Some(&a));
        assert_eq!(history.undo(), None);
        assert_eq!(history.redo(), Some(&b));
        history.change("Territory Names", a);
        assert_eq!(history.redo_step(), None);
        assert_eq!(history.undo(), Some(&b));
    }

    #[test]
    fn a_change_that_changes_nothing_is_no_step() {
        let mut history = History::new(Settings::default());
        history.change("Territory Names", Settings::default());
        assert_eq!(history.undo_step(), None);
    }

    #[test]
    fn every_change_is_its_own_step_however_quick() {
        let mut history = History::new(opacity(0.26));
        history.change("Territory opacity", opacity(0.5));
        history.change("Territory opacity", opacity(0.7));
        assert_eq!(history.undo(), Some(&opacity(0.5)));
        assert_eq!(history.undo(), Some(&opacity(0.26)));
    }

    #[test]
    fn a_correction_holds_in_every_state_undo_or_redo_can_reach() {
        let at = |season: Option<i32>, names: bool| Settings {
            heat_selected_season_id: season,
            show_names: names,
            ..Settings::default()
        };
        let mut history = History::new(at(Some(999), false));
        history.change("Territory Names", at(Some(999), true));
        history.change("Territory Names", at(Some(999), false));
        history.undo();
        history.correct(&|s: &mut Settings| s.heat_selected_season_id = None);
        assert_eq!(history.current(), &at(None, true));
        assert_eq!(history.redo(), Some(&at(None, false)));
        history.undo();
        assert_eq!(history.undo(), Some(&at(None, false)));
        assert_eq!(history.undo(), None);
    }

    #[test]
    fn a_correction_leaves_other_choices_and_drops_steps_it_empties() {
        let at = |season: Option<i32>| Settings {
            heat_selected_season_id: season,
            ..Settings::default()
        };
        let forget_999 = |s: &mut Settings| {
            if s.heat_selected_season_id == Some(999) {
                s.heat_selected_season_id = None;
            }
        };
        let mut history = History::new(at(Some(30)));
        history.change("Season", at(Some(999)));
        history.correct(&forget_999);
        assert_eq!(history.undo_step(), Some("Season"));
        assert_eq!(history.undo(), Some(&at(Some(30))));

        let mut history = History::new(at(Some(999)));
        history.change("Season", at(None));
        history.undo();
        history.correct(&forget_999);
        assert_eq!(history.undo_step(), None);
        assert_eq!(history.redo_step(), None);
        assert_eq!(history.current(), &at(None));
    }

    #[test]
    fn history_keeps_its_depth() {
        let mut history = History::new(Settings {
            player_head_size: 0.0,
            ..Settings::default()
        });
        for size in 1..=150 {
            history.change(
                "Head size",
                Settings {
                    player_head_size: f64::from(size),
                    ..Settings::default()
                },
            );
        }
        let mut steps = 0;
        while history.undo().is_some() {
            steps += 1;
        }
        assert_eq!(steps, DEPTH);
        assert_eq!(history.current().player_head_size, 50.0);
    }
}
