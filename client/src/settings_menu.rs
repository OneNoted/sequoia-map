//! The settings menu in the sidebar and the phone sheet.
//!
//! Every control writes the same signals (and so the same persisted keys) the app has
//! always used; this module only decides how they are grouped and presented. Everyday
//! settings come first, grouped by what they are for. Fine-tuning (sizes, line style) and
//! diagnostics sit behind two native `<details>` disclosures.

use leptos::prelude::*;
use wasm_bindgen::JsCast;

use sequoia_shared::history::HistoryHeatMeta;

use crate::app::{
    AbbreviateNames, AutoSrScalarEnabled, BoldConnections, CONNECTION_OPACITY_SCALE_MAX,
    CONNECTION_OPACITY_SCALE_MIN, CONNECTION_THICKNESS_SCALE_MAX, CONNECTION_THICKNESS_SCALE_MIN,
    ConnectionOpacityScale, ConnectionThicknessScale, CurrentMode,
    DEFAULT_CONNECTION_OPACITY_SCALE, DEFAULT_CONNECTION_THICKNESS_SCALE,
    DEFAULT_LABEL_SCALE_GROUP, DEFAULT_LABEL_SCALE_MASTER, DEFAULT_LABEL_SCALE_STATIC_NAME,
    DEFAULT_LABEL_SCALE_STATIC_TAG, DefenseHighlight, HeatFallbackApplied, HeatHistoryBasis,
    HeatHistoryBasisSetting, HeatLiveSource, HeatLiveSourceSetting, HeatMetaState, HeatModeEnabled,
    HeatSelectedSeasonId, HeatWindowLabel, IsMobile, LABEL_SCALE_GROUP_MAX, LABEL_SCALE_GROUP_MIN,
    LABEL_SCALE_MASTER_MAX, LABEL_SCALE_MASTER_MIN, LabelScaleDynamic, LabelScaleIcons,
    LabelScaleMaster, LabelScaleStatic, LabelScaleStaticName, ManualSrScalar, MapIntelModeEnabled,
    MapMode, NameColor, NameColorSetting, PLAYER_HEAD_SIZE_MAX, PLAYER_HEAD_SIZE_MIN,
    PlayerHeadRenderHead, PlayerHeadRenderLabel, PlayerHeadSize, RESOURCE_HIGHLIGHT_OPACITY_MAX,
    RESOURCE_HIGHLIGHT_OPACITY_MIN, ReadableFont, ResourceHighlight, ResourceHighlightOpacity,
    ShowClaimLabels, ShowCompoundMapTime, ShowCountdown, ShowDebugInfo, ShowFarZoomTerritoryTags,
    ShowGranularMapTime, ShowLeaderboardOnline, ShowLeaderboardSrGain, ShowLeaderboardSrValue,
    ShowLeaderboardTerritoryCount, ShowMinimap, ShowNames, ShowPlayerHeads, ShowResourceIcons,
    ShowSettings, ShowTerritoryOrnaments, ShowWarQueue, ShowWarStats, TagColorSetting,
    ThickCooldownBorders, WarFeedVisible, clamp_connection_opacity_scale,
    clamp_connection_thickness_scale, clamp_label_scale_group, clamp_label_scale_master,
    clamp_player_head_size, clamp_resource_highlight_opacity,
};
use crate::season_scalar::clamp_manual_scalar;
use crate::territory::ClientTerritoryMap;

/// The one territory overlay that can be on at a time. Resource, defense and intel are
/// already mutually exclusive in the app; this is that rule shown as a single choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Overlay {
    Off,
    Resources,
    Defense,
    Intel,
}

impl Overlay {
    fn from_flags(resources: bool, defense: bool, intel: bool) -> Self {
        if resources {
            Overlay::Resources
        } else if defense {
            Overlay::Defense
        } else if intel {
            Overlay::Intel
        } else {
            Overlay::Off
        }
    }

    /// `(resources, defense, intel)` for this choice.
    fn flags(self) -> (bool, bool, bool) {
        (
            self == Overlay::Resources,
            self == Overlay::Defense,
            self == Overlay::Intel,
        )
    }
}

/// What labels the zoomed-out map shows. Guild names (one per connected claim) replace the
/// per-territory tags, so the stored pair `(claim labels, far-zoom tags)` is one choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ZoomedOutLabels {
    GuildNames,
    TerritoryTags,
    Off,
}

