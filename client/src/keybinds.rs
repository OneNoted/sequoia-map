//! Map keyboard shortcuts. The global keydown handler resolves keys through
//! [`Shortcut::from_key`] and the help dialog lists [`Shortcut::ALL`], so the two
//! cannot drift apart.

use leptos::prelude::*;
use wasm_bindgen::JsCast;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Shortcut {
    Dismiss,
    FocusSearch,
    ListNext,
    ListPrevious,
    OpenListItem,
    PanLeft,
    PanRight,
    ZoomIn,
    ZoomOut,
    FitMap,
    ShowNames,
    AbbreviateNames,
    ReadableFont,
    Countdown,
    Connections,
    BoldConnections,
    ResourceHighlight,
    DefenseHighlight,
    MapIntel,
    Minimap,
    History,
    PlayPause,
    StepBack,
    StepForward,
}

impl Shortcut {
    /// Every shortcut the handler answers to, in help-dialog order.
    pub(crate) const ALL: [Shortcut; 24] = [
        Shortcut::Dismiss,
        Shortcut::FocusSearch,
        Shortcut::ListNext,
        Shortcut::ListPrevious,
        Shortcut::OpenListItem,
        Shortcut::PanLeft,
        Shortcut::PanRight,
        Shortcut::ZoomIn,
        Shortcut::ZoomOut,
        Shortcut::FitMap,
        Shortcut::ShowNames,
        Shortcut::AbbreviateNames,
        Shortcut::ReadableFont,
        Shortcut::Countdown,
        Shortcut::Connections,
        Shortcut::BoldConnections,
        Shortcut::ResourceHighlight,
        Shortcut::DefenseHighlight,
        Shortcut::MapIntel,
        Shortcut::Minimap,
        Shortcut::History,
        Shortcut::PlayPause,
        Shortcut::StepBack,
        Shortcut::StepForward,
    ];

    /// The `KeyboardEvent.key` values that trigger this shortcut.
    pub(crate) const fn keys(self) -> &'static [&'static str] {
        match self {
            Shortcut::Dismiss => &["Escape"],
            Shortcut::FocusSearch => &["/"],
            Shortcut::ListNext => &["j", "ArrowDown"],
            Shortcut::ListPrevious => &["k", "ArrowUp"],
            Shortcut::OpenListItem => &["Enter"],
            Shortcut::PanLeft => &["ArrowLeft"],
            Shortcut::PanRight => &["ArrowRight"],
            Shortcut::ZoomIn => &["+", "="],
            Shortcut::ZoomOut => &["-"],
            Shortcut::FitMap => &["r", "0"],
            Shortcut::ShowNames => &["n"],
            Shortcut::AbbreviateNames => &["a"],
            Shortcut::ReadableFont => &["f"],
            Shortcut::Countdown => &["t"],
            Shortcut::Connections => &["c"],
            Shortcut::BoldConnections => &["b"],
            Shortcut::ResourceHighlight => &["p"],
            Shortcut::DefenseHighlight => &["d"],
            Shortcut::MapIntel => &["i"],
            Shortcut::Minimap => &["m"],
            Shortcut::History => &["h"],
            Shortcut::PlayPause => &[" "],
            Shortcut::StepBack => &["["],
            Shortcut::StepForward => &["]"],
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Shortcut::Dismiss => "Close details, clear selection",
            Shortcut::FocusSearch => "Search",
            Shortcut::ListNext => "Next list item",
            Shortcut::ListPrevious => "Previous list item",
            Shortcut::OpenListItem => "Open highlighted item",
            Shortcut::PanLeft => "Pan left",
            Shortcut::PanRight => "Pan right",
            Shortcut::ZoomIn => "Zoom in",
            Shortcut::ZoomOut => "Zoom out",
            Shortcut::FitMap => "Fit all territories",
            Shortcut::ShowNames => "Territory names",
            Shortcut::AbbreviateNames => "Abbreviate names",
            Shortcut::ReadableFont => "Readable font",
            Shortcut::Countdown => "Countdown timers",
            Shortcut::Connections => "Connection lines",
            Shortcut::BoldConnections => "Bold connections",
            Shortcut::ResourceHighlight => "Resource highlight",
            Shortcut::DefenseHighlight => "Defense highlight",
            Shortcut::MapIntel => "Map intel",
            Shortcut::Minimap => "Minimap",
            Shortcut::History => "History mode",
            Shortcut::PlayPause => "Play or pause",
            Shortcut::StepBack => "Step back",
            Shortcut::StepForward => "Step forward",
        }
    }

    /// When the shortcut only does something in some states.
    pub(crate) const fn context(self) -> Option<&'static str> {
        match self {
            Shortcut::History => Some("when available"),
            Shortcut::PlayPause | Shortcut::StepBack | Shortcut::StepForward => Some("in history"),
            _ => None,
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|shortcut| shortcut.keys().contains(&key))
    }
}

