//! The settings menu in the sidebar and the phone sheet.
//!
//! Every control shows one setting of [`ViewerSettings`] and changes it through the owner, so
//! each change is one Undo step named for it and each row's reset puts back only its own
//! setting. Everyday settings come first, grouped by what they are for. Fine-tuning (sizes,
//! line style) and diagnostics sit behind two native `<details>` disclosures.

use leptos::prelude::*;
use wasm_bindgen::JsCast;

use sequoia_shared::history::HistoryHeatMeta;

use crate::app::{
    CurrentMode, HeatFallbackApplied, HeatMetaState, HeatWindowLabel, IsMobile, MapMode,
    ShowSettings, WarFeedVisible,
};
use crate::settings::{self, *};
use crate::territory::ClientTerritoryMap;
use crate::ui::{ViewHead, disclosure_toggled};

fn percent(value: f64) -> String {
    format!("{:.0}%", value * 100.0)
}

/// A multiplier of the style's own default, such as the classic opacity.
fn times(value: f64) -> String {
    format!("\u{d7}{value:.2}")
}

fn pixels(value: f64) -> String {
    format!("{value:.0}px")
}

/// Stable element id fragment for a label, for `aria-describedby` and friends.
fn slug(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// The settings view: everyday groups first, then the two collapsed sections.
#[component]
pub(crate) fn SettingsPanel() -> impl IntoView {
    let settings = expect_context::<ViewerSettings>();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let ShowSettings(show_settings) = expect_context();
    let WarFeedVisible(war_feed_visible) = expect_context();
    let IsMobile(is_mobile) = expect_context();
    let territory_count = Memo::new(move |_| territories.with(|map| map.len()));
    let overlay = settings.value(settings::OVERLAY);
    let heat_map = settings.value(settings::HEAT_MAP);
    let countdown = settings.value(settings::COOLDOWN_COUNTDOWN);
    let teammate_heads = settings.value(settings::TEAMMATE_HEADS);
    let connection_style = settings.value(settings::CONNECTION_STYLE);

    // With the countdown on, "off" still writes a rounded time held (3h), so say that.
    let time_held_off_label = Signal::derive(move || if countdown.get() { "3h" } else { "Off" });
    let war_heads_visible = move || war_feed_visible.get() && teammate_heads.get();

    view! {
        <div class="settings-panel">
            <ViewHead
                back=("Back from settings", Callback::new(move |()| show_settings.set(false)))
                title=|| view! { <h2 class="sb-view-title">"Settings"</h2> }
            >
                <SettingsNotice />
                <SettingsUndoRedo />
            </ViewHead>

            <SettingsSection title="Map">
                <SettingsSlider
                    label="Territory opacity"
                    setting=settings::TERRITORY_OPACITY
                    min=TERRITORY_OPACITY_MIN
                    max=TERRITORY_OPACITY_MAX
                    step=0.01
                    format=percent
                />
                <SettingsChoice
                    label="Overlay"
                    hint="Shortcuts P, D and I"
                    name="settings-overlay"
                    setting=settings::OVERLAY
                    options=vec![
                        (Overlay::Off, Signal::stored("Off")),
                        (Overlay::Resources, Signal::stored("Resources")),
                        (Overlay::Defense, Signal::stored("Defense")),
                        (Overlay::Intel, Signal::stored("Intel")),
                    ]
                />
                <Show when=move || overlay.get() == Overlay::Resources>
                    <SettingsSlider
                        label="Resource opacity"
                        setting=settings::RESOURCE_OPACITY
                        min=RESOURCE_HIGHLIGHT_OPACITY_MIN
                        max=RESOURCE_HIGHLIGHT_OPACITY_MAX
                        step=0.05
                        format=percent
                    />
                </Show>
                <SettingsSwitch label="Heat Map" setting=settings::HEAT_MAP hint="Color territories by how often they change hands" />
                <Show when=move || heat_map.get()>
                    <HeatOptions />
                </Show>
                <SettingsSwitch label="Connections" shortcut="C" setting=settings::CONNECTIONS />
                <SettingsSwitch label="Resource Icons" setting=settings::RESOURCE_ICONS />
                <SettingsSwitch label="Corner Ornaments" setting=settings::CORNER_ORNAMENTS hint="Decorative corners in the guild color" />
                // The minimap is never drawn on a phone.
                <Show when=move || !is_mobile.get()>
                    <SettingsSwitch label="Minimap" shortcut="M" setting=settings::MINIMAP />
                </Show>
            </SettingsSection>

            <SettingsSection title="Labels">
                <SettingsSwitch label="Territory Names" shortcut="N" setting=settings::TERRITORY_NAMES />
                <SettingsSwitch label="Abbreviate Names" shortcut="A" setting=settings::ABBREVIATE_NAMES hint="Cascading Basins becomes CB" />
                <SettingsChoice
                    label="When zoomed out"
                    name="settings-zoomed-out"
                    setting=settings::ZOOMED_OUT
                    options=vec![
                        (ZoomedOutLabels::GuildNames, Signal::stored("Guild names")),
                        (ZoomedOutLabels::TerritoryTags, Signal::stored("Tags")),
                        (ZoomedOutLabels::Off, Signal::stored("Off")),
                    ]
                />
                <SettingsSwitch label="Readable Font" shortcut="F" setting=settings::READABLE_FONT hint="Plain text instead of the pixel font" />
                <SettingsColorRow label="Territory name color" setting=settings::NAME_COLOR />
                <SettingsColorRow label="Guild tag color" setting=settings::TAG_COLOR />
                <SettingsChoice
                    label="HQ crown"
                    hint="Fill hides the owner when zoomed out"
                    name="settings-hq-crown"
                    setting=settings::HQ_CROWN
                    options=vec![
                        (HqCrownStyle::NameBelow, Signal::stored("Name below")),
                        (HqCrownStyle::Compact, Signal::stored("Compact")),
                        (HqCrownStyle::Fill, Signal::stored("Fill")),
                    ]
                />
                <SettingsSlider
                    label="Crown size"
                    setting=settings::CROWN_SIZE
                    min=HQ_CROWN_SCALE_MIN
                    max=HQ_CROWN_SCALE_MAX
                    step=0.1
                    format=percent
                />
            </SettingsSection>

            <SettingsSection title="Timers">
                <SettingsChoice
                    label="Time held"
                    name="settings-time-held"
                    setting=settings::TIME_HELD
                    options=vec![
                        (TimeHeld::Off, time_held_off_label),
                        (TimeHeld::Compact, Signal::stored("3h05m")),
                        (TimeHeld::Exact, Signal::stored("03:05:12")),
                    ]
                />
                <SettingsSwitch label="Cooldown Countdown" shortcut="T" setting=settings::COOLDOWN_COUNTDOWN hint="Time left on a new capture's 10-minute cooldown" />
                <SettingsSwitch label="Thick Cooldown Borders" setting=settings::THICK_COOLDOWN_BORDERS />
                <SettingsChoice
                    label="Cooldown"
                    hint="Fill runs red to green over the cooldown. Heat and highlights keep the bar"
                    name="settings-cooldown-display"
                    setting=settings::COOLDOWN_DISPLAY
                    options=vec![
                        (CooldownDisplay::Bar, Signal::stored("Bar")),
                        (CooldownDisplay::Fill, Signal::stored("Fill")),
                    ]
                />
            </SettingsSection>

            // Only for viewers who can actually see the war feed - the same rule the
            // panels themselves follow. For everyone else these would be dead toggles.
            <Show when=move || war_feed_visible.get()>
                <SettingsSection title="War">
                    <SettingsSwitch label="War Panel" setting=settings::WAR_PANEL hint="Queued territories, wars and members" />
                    <SettingsSwitch label="War Markers" setting=settings::WAR_MARKERS hint="Countdowns and war clocks on territories" />
                    <SettingsSwitch
                        label="Unconfirmed Activity"
                        setting=settings::UNCONFIRMED_ACTIVITY
                        hint="Recent activity also lists cancelled queues and attacks without a confirmed capture"
                    />
                    <SettingsChoice
                        label="War outline"
                        name="settings-war-outline-style"
                        setting=settings::WAR_OUTLINE_STYLE
                        options=vec![
                            (WarOutlineStyle::Dashed, Signal::stored("Dashed")),
                            (WarOutlineStyle::Solid, Signal::stored("Solid")),
                        ]
                    />
                    <SettingsChoice
                        label="Outline color"
                        hint="Urgency warms over the last three minutes"
                        name="settings-war-outline-color"
                        setting=settings::WAR_OUTLINE_COLOR
                        options=vec![
                            (WarOutlineColor::Stage, Signal::stored("Stage")),
                            (WarOutlineColor::Urgency, Signal::stored("Urgency")),
                        ]
                    />
                    <SettingsSwitch label="Queue Progress" setting=settings::WAR_OUTLINE_PROGRESS hint="The outline shows the queue time left" />
                    <SettingsSlider
                        label="Outline width"
                        setting=settings::WAR_OUTLINE_WIDTH
                        min=WAR_OUTLINE_WIDTH_MIN
                        max=WAR_OUTLINE_WIDTH_MAX
                        step=0.25
                        format=times
                    />
                    <SettingsSlider
                        label="Outline opacity"
                        setting=settings::WAR_OUTLINE_OPACITY
                        min=WAR_OUTLINE_OPACITY_MIN
                        max=WAR_OUTLINE_OPACITY_MAX
                        step=0.05
                        format=percent
                    />
                    <SettingsSwitch label="Teammate Heads" setting=settings::TEAMMATE_HEADS />
                    <Show when=move || teammate_heads.get()>
                        <SettingsSwitch label="Show Faces" setting=settings::TEAMMATE_FACES hint="A dot when off" nested=true />
                        <SettingsSwitch label="Show Names" hint="Hidden when zoomed far out" setting=settings::TEAMMATE_NAMES nested=true />
                    </Show>
                </SettingsSection>
            </Show>

            <SettingsSection title="Leaderboard">
                <SettingsSwitch label="Territories" setting=settings::LEADERBOARD_TERRITORIES />
                <SettingsSwitch label="Online Members" setting=settings::LEADERBOARD_ONLINE />
                <SettingsSwitch label="SR Rate" setting=settings::LEADERBOARD_SR_RATE hint="Estimated SR per hour; per 5 minutes in history" />
                <SettingsSwitch label="Season Rating" setting=settings::LEADERBOARD_SEASON_RATING hint="Always shown when sorted by rating" />
                <SettingsSwitch label="Estimate SR Scalar" setting=settings::ESTIMATE_SR_SCALAR hint="From season data when available" />
                <SettingsScalarRow />
            </SettingsSection>

            <SettingsDisclosure title="Sizes and lines" hint="Label, icon and head sizes; connection style">
                <div class="settings-subhead">"Labels and icons"</div>
                <SettingsSlider
                    label="Overall"
                    setting=settings::LABEL_SIZE
                    min=LABEL_SCALE_MASTER_MIN
                    max=LABEL_SCALE_MASTER_MAX
                    step=0.05
                    format=percent
                />
                <SettingsSlider
                    label="Guild tags"
                    setting=settings::GUILD_TAG_SIZE
                    min=LABEL_SCALE_GROUP_MIN
                    max=LABEL_SCALE_GROUP_MAX
                    step=0.05
                    format=percent
                />
                <SettingsSlider
                    label="Territory names"
                    setting=settings::TERRITORY_NAME_SIZE
                    min=LABEL_SCALE_GROUP_MIN
                    max=LABEL_SCALE_GROUP_MAX
                    step=0.05
                    format=percent
                />
                <SettingsSlider
                    label="Timers"
                    setting=settings::TIMER_SIZE
                    min=LABEL_SCALE_GROUP_MIN
                    max=LABEL_SCALE_GROUP_MAX
                    step=0.05
                    format=percent
                />
                <SettingsSlider
                    label="Icons"
                    setting=settings::ICON_SIZE
                    min=LABEL_SCALE_GROUP_MIN
                    max=LABEL_SCALE_GROUP_MAX
                    step=0.05
                    format=percent
                />
                <SettingsResetButton label="Reset label sizes" step="Label sizes" settings=settings::LABEL_SIZES />

                <div class="settings-subhead">"Connections"</div>
                <SettingsChoice
                    label="Style"
                    hint="Classic is the original soft look"
                    name="settings-connection-style"
                    setting=settings::CONNECTION_STYLE
                    options=vec![
                        (ConnectionStyle::Classic, Signal::stored("Classic")),
                        (ConnectionStyle::White, Signal::stored("Solid white")),
                        (ConnectionStyle::Guild, Signal::stored("Solid guild")),
                    ]
                />
                <SettingsSwitch
                    label="Bold Connections"
                    setting=settings::BOLD_CONNECTIONS
                    shortcut="B"
                    hint="Wider lines; classic lines also take the guild color"
                />
                <Show
                    when=move || connection_style.get() == ConnectionStyle::Classic
                    fallback=move || {
                        view! {
                            <SettingsSlider
                                label="Opacity"
                                setting=settings::SOLID_CONNECTION_OPACITY
                                min=CONNECTION_SOLID_OPACITY_MIN
                                max=CONNECTION_SOLID_OPACITY_MAX
                                step=0.05
                                format=percent
                            />
                        }
                    }
                >
                    <SettingsSlider
                        label="Opacity"
                        setting=settings::CLASSIC_CONNECTION_OPACITY
                        min=CONNECTION_OPACITY_SCALE_MIN
                        max=CONNECTION_OPACITY_SCALE_MAX
                        step=0.05
                        format=times
                    />
                </Show>
                <SettingsSlider
                    label="Thickness"
                    setting=settings::CONNECTION_THICKNESS
                    min=CONNECTION_THICKNESS_SCALE_MIN
                    max=CONNECTION_THICKNESS_SCALE_MAX
                    step=0.05
                    format=times
                />
                <SettingsResetButton label="Reset connection style" step="Connection style" settings=settings::CONNECTION_LINES />

                <Show when=war_heads_visible>
                    <div class="settings-subhead">"Teammate heads"</div>
                    <SettingsSlider
                        label="Head size"
                        setting=settings::HEAD_SIZE
                        min=PLAYER_HEAD_SIZE_MIN
                        max=PLAYER_HEAD_SIZE_MAX
                        step=1.0
                        format=pixels
                    />
                </Show>
            </SettingsDisclosure>

            <SettingsDisclosure title="Advanced" hint="Diagnostics">
                <SettingsSwitch label="Diagnostics" setting=settings::DIAGNOSTICS hint="API status and territory data sources" />
                <div class="settings-row settings-info">
                    <span class="settings-row-label">"Territories loaded"</span>
                    <span class="settings-value">{move || territory_count.get()}</span>
                </div>
            </SettingsDisclosure>

            <SettingsResetAll />
        </div>
    }
}

/// A titled group of rows.
#[component]
fn SettingsSection(title: &'static str, children: Children) -> impl IntoView {
    let heading_id = format!("settings-section-{}", slug(title));
    let labelled_by = heading_id.clone();
    view! {
        <section class="settings-section" aria-labelledby=labelled_by>
            <h3 class="settings-section-title" id=heading_id>{title}</h3>
            {children()}
        </section>
    }
}

/// A collapsed group for fine-tuning. Native `<details>`, so Enter, Space and taps work
/// without extra code; it starts closed every time the menu opens, and opening it brings what it
/// reveals into view as the sidebar's other disclosures do.
#[component]
fn SettingsDisclosure(
    title: &'static str,
    hint: &'static str,
    children: Children,
) -> impl IntoView {
    view! {
        <details class="settings-disclosure" on:toggle=move |event| { disclosure_toggled(&event); }>
            <summary>
                <span class="settings-row-text">
                    <span class="settings-disclosure-title">{title}</span>
                    <span class="settings-row-hint">{hint}</span>
                </span>
            </summary>
            <div class="settings-disclosure-body">{children()}</div>
        </details>
    }
}

/// An on/off setting: a full-width `role="switch"` button.
#[component]
fn SettingsSwitch(
    label: &'static str,
    setting: Setting<bool>,
    /// The global keyboard shortcut for this setting, shown as a key cap.
    #[prop(optional)]
    shortcut: Option<&'static str>,
    /// One short line under the label, for settings whose name needs it.
    #[prop(optional)]
    hint: Option<&'static str>,
    /// Indents the row under the setting it depends on.
    #[prop(optional)]
    nested: bool,
) -> impl IntoView {
    let settings = expect_context::<ViewerSettings>();
    let active = settings.value(setting);
    let hint_id = hint.map(|_| format!("settings-hint-{}", slug(label)));
    view! {
        <div class="settings-line" class:settings-row-nested=nested>
            <button
                type="button"
                role="switch"
                class="settings-row settings-switch"
                aria-label=label
                aria-checked=move || active.get().to_string()
                aria-describedby=hint_id.clone()
                on:click=move |_| settings.toggle(setting)
            >
                <span class="settings-row-text">
                    <span class="settings-row-label">
                        {label}
                        {shortcut.map(|key| view! { <kbd class="settings-kbd" aria-hidden="true">{key}</kbd> })}
                    </span>
                    {hint.map(|hint| view! { <span class="settings-row-hint" id=hint_id.clone()>{hint}</span> })}
                </span>
                <span class="settings-switch-track" aria-hidden="true"></span>
            </button>
            <SettingsDefault label setting />
        </div>
    }
}

/// Puts one row's setting back to its default, leaving every other setting. Shown only while
/// it differs, in a column of its own at the row's end.
#[component]
fn SettingsDefault<T>(label: &'static str, setting: Setting<T>) -> impl IntoView
where
    T: PartialEq + Send + Sync + 'static,
{
    let settings = expect_context::<ViewerSettings>();
    let title = format!("Reset {label} to default");
    view! {
        <button
            type="button"
            class="settings-default"
            title=title.clone()
            aria-label=title
            disabled=move || settings.is_default(&[&setting])
            on:click=move |_| settings.reset_one(setting)
        >
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" />
                <path d="M3 3v5h5" />
            </svg>
        </button>
    }
}

/// How long the note saying what Undo or Redo changed stays.
const NOTICE_FOR: std::time::Duration = std::time::Duration::from_secs(4);

/// What the last Undo or Redo changed, for a few seconds.
#[component]
fn SettingsNotice() -> impl IntoView {
    let settings = expect_context::<ViewerSettings>();
    Effect::new(move || {
        if let Some((serial, _)) = settings.notice() {
            set_timeout(move || settings.dismiss_notice(serial), NOTICE_FOR);
        }
    });
    view! {
        <span class="settings-notice" role="status">
            {move || settings.notice().map(|(_, text)| text)}
        </span>
    }
}

/// Undo and Redo for the settings, named for what they would change.
#[component]
fn SettingsUndoRedo() -> impl IntoView {
    let settings = expect_context::<ViewerSettings>();
    let title = |verb: &str, step: Option<&str>| match step {
        Some(step) => format!("{verb} {step}"),
        None => format!("Nothing to {}", verb.to_lowercase()),
    };
    view! {
        <button
            type="button"
            class="ui-button ui-icon-button settings-history-button"
            title=move || title("Undo", settings.undo_step())
            aria-label=move || title("Undo", settings.undo_step())
            disabled=move || settings.undo_step().is_none()
            on:click=move |_| settings.undo()
        >
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <path d="M9 14 4 9l5-5" />
                <path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11" />
            </svg>
        </button>
        <button
            type="button"
            class="ui-button ui-icon-button settings-history-button"
            title=move || title("Redo", settings.redo_step())
            aria-label=move || title("Redo", settings.redo_step())
            disabled=move || settings.redo_step().is_none()
            on:click=move |_| settings.redo()
        >
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <path d="m15 14 5-5-5-5" />
                <path d="M20 9H9.5a5.5 5.5 0 0 0 0 11H13" />
            </svg>
        </button>
    }
}

/// One choice out of a few, as native radio buttons styled into a segmented control, so
/// arrow keys move between options.
#[component]
fn SettingsChoice<T>(
    label: &'static str,
    /// The radio group name; unique within the menu.
    name: &'static str,
    options: Vec<(T, Signal<&'static str>)>,
    setting: Setting<T>,
    #[prop(optional)] hint: Option<&'static str>,
) -> impl IntoView
where
    T: Copy + PartialEq + Send + Sync + 'static,
{
    let settings = expect_context::<ViewerSettings>();
    let selected = settings.value(setting);
    let label_id = format!("{name}-label");
    let labelled_by = label_id.clone();
    view! {
        <div class="settings-row settings-choice" role="radiogroup" aria-labelledby=labelled_by>
            <div class="settings-choice-head">
                <span class="settings-row-text">
                    <span class="settings-row-label" id=label_id>{label}</span>
                    {hint.map(|hint| view! { <span class="settings-row-hint">{hint}</span> })}
                </span>
                <SettingsDefault label setting />
            </div>
            <div class="settings-segments">
                {options
                    .into_iter()
                    .map(|(value, text)| {
                        view! {
                            <label class="settings-segment">
                                <input
                                    type="radio"
                                    name=name
                                    prop:checked=move || selected.get() == value
                                    on:change=move |_| settings.edit(setting, value)
                                />
                                <span>{move || text.get()}</span>
                            </label>
                        }
                    })
                    .collect_view()}
            </div>
        </div>
    }
}

/// The keys a range input moves by.
fn is_range_key(key: &str) -> bool {
    matches!(
        key,
        "ArrowLeft"
            | "ArrowRight"
            | "ArrowUp"
            | "ArrowDown"
            | "PageUp"
            | "PageDown"
            | "Home"
            | "End"
    )
}

/// A labelled range input. Dragging shows each value live and records where it ends as one
/// step; the shown value uses `format`.
#[component]
fn SettingsSlider(
    label: &'static str,
    setting: Setting<f64>,
    min: f64,
    max: f64,
    step: f64,
    format: fn(f64) -> String,
) -> impl IntoView {
    let settings = expect_context::<ViewerSettings>();
    let value = settings.value(setting);
    let slider_ref = NodeRef::<leptos::html::Input>::new();
    let input_id = format!("settings-slider-{}", slug(label));
    let label_for = input_id.clone();
    // How far along the track the value is, for the filled part of the track.
    let filled = move || {
        let share = ((value.get() - min) / (max - min)).clamp(0.0, 1.0);
        format!("{share:.4}")
    };
    let dragging: RwSignal<bool> = RwSignal::new(false);

    // Outside changes (reset, Undo, other tabs of the menu) move the thumb, but not mid-drag.
    Effect::new(move || {
        let external = value.get();
        if !dragging.get()
            && let Some(input) = slider_ref.get()
        {
            input.set_value(&format!("{external:.2}"));
        }
    });

    let read = |e: &leptos::ev::Event| {
        e.target()
            .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
            .and_then(|input| input.value().trim().parse::<f64>().ok())
    };
    // One adjustment, one Undo step: a drag from press to release, or the range keys from
    // the first press to the last release however long they are held, whatever pauses it has
    // on the way. Other keys (Shift, say) neither start nor end it.
    let adjusting = StoredValue::new(false);
    let keys_held = StoredValue::new(Vec::<String>::new());
    let begin = move || {
        if !adjusting.get_value() {
            adjusting.set_value(true);
            settings.begin_adjustment();
        }
    };
    let finish = move || {
        if adjusting.try_get_value() == Some(true) {
            adjusting.set_value(false);
            settings.end_adjustment(setting);
        }
        dragging.try_set(false);
    };
    on_cleanup(finish);
    // A drag ends when the pointer that pressed the range is released, wherever that is. A range
    // dragged back to the value it last committed fires no change event, so that cannot end it.
    // The browser handles the release after this event and may still change the value: it sets
    // it at the release point (input, then change) or, for a cancelled touch, puts the start
    // value back (input alone). So the drag ends once it has.
    let pressed_by = StoredValue::new(None::<i32>);
    let release = move |e: leptos::ev::PointerEvent| {
        if pressed_by.try_get_value().flatten() != Some(e.pointer_id()) {
            return;
        }
        pressed_by.set_value(None);
        set_timeout(
            move || {
                if pressed_by.try_get_value() == Some(None)
                    && keys_held.try_with_value(Vec::is_empty) == Some(true)
                {
                    finish();
                }
            },
            std::time::Duration::ZERO,
        );
    };
    let released = window_event_listener(leptos::ev::pointerup, release);
    let cancelled = window_event_listener(leptos::ev::pointercancel, release);
    on_cleanup(move || {
        released.remove();
        cancelled.remove();
    });
    let on_input = move |e: leptos::ev::Event| {
        if let Some(parsed) = read(&e) {
            begin();
            dragging.set(true);
            settings.preview(setting, parsed);
        }
    };
    let on_change = move |e: leptos::ev::Event| {
        if let Some(parsed) = read(&e)
            && adjusting.get_value()
        {
            settings.preview(setting, parsed);
        }
        dragging.set(false);
        if keys_held.with_value(Vec::is_empty) {
            finish();
        }
    };
    let on_keydown = move |e: leptos::ev::KeyboardEvent| {
        let key = e.key();
        if is_range_key(&key) {
            keys_held.update_value(|held| {
                if !held.contains(&key) {
                    held.push(key);
                }
            });
            begin();
        }
    };
    let on_keyup = move |e: leptos::ev::KeyboardEvent| {
        let key = e.key();
        let released = keys_held.try_update_value(|held| {
            let before = held.len();
            held.retain(|down| *down != key);
            before != held.len() && held.is_empty()
        });
        if released == Some(true) {
            finish();
        }
    };
    let on_blur = move |_: leptos::ev::FocusEvent| {
        keys_held.update_value(Vec::clear);
        finish();
    };

    view! {
        <div class="settings-slider">
            <label class="settings-slider-label" for=label_for>{label}</label>
            <input
                node_ref=slider_ref
                id=input_id
                type="range"
                class="timeline-slider"
                min=min
                max=max
                step=step
                value=format!("{:.2}", value.get_untracked())
                style:--tl-filled=filled
                aria-valuetext=move || format(value.get())
                on:pointerdown=move |e| pressed_by.set_value(Some(e.pointer_id()))
                on:input=on_input
                on:change=on_change
                on:keydown=on_keydown
                on:keyup=on_keyup
                on:blur=on_blur
            />
            <span class="settings-value" aria-hidden="true">{move || format(value.get())}</span>
            <SettingsDefault label setting />
        </div>
    }
}

const NAME_COLOR_OPTIONS: &[(NameColor, &str, &str)] = &[
    (NameColor::White, "White", "#dcdad2"),
    (NameColor::Guild, "Guild color", "#a88cc8"), // representative purple for the swatch
    (NameColor::Gold, "Gold", "#f5c542"),
    (NameColor::Copper, "Copper", "#b56727"),
    (NameColor::Muted, "Muted", "#787470"),
];

/// A row of color swatch buttons for one label color.
#[component]
fn SettingsColorRow(label: &'static str, setting: Setting<NameColor>) -> impl IntoView {
    let settings = expect_context::<ViewerSettings>();
    let color = settings.value(setting);
    let label_id = format!("settings-color-{}", slug(label));
    let labelled_by = label_id.clone();
    view! {
        <div class="settings-row settings-colors" role="group" aria-labelledby=labelled_by>
            <span class="settings-row-label" id=label_id>{label}</span>
            <div class="settings-swatches">
                {NAME_COLOR_OPTIONS
                    .iter()
                    .map(|&(variant, name, css_color)| {
                        view! {
                            <button
                                type="button"
                                class="settings-swatch"
                                title=name
                                aria-label=name
                                aria-pressed=move || (color.get() == variant).to_string()
                                style=format!("--swatch: {css_color};")
                                on:click=move |_| settings.edit(setting, variant)
                            />
                        }
                    })
                    .collect_view()}
            </div>
            <SettingsDefault label setting />
        </div>
    }
}

/// Reads a finished scalar edit. Only a positive number is an edit; empty, zero, negative
/// or unparsable text means "no change". (`clamp_manual_scalar` maps those to the default,
/// which suits a corrupt saved value, not a half-typed one.)
fn parse_scalar_edit(text: &str) -> Option<f64> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0)
}

/// The manual season-rating scalar, as a number field. Typing only changes the field;
/// Enter, Tab/blur or a spin-button step commits it (clamped into range), and Escape
/// abandons the edit.
#[component]
fn SettingsScalarRow() -> impl IntoView {
    let settings = expect_context::<ViewerSettings>();
    let scalar = settings.value(settings::MANUAL_SR_SCALAR);
    let input_ref = NodeRef::<leptos::html::Input>::new();
    // Set while the field holds unfinished text, so outside updates do not overwrite it.
    let editing = RwSignal::new(false);
    let show = move |input: &web_sys::HtmlInputElement| {
        input.set_value(&format!("{:.2}", scalar.get_untracked()));
    };

    // Outside changes (Reset all, Undo) reach the field whenever no edit is in progress.
    Effect::new(move || {
        scalar.track();
        if !editing.get()
            && let Some(input) = input_ref.get()
        {
            show(&input);
        }
    });

    // Only text the user typed is committed: focusing and leaving a field that shows a
    // rounded value must not rewrite the stored one.
    let commit = move || {
        if !editing.get_untracked() {
            return;
        }
        let Some(input) = input_ref.get_untracked() else {
            return;
        };
        if let Some(value) = parse_scalar_edit(&input.value()) {
            settings.edit(settings::MANUAL_SR_SCALAR, value);
        }
        editing.set(false);
        // Also normalizes text that changed nothing ("1.5", rejected input).
        show(&input);
    };
    let on_keydown = move |e: leptos::ev::KeyboardEvent| match e.key().as_str() {
        "Enter" => commit(),
        "Escape" => {
            // The global handler blurs the field next; with the edit dropped, nothing commits.
            editing.set(false);
            if let Some(input) = input_ref.get_untracked() {
                show(&input);
            }
        }
        _ => {}
    };

    view! {
        <div class="settings-row settings-number">
            <label class="settings-row-text" for="settings-manual-sr-scalar">
                <span class="settings-row-label">"Manual SR Scalar"</span>
                <span class="settings-row-hint">"Used when estimating is off or unavailable"</span>
            </label>
            <input
                node_ref=input_ref
                id="settings-manual-sr-scalar"
                type="number"
                class="seq-input"
                min="0.05"
                max="20"
                step="0.05"
                value=format!("{:.2}", scalar.get_untracked())
                on:input=move |_| editing.set(true)
                on:change=move |_| commit()
                on:blur=move |_| commit()
                on:keydown=on_keydown
            />
            <SettingsDefault label="Manual SR Scalar" setting=settings::MANUAL_SR_SCALAR />
        </div>
    }
}

/// How long "Reset all" waits for its confirming second press.
const RESET_CONFIRM_WINDOW: std::time::Duration = std::time::Duration::from_secs(4);

/// Puts every setting back to its default, on a second press within a few seconds. Undo
/// brings them all back.
#[component]
fn SettingsResetAll() -> impl IntoView {
    let settings = expect_context::<ViewerSettings>();
    let armed = RwSignal::new(false);
    // Only the latest arming may disarm; an older timer is stale.
    let arming = StoredValue::new(0u32);
    let on_click = move |_| {
        if armed.get_untracked() {
            armed.set(false);
            settings.reset_all();
            return;
        }
        armed.set(true);
        let this = arming.get_value().wrapping_add(1);
        arming.set_value(this);
        set_timeout(
            move || {
                if arming.try_get_value() == Some(this) {
                    armed.set(false);
                }
            },
            RESET_CONFIRM_WINDOW,
        );
    };
    view! {
        <div class="settings-reset-all-row">
            <span class="settings-row-text">
                <span class="settings-row-label">"Reset all settings"</span>
                <span class="settings-row-hint" role="status">
                    {move || if armed.get() { "Press again to confirm" } else { "Every setting in this menu" }}
                </span>
            </span>
            <button
                type="button"
                class="ui-button settings-reset-all"
                data-armed=move || armed.get().to_string()
                on:click=on_click
            >
                {move || if armed.get() { "Confirm reset" } else { "Reset all" }}
            </button>
        </div>
    }
}

/// A small right-aligned button that resets one group of settings as one step; the label names
/// the group. It is unavailable while the whole group is at its defaults.
#[component]
fn SettingsResetButton(
    label: &'static str,
    /// What Undo calls the reset.
    step: &'static str,
    settings: &'static [&'static dyn Defaulted],
) -> impl IntoView {
    let owner = expect_context::<ViewerSettings>();
    view! {
        <div class="settings-reset-row">
            <button
                type="button"
                class="ui-button settings-reset"
                disabled=move || owner.is_default(settings)
                on:click=move |_| owner.reset(step, settings)
            >
                {label}
            </button>
        </div>
    }
}