impl ZoomedOutLabels {
    fn from_flags(claim_labels: bool, far_zoom_tags: bool) -> Self {
        if claim_labels {
            ZoomedOutLabels::GuildNames
        } else if far_zoom_tags {
            ZoomedOutLabels::TerritoryTags
        } else {
            ZoomedOutLabels::Off
        }
    }

    /// New `(claim labels, far-zoom tags)`. Choosing guild names keeps the tag setting,
    /// which is unused until guild names are turned off again.
    fn flags(self, far_zoom_tags: bool) -> (bool, bool) {
        match self {
            ZoomedOutLabels::GuildNames => (true, far_zoom_tags),
            ZoomedOutLabels::TerritoryTags => (false, true),
            ZoomedOutLabels::Off => (false, false),
        }
    }
}

/// How the time a guild has held a territory is written. Exact overrides the compact
/// format, so the stored pair `(exact, compact)` is one choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimeHeld {
    Off,
    Compact,
    Exact,
}

impl TimeHeld {
    fn from_flags(exact: bool, compact: bool) -> Self {
        if exact {
            TimeHeld::Exact
        } else if compact {
            TimeHeld::Compact
        } else {
            TimeHeld::Off
        }
    }

    /// New `(exact, compact)`. Exact keeps the compact setting it overrides.
    fn flags(self, compact: bool) -> (bool, bool) {
        match self {
            TimeHeld::Off => (false, false),
            TimeHeld::Compact => (false, true),
            TimeHeld::Exact => (true, compact),
        }
    }
}