fn key_cap(key: &str) -> String {
    match key {
        "Escape" => "Esc".to_string(),
        " " => "Space".to_string(),
        "ArrowUp" => "\u{2191}".to_string(),
        "ArrowDown" => "\u{2193}".to_string(),
        "ArrowLeft" => "\u{2190}".to_string(),
        "ArrowRight" => "\u{2192}".to_string(),
        letter if letter.chars().count() == 1 => letter.to_uppercase(),
        other => other.to_string(),
    }
}

/// While set, the global shortcuts stand down so the help dialog keeps the keyboard.
#[derive(Clone, Copy)]
pub(crate) struct KeybindHelpOpen(pub RwSignal<bool>);

/// The `?` button and the shortcut list it opens.
#[component]
pub(crate) fn KeybindHelp(#[prop(into)] touch_target: Signal<bool>) -> impl IntoView {
    let KeybindHelpOpen(open) = expect_context();
    let dialog_ref = NodeRef::<leptos::html::Dialog>::new();
    let button_ref = NodeRef::<leptos::html::Button>::new();

    Effect::new(move || {
        let Some(dialog) = dialog_ref.get() else {
            return;
        };
        if open.get() {
            if !dialog.open() {
                let _ = dialog.show_modal();
            }
        } else if dialog.open() {
            dialog.close();
        }
    });

    let on_close = move |_| {
        open.set(false);
        if let Some(button) = button_ref.get_untracked() {
            let _ = button.focus();
        }
    };
    // A click whose target is the dialog itself landed on the backdrop.
    let on_backdrop_click = move |e: leptos::ev::MouseEvent| {
        if let Some(dialog) = dialog_ref.get_untracked()
            && e.target()
                .and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok())
                .is_some_and(|target| target == *dialog)
        {
            dialog.close();
        }
    };

    let rows = Shortcut::ALL
        .into_iter()
        .map(|shortcut| {
            view! {
                <li class="keybind-help-row">
                    <span class="keybind-help-keys">
                        {shortcut
                            .keys()
                            .iter()
                            .map(|key| view! { <kbd>{key_cap(key)}</kbd> })
                            .collect_view()}
                    </span>
                    <span>
                        {shortcut.description()}
                        {shortcut
                            .context()
                            .map(|context| view! { <span class="keybind-help-context">{format!(" ({context})")}</span> })}
                    </span>
                </li>
            }
        })
        .collect_view();

    view! {
        <button
            node_ref=button_ref
            type="button"
            class="keybind-help-button"
            title="Keyboard shortcuts"
            aria-label="Keyboard shortcuts"
            aria-haspopup="dialog"
            aria-expanded=move || open.get().to_string()
            style:min-width=move || if touch_target.get() { "44px" } else { "26px" }
            style:min-height=move || if touch_target.get() { "44px" } else { "26px" }
            on:click=move |_| open.update(|value| *value = !*value)
        >
            "?"
        </button>
        <dialog
            node_ref=dialog_ref
            class="keybind-help"
            aria-labelledby="keybind-help-title"
            on:close=on_close
            on:click=on_backdrop_click
        >
            <div class="keybind-help-body">
                <div class="keybind-help-header">
                    <h2 id="keybind-help-title">"Keyboard shortcuts"</h2>
                    <button type="button" class="keybind-help-close" aria-label="Close" on:click=move |_| open.set(false)>
                        "\u{00D7}"
                    </button>
                </div>
                <ul>{rows}</ul>
                <p class="keybind-help-note">"Ignored while typing. Esc leaves a text field."</p>
            </div>
        </dialog>
    }
}

#[cfg(test)]
mod tests {
    use super::{Shortcut, key_cap};
    use std::collections::HashSet;

    #[test]
    fn every_listed_key_resolves_to_its_own_shortcut() {
        let mut seen = HashSet::new();
        for shortcut in Shortcut::ALL {
            assert!(!shortcut.keys().is_empty(), "{shortcut:?} has no key");
            for key in shortcut.keys() {
                assert!(seen.insert(*key), "{key:?} is bound twice");
                assert_eq!(Shortcut::from_key(key), Some(shortcut));
            }
        }
        assert_eq!(
            Shortcut::ALL.into_iter().collect::<HashSet<_>>().len(),
            Shortcut::ALL.len()
        );
    }

    #[test]
    fn unbound_and_shifted_keys_do_nothing() {
        for key in ["?", "A", "N", "Tab", "Shift", "x"] {
            assert_eq!(Shortcut::from_key(key), None, "{key:?}");
        }
    }

    #[test]
    fn key_caps_read_like_the_keyboard() {
        assert_eq!(key_cap("Escape"), "Esc");
        assert_eq!(key_cap(" "), "Space");
        assert_eq!(key_cap("ArrowDown"), "\u{2193}");
        assert_eq!(key_cap("p"), "P");
        assert_eq!(key_cap("Enter"), "Enter");
        assert_eq!(key_cap("["), "[");
    }
}