/// Heat map source and season, shown while the heat map is on.
#[component]
fn HeatOptions() -> impl IntoView {
    let settings = expect_context::<ViewerSettings>();
    let CurrentMode(mode) = expect_context();
    let HeatMetaState(meta) = expect_context();
    let HeatFallbackApplied(fallback_applied) = expect_context();
    let HeatWindowLabel(window_label) = expect_context();
    // Live mode picks a source, history a cumulative basis; both are season or all-time, and
    // each mode keeps its own.
    let period = Signal::derive(move || {
        if mode.get() == MapMode::History {
            settings::HEAT_HISTORY_SEASON
        } else {
            settings::HEAT_LIVE_SEASON
        }
    });
    let season_wide = Memo::new(move |_| {
        let period = period.get();
        settings.with(|s| period.get(s))
    });

    view! {
        <div class="settings-row-nested settings-heat">
            {move || {
                view! {
                    <SettingsChoice
                        label="Heat period"
                        name="settings-heat-period"
                        setting=period.get()
                        options=vec![
                            (true, Signal::stored("Season")),
                            (false, Signal::stored("All-time")),
                        ]
                    />
                }
            }}
            <Show when=move || season_wide.get()>
                <SettingsHeatSeasonRow meta=meta />
            </Show>
            <Show when=move || fallback_applied.get()>
                <div class="settings-note settings-note-warn">
                    "Season data unavailable, using last 60d fallback."
                </div>
            </Show>
            <div class="settings-note">{move || window_label.get()}</div>
        </div>
    }
}