fn percent(value: f64) -> String {
    format!("{:.0}%", value * 100.0)
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
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let ShowSettings(show_settings) = expect_context();
    let AbbreviateNames(abbreviate_names) = expect_context();
    let show_connections: RwSignal<bool> = expect_context();
    let ShowCountdown(show_countdown) = expect_context();
    let ShowGranularMapTime(show_granular_map_time) = expect_context();
    let ShowCompoundMapTime(show_compound_map_time) = expect_context();
    let ShowNames(show_names) = expect_context();
    let ShowClaimLabels(show_claim_labels) = expect_context();
    let ShowFarZoomTerritoryTags(show_far_zoom_territory_tags) = expect_context();
    let ThickCooldownBorders(thick_cooldown_borders) = expect_context();
    let BoldConnections(bold_connections) = expect_context();
    let ConnectionOpacityScale(connection_opacity_scale) = expect_context();
    let ConnectionThicknessScale(connection_thickness_scale) = expect_context();
    let ResourceHighlight(resource_highlight) = expect_context();
    let ResourceHighlightOpacity(resource_highlight_opacity) = expect_context();
    let DefenseHighlight(defense_highlight) = expect_context();
    let MapIntelModeEnabled(map_intel_enabled) = expect_context();
    let ShowResourceIcons(show_resource_icons) = expect_context();
    let ShowTerritoryOrnaments(show_territory_ornaments) = expect_context();
    let ManualSrScalar(manual_sr_scalar) = expect_context();
    let AutoSrScalarEnabled(auto_sr_scalar_enabled) = expect_context();
    let ShowLeaderboardSrGain(show_leaderboard_sr_gain) = expect_context();
    let ShowLeaderboardSrValue(show_leaderboard_sr_value) = expect_context();
    let ShowLeaderboardTerritoryCount(show_leaderboard_territory_count) = expect_context();
    let ShowLeaderboardOnline(show_leaderboard_online) = expect_context();
    let HeatModeEnabled(heat_mode_enabled) = expect_context();
    let ReadableFont(readable_font) = expect_context();
    let NameColorSetting(name_color) = expect_context();
    let TagColorSetting(tag_color) = expect_context();
    let ShowMinimap(show_minimap) = expect_context();
    let ShowWarQueue(show_war_queue) = expect_context();
    let ShowWarStats(show_war_stats) = expect_context();
    let ShowPlayerHeads(show_player_heads) = expect_context();
    let PlayerHeadRenderHead(player_head_render_head) = expect_context();
    let PlayerHeadRenderLabel(player_head_render_label) = expect_context();
    let PlayerHeadSize(player_head_size) = expect_context();
    let WarFeedVisible(war_feed_visible) = expect_context();
    let IsMobile(is_mobile) = expect_context();
    let LabelScaleMaster(label_scale_master) = expect_context();
    let LabelScaleStatic(label_scale_static_tag) = expect_context();
    let LabelScaleStaticName(label_scale_static_name) = expect_context();
    let LabelScaleDynamic(label_scale_dynamic) = expect_context();
    let LabelScaleIcons(label_scale_icons) = expect_context();
    let ShowDebugInfo(show_debug_info) = expect_context();
    let territory_count = Memo::new(move |_| territories.with(|map| map.len()));

    let overlay = Signal::derive(move || {
        Overlay::from_flags(
            resource_highlight.get(),
            defense_highlight.get(),
            map_intel_enabled.get(),
        )
    });
    let set_overlay = Callback::new(move |choice: Overlay| {
        let (resources, defense, intel) = choice.flags();
        resource_highlight.set(resources);
        defense_highlight.set(defense);
        map_intel_enabled.set(intel);
    });

    let zoomed_out = Signal::derive(move || {
        ZoomedOutLabels::from_flags(show_claim_labels.get(), show_far_zoom_territory_tags.get())
    });
    let set_zoomed_out = Callback::new(move |choice: ZoomedOutLabels| {
        let (claims, tags) = choice.flags(show_far_zoom_territory_tags.get_untracked());
        show_claim_labels.set(claims);
        show_far_zoom_territory_tags.set(tags);
    });

    let time_held = Signal::derive(move || {
        TimeHeld::from_flags(show_granular_map_time.get(), show_compound_map_time.get())
    });
    let set_time_held = Callback::new(move |choice: TimeHeld| {
        let (exact, compact) = choice.flags(show_compound_map_time.get_untracked());
        show_granular_map_time.set(exact);
        show_compound_map_time.set(compact);
    });
    // With the countdown on, "off" still writes a rounded time held (3h), so say that.
    let time_held_off_label =
        Signal::derive(move || if show_countdown.get() { "3h" } else { "Off" });

    let war_heads_visible = move || war_feed_visible.get() && show_player_heads.get();

    view! {
        <div class="settings-panel">
            <div class="settings-header">
                <button
                    type="button"
                    class="settings-back"
                    title="Back"
                    aria-label="Close settings"
                    on:click=move |_| show_settings.set(false)
                >
                    "\u{2039}"
                </button>
                <h2 class="settings-title">
                    <span aria-hidden="true">{"\u{2699}"}</span>
                    "Settings"
                </h2>
            </div>

            <SettingsSection title="Map">
                <SettingsChoice
                    label="Overlay"
                    hint="Shortcuts P, D and I"
                    name="settings-overlay"
                    options=vec![
                        (Overlay::Off, Signal::stored("Off")),
                        (Overlay::Resources, Signal::stored("Resources")),
                        (Overlay::Defense, Signal::stored("Defense")),
                        (Overlay::Intel, Signal::stored("Intel")),
                    ]
                    selected=overlay
                    on_select=set_overlay
                />
                <Show when=move || resource_highlight.get()>
                    <SettingsSlider
                        label="Resource opacity"
                        value=resource_highlight_opacity
                        min=RESOURCE_HIGHLIGHT_OPACITY_MIN
                        max=RESOURCE_HIGHLIGHT_OPACITY_MAX
                        step=0.05
                        clamp=clamp_resource_highlight_opacity
                        format=percent
                    />
                </Show>
                <SettingsSwitch label="Heat Map" hint="Color territories by how often they change hands" active=heat_mode_enabled />
                <Show when=move || heat_mode_enabled.get()>
                    <HeatOptions />
                </Show>
                <SettingsSwitch label="Connections" shortcut="C" active=show_connections />
                <SettingsSwitch label="Resource Icons" active=show_resource_icons />
                <SettingsSwitch label="Corner Ornaments" hint="Decorative corners in the guild color" active=show_territory_ornaments />
                // The minimap is never drawn on a phone.
                <Show when=move || !is_mobile.get()>
                    <SettingsSwitch label="Minimap" shortcut="M" active=show_minimap />
                </Show>
            </SettingsSection>

            <SettingsSection title="Labels">
                <SettingsSwitch label="Territory Names" shortcut="N" active=show_names />
                <SettingsSwitch label="Abbreviate Names" shortcut="A" hint="Cascading Basins becomes CB" active=abbreviate_names />
                <SettingsChoice
                    label="When zoomed out"
                    name="settings-zoomed-out"
                    options=vec![
                        (ZoomedOutLabels::GuildNames, Signal::stored("Guild names")),
                        (ZoomedOutLabels::TerritoryTags, Signal::stored("Tags")),
                        (ZoomedOutLabels::Off, Signal::stored("Off")),
                    ]
                    selected=zoomed_out
                    on_select=set_zoomed_out
                />
                <SettingsSwitch label="Readable Font" shortcut="F" hint="Plain text instead of the pixel font" active=readable_font />
                <SettingsColorRow label="Territory name color" color=name_color />
                <SettingsColorRow label="Guild tag color" color=tag_color />
            </SettingsSection>

            <SettingsSection title="Timers">
                <SettingsChoice
                    label="Time held"
                    name="settings-time-held"
                    options=vec![
                        (TimeHeld::Off, time_held_off_label),
                        (TimeHeld::Compact, Signal::stored("3h05m")),
                        (TimeHeld::Exact, Signal::stored("03:05:12")),
                    ]
                    selected=time_held
                    on_select=set_time_held
                />
                <SettingsSwitch label="Cooldown Countdown" shortcut="T" hint="Time left on a new capture's 10-minute cooldown" active=show_countdown />
                <SettingsSwitch label="Thick Cooldown Borders" active=thick_cooldown_borders />
            </SettingsSection>

            // Only for viewers who can actually see the war feed - the same rule the
            // panels themselves follow. For everyone else these would be dead toggles.
            <Show when=move || war_feed_visible.get()>
                <SettingsSection title="War">
                    <SettingsSwitch label="War Queue" active=show_war_queue />
                    // Desktop only: `WarStatsStrip` never renders on a phone.
                    <Show when=move || !is_mobile.get()>
                        <SettingsSwitch label="War Stats" active=show_war_stats />
                    </Show>
                    <SettingsSwitch label="Teammate Heads" active=show_player_heads />
                    <Show when=move || show_player_heads.get()>
                        <SettingsSwitch label="Show Faces" hint="A dot when off" nested=true active=player_head_render_head />
                        <SettingsSwitch label="Show Names" hint="Hidden when zoomed far out" nested=true active=player_head_render_label />
                    </Show>
                </SettingsSection>
            </Show>

            <SettingsSection title="Leaderboard">
                <SettingsSwitch label="Territories" active=show_leaderboard_territory_count />
                <SettingsSwitch label="Online Members" active=show_leaderboard_online />
                <SettingsSwitch label="SR Rate" hint="Estimated SR per hour; per 5 minutes in history" active=show_leaderboard_sr_gain />
                <SettingsSwitch label="Season Rating" hint="Always shown when sorted by rating" active=show_leaderboard_sr_value />
                <SettingsSwitch label="Estimate SR Scalar" hint="From season data when available" active=auto_sr_scalar_enabled />
                <SettingsScalarRow scalar=manual_sr_scalar />
            </SettingsSection>

            <SettingsDisclosure title="Sizes and lines" hint="Label, icon, connection and head sizes">
                <div class="settings-subhead">"Labels and icons"</div>
                <SettingsSlider
                    label="Overall"
                    value=label_scale_master
                    min=LABEL_SCALE_MASTER_MIN
                    max=LABEL_SCALE_MASTER_MAX
                    step=0.05
                    clamp=clamp_label_scale_master
                    format=percent
                />
                <SettingsSlider
                    label="Guild tags"
                    value=label_scale_static_tag
                    min=LABEL_SCALE_GROUP_MIN
                    max=LABEL_SCALE_GROUP_MAX
                    step=0.05
                    clamp=clamp_label_scale_group
                    format=percent
                />
                <SettingsSlider
                    label="Territory names"
                    value=label_scale_static_name
                    min=LABEL_SCALE_GROUP_MIN
                    max=LABEL_SCALE_GROUP_MAX
                    step=0.05
                    clamp=clamp_label_scale_group
                    format=percent
                />
                <SettingsSlider
                    label="Timers"
                    value=label_scale_dynamic
                    min=LABEL_SCALE_GROUP_MIN
                    max=LABEL_SCALE_GROUP_MAX
                    step=0.05
                    clamp=clamp_label_scale_group
                    format=percent
                />
                <SettingsSlider
                    label="Icons"
                    value=label_scale_icons
                    min=LABEL_SCALE_GROUP_MIN
                    max=LABEL_SCALE_GROUP_MAX
                    step=0.05
                    clamp=clamp_label_scale_group
                    format=percent
                />
                <SettingsResetButton
                    label="Reset label sizes"
                    on_reset=Callback::new(move |()| {
                        label_scale_master.set(DEFAULT_LABEL_SCALE_MASTER);
                        label_scale_static_tag.set(DEFAULT_LABEL_SCALE_STATIC_TAG);
                        label_scale_static_name.set(DEFAULT_LABEL_SCALE_STATIC_NAME);
                        label_scale_dynamic.set(DEFAULT_LABEL_SCALE_GROUP);
                        label_scale_icons.set(DEFAULT_LABEL_SCALE_GROUP);
                    })
                />

                <div class="settings-subhead">"Connections"</div>
                <SettingsSwitch label="Bold Connections" shortcut="B" active=bold_connections />
                <SettingsSlider
                    label="Opacity"
                    value=connection_opacity_scale
                    min=CONNECTION_OPACITY_SCALE_MIN
                    max=CONNECTION_OPACITY_SCALE_MAX
                    step=0.05
                    clamp=clamp_connection_opacity_scale
                    format=percent
                />
                <SettingsSlider
                    label="Thickness"
                    value=connection_thickness_scale
                    min=CONNECTION_THICKNESS_SCALE_MIN
                    max=CONNECTION_THICKNESS_SCALE_MAX
                    step=0.05
                    clamp=clamp_connection_thickness_scale
                    format=percent
                />
                <SettingsResetButton
                    label="Reset line sliders"
                    on_reset=Callback::new(move |()| {
                        connection_opacity_scale.set(DEFAULT_CONNECTION_OPACITY_SCALE);
                        connection_thickness_scale.set(DEFAULT_CONNECTION_THICKNESS_SCALE);
                    })
                />

                <Show when=war_heads_visible>
                    <div class="settings-subhead">"Teammate heads"</div>
                    <SettingsSlider
                        label="Head size"
                        value=player_head_size
                        min=PLAYER_HEAD_SIZE_MIN
                        max=PLAYER_HEAD_SIZE_MAX
                        step=1.0
                        clamp=clamp_player_head_size
                        format=pixels
                    />
                </Show>
            </SettingsDisclosure>

            <SettingsDisclosure title="Advanced" hint="Diagnostics">
                <SettingsSwitch label="API Status Badge" hint="Live data source status in the sidebar header" active=show_debug_info />
                <div class="settings-row settings-info">
                    <span class="settings-row-label">"Territories loaded"</span>
                    <span class="settings-value">{move || territory_count.get()}</span>
                </div>
            </SettingsDisclosure>
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
/// without extra code; it starts closed every time the menu opens.
#[component]
fn SettingsDisclosure(
    title: &'static str,
    hint: &'static str,
    children: Children,
) -> impl IntoView {
    view! {
        <details class="settings-disclosure">
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
    active: RwSignal<bool>,
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
    let hint_id = hint.map(|_| format!("settings-hint-{}", slug(label)));
    view! {
        <button
            type="button"
            role="switch"
            class="settings-row settings-switch"
            class:settings-row-nested=nested
            aria-label=label
            aria-checked=move || active.get().to_string()
            aria-describedby=hint_id.clone()
            on:click=move |_| active.update(|value| *value = !*value)
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
    #[prop(into)] selected: Signal<T>,
    on_select: Callback<T>,
    #[prop(optional)] hint: Option<&'static str>,
) -> impl IntoView
where
    T: Copy + PartialEq + Send + Sync + 'static,
{
    let label_id = format!("{name}-label");
    let labelled_by = label_id.clone();
    view! {
        <div class="settings-row settings-choice" role="radiogroup" aria-labelledby=labelled_by>
            <span class="settings-row-text">
                <span class="settings-row-label" id=label_id>{label}</span>
                {hint.map(|hint| view! { <span class="settings-row-hint">{hint}</span> })}
            </span>
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
                                    on:change=move |_| on_select.run(value)
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

/// A labelled range input. Dragging writes through live; the shown value uses `format`.
#[component]
fn SettingsSlider(
    label: &'static str,
    value: RwSignal<f64>,
    min: f64,
    max: f64,
    step: f64,
    clamp: fn(f64) -> f64,
    format: fn(f64) -> String,
) -> impl IntoView {
    let slider_ref = NodeRef::<leptos::html::Input>::new();
    let dragging: RwSignal<bool> = RwSignal::new(false);

    // Outside changes (reset, other tabs of the menu) move the thumb, but not mid-drag.
    Effect::new(move || {
        let external = clamp(value.get());
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
    let on_input = move |e: leptos::ev::Event| {
        if let Some(parsed) = read(&e) {
            dragging.set(true);
            value.set(clamp(parsed));
        }
    };
    let on_change = move |e: leptos::ev::Event| {
        if let Some(parsed) = read(&e) {
            value.set(clamp(parsed));
        }
        dragging.set(false);
    };

    view! {
        <label class="settings-slider">
            <span class="settings-slider-label">{label}</span>
            <input
                node_ref=slider_ref
                type="range"
                class="timeline-slider"
                min=min
                max=max
                step=step
                value=format!("{:.2}", clamp(value.get_untracked()))
                aria-valuetext=move || format(value.get())
                on:input=on_input
                on:change=on_change
            />
            <span class="settings-value" aria-hidden="true">{move || format(value.get())}</span>
        </label>
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
fn SettingsColorRow(label: &'static str, color: RwSignal<NameColor>) -> impl IntoView {
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
                                on:click=move |_| color.set(variant)
                            />
                        }
                    })
                    .collect_view()}
            </div>
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
fn SettingsScalarRow(scalar: RwSignal<f64>) -> impl IntoView {
    let input_ref = NodeRef::<leptos::html::Input>::new();
    // Set while the field holds unfinished text, so outside updates do not overwrite it.
    let editing = RwSignal::new(false);
    let show = move |input: &web_sys::HtmlInputElement| {
        input.set_value(&format!("{:.2}", scalar.get_untracked()));
    };

    // Outside changes (Reset all, load) reach the field whenever no edit is in progress.
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
            scalar.set(clamp_manual_scalar(value));
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
        <label class="settings-row settings-number">
            <span class="settings-row-text">
                <span class="settings-row-label">"Manual SR Scalar"</span>
                <span class="settings-row-hint">"Used when estimating is off or unavailable"</span>
            </span>
            <input
                node_ref=input_ref
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
        </label>
    }
}

/// A small right-aligned button that resets one group of values; the label names the group.
#[component]
fn SettingsResetButton(label: &'static str, on_reset: Callback<()>) -> impl IntoView {
    view! {
        <div class="settings-reset-row">
            <button type="button" class="settings-reset" on:click=move |_| on_reset.run(())>
                {label}
            </button>
        </div>
    }
}

/// Heat map source and season, shown while the heat map is on.
#[component]
fn HeatOptions() -> impl IntoView {
    let CurrentMode(mode) = expect_context();
    let HeatLiveSourceSetting(live_source) = expect_context();
    let HeatHistoryBasisSetting(history_basis) = expect_context();
    let HeatSelectedSeasonId(season_id) = expect_context();
    let HeatMetaState(meta) = expect_context();
    let HeatFallbackApplied(fallback_applied) = expect_context();
    let HeatWindowLabel(window_label) = expect_context();
    let is_history = move || mode.get() == MapMode::History;

    // Live mode picks a source, history a cumulative basis; both are season or all-time.
    let season_wide = Signal::derive(move || {
        if is_history() {
            history_basis.get() == HeatHistoryBasis::SeasonCumulative
        } else {
            live_source.get() == HeatLiveSource::Season
        }
    });
    let set_season_wide = Callback::new(move |season: bool| {
        if mode.get_untracked() == MapMode::History {
            history_basis.set(if season {
                HeatHistoryBasis::SeasonCumulative
            } else {
                HeatHistoryBasis::AllTimeCumulative
            });
        } else {
            live_source.set(if season {
                HeatLiveSource::Season
            } else {
                HeatLiveSource::AllTime
            });
        }
    });

    view! {
        <div class="settings-row-nested settings-heat">
            <SettingsChoice
                label="Heat period"
                name="settings-heat-period"
                options=vec![
                    (true, Signal::stored("Season")),
                    (false, Signal::stored("All-time")),
                ]
                selected=season_wide
                on_select=set_season_wide
            />
            <Show when=move || season_wide.get()>
                <SettingsHeatSeasonRow season_id=season_id meta=meta />
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
fn SettingsHeatSeasonRow(
    season_id: RwSignal<Option<i32>>,
    meta: RwSignal<Option<HistoryHeatMeta>>,
) -> impl IntoView {
    let on_change = move |e: leptos::ev::Event| {
        let Some(select) = e
            .target()
            .and_then(|target| target.dyn_into::<web_sys::HtmlSelectElement>().ok())
        else {
            return;
        };
        let value = select.value();
        if value == "latest" {
            season_id.set(None);
        } else {
            season_id.set(value.parse::<i32>().ok());
        }
    };

    view! {
        <label class="settings-row settings-number">
            <span class="settings-row-label">"Season"</span>
            <select class="seq-input" on:change=on_change>
                <option value="latest" selected=move || season_id.get().is_none()>
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
                                            selected=move || season_id.get() == Some(season_id_value)
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
        </label>
    }
}

#[cfg(test)]
mod tests {
    use super::{Overlay, TimeHeld, ZoomedOutLabels, parse_scalar_edit};

    #[test]
    fn scalar_edits_accept_only_positive_numbers() {
        assert_eq!(parse_scalar_edit("0.5"), Some(0.5));
        assert_eq!(parse_scalar_edit(" 1.25 "), Some(1.25));
        assert_eq!(parse_scalar_edit("25"), Some(25.0)); // clamped on commit, not here
        for rejected in ["", " ", "0", "-2", "abc", "NaN", "inf", "1e999"] {
            assert_eq!(parse_scalar_edit(rejected), None, "{rejected:?}");
        }
    }

    #[test]
    fn overlay_choice_round_trips_the_exclusive_flags() {
        for choice in [
            Overlay::Off,
            Overlay::Resources,
            Overlay::Defense,
            Overlay::Intel,
        ] {
            let (resources, defense, intel) = choice.flags();
            assert_eq!(Overlay::from_flags(resources, defense, intel), choice);
        }
        // Resources wins if stale settings ever held two at once, as the app's own
        // exclusivity effects do.
        assert_eq!(Overlay::from_flags(true, true, false), Overlay::Resources);
    }

    #[test]
    fn zoomed_out_labels_match_what_the_renderer_draws() {
        // Guild names replace the tags whatever the tag flag says.
        assert_eq!(
            ZoomedOutLabels::from_flags(true, false),
            ZoomedOutLabels::GuildNames
        );
        assert_eq!(
            ZoomedOutLabels::from_flags(true, true),
            ZoomedOutLabels::GuildNames
        );
        assert_eq!(
            ZoomedOutLabels::from_flags(false, true),
            ZoomedOutLabels::TerritoryTags
        );
        assert_eq!(
            ZoomedOutLabels::from_flags(false, false),
            ZoomedOutLabels::Off
        );
        // Picking guild names leaves the dormant tag flag alone.
        assert_eq!(ZoomedOutLabels::GuildNames.flags(true), (true, true));
        assert_eq!(ZoomedOutLabels::GuildNames.flags(false), (true, false));
        assert_eq!(ZoomedOutLabels::TerritoryTags.flags(false), (false, true));
        assert_eq!(ZoomedOutLabels::Off.flags(true), (false, false));
    }

    #[test]
    fn time_held_matches_the_renderer_precedence() {
        // Exact overrides the compact format.
        assert_eq!(TimeHeld::from_flags(true, true), TimeHeld::Exact);
        assert_eq!(TimeHeld::from_flags(true, false), TimeHeld::Exact);
        assert_eq!(TimeHeld::from_flags(false, true), TimeHeld::Compact);
        assert_eq!(TimeHeld::from_flags(false, false), TimeHeld::Off);
        assert_eq!(TimeHeld::Exact.flags(true), (true, true));
        assert_eq!(TimeHeld::Compact.flags(false), (false, true));
        assert_eq!(TimeHeld::Off.flags(true), (false, false));
        for choice in [TimeHeld::Off, TimeHeld::Compact, TimeHeld::Exact] {
            let (exact, compact) = choice.flags(true);
            assert_eq!(TimeHeld::from_flags(exact, compact), choice);
        }
    }
}