#[component]
fn SettingsHeatSeasonRow(meta: RwSignal<Option<HistoryHeatMeta>>) -> impl IntoView {
    let settings = expect_context::<ViewerSettings>();
    let season_id = settings.value(settings::HEAT_SEASON);
    let on_change = move |e: leptos::ev::Event| {
        let Some(select) = e
            .target()
            .and_then(|target| target.dyn_into::<web_sys::HtmlSelectElement>().ok())
        else {
            return;
        };
        let value = select.value();
        let season = if value == "latest" {
            None
        } else {
            value.parse::<i32>().ok()
        };
        settings.edit(settings::HEAT_SEASON, season);
    };

    view! {
        <div class="settings-row settings-number">
            <label class="settings-row-label" for="settings-heat-season">"Season"</label>
            <select id="settings-heat-season" class="seq-input" on:change=on_change>
                <option value="latest" prop:selected=move || season_id.get().is_none()>
                    "Latest"
                </option>
                {move || {
                    meta.get()
                        .map(|m| {
                            m.seasons
                                .iter()
                                .map(|season| {
                                    let season_id_value = season.season_id;
                                    view! {
                                        <option
                                            value=season_id_value.to_string()
                                            prop:selected=move || season_id.get() == Some(season_id_value)
                                        >
                                            {format!("Season {season_id_value}")}
                                        </option>
                                    }
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                }}
            </select>
            <SettingsDefault label="Season" setting=settings::HEAT_SEASON />
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::parse_scalar_edit;

    #[test]
    fn scalar_edits_accept_only_positive_numbers() {
        assert_eq!(parse_scalar_edit("0.5"), Some(0.5));
        assert_eq!(parse_scalar_edit(" 1.25 "), Some(1.25));
        assert_eq!(parse_scalar_edit("25"), Some(25.0)); // clamped on commit, not here
        for rejected in ["", " ", "0", "-2", "abc", "NaN", "inf", "1e999"] {
            assert_eq!(parse_scalar_edit(rejected), None, "{rejected:?}");
        }
    }
}
