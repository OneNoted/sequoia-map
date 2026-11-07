//! The sidebar: search, the guild leaderboard, a guild, a territory, settings and the toolbar.
//! Every view opens with the same navigation row and uses the same gutters, headings and
//! controls, so moving between them changes the content and not the layout.

use leptos::prelude::*;
use std::collections::HashMap;

use sequoia_shared::{
    DataProvenance, TerritoryRuntimeData, passive_sr_per_5s, passive_sr_per_hour,
};

use crate::app::{
    CurrentMode, DetailReturnGuild, GuildColorStore, GuildOnlineData, HeatEntriesByTerritory,
    HistoryAvailable, HistoryBoundsSignal, HistoryBufferModeActive, HistoryBufferedUpdates,
    HistoryFetchNonce, HistoryLegacyGeometryActive, HistoryMapTime, HistorySeasonLeaderboard,
    HistorySeasonScalarSample, HistoryTimestamp, IsMobile, LastLiveSeq, LeaderboardSortBySr,
    LiveHandoffResyncCount, LiveSeasonScalarSample, MapClock, MapMode, NeedsLiveResync,
    PlaybackActive, Selected, SelectedGuild, ShowSettings, SidebarIndex, SidebarItems, SidebarOpen,
    SidebarTransient, TerritoryGeometryStore,
};
use crate::colors::rgba_css;
use crate::format::{compact, rate, since};
use crate::history;
use crate::keybinds::KeybindHelp;
use crate::season_scalar::{EffectiveScalar, ScalarSource, effective_scalar};
use crate::settings::{self, ViewerSettings};
use crate::settings_menu::SettingsPanel;
use crate::territory::ClientTerritoryMap;
use crate::territory_card::{
    DefenseValue, ResourceRow, ResourceTable, TerritoryHead, TerritoryWar, TreasuryValue, cooldown,
    territory_resource_rows, treasury,
};
use crate::tower::TowerCalculator;
use crate::ui::{Disclosure, MapPinIcon, PanelNav, SCROLLER, ViewTitle, scroll_padding_top};
use crate::war_room::ActivityDisclosure;
use sequoia_browser_map::live_feed::ConnectionStatus;
use sequoia_browser_map::{BrowserMap, MapCamera};

/// Newer than this, the live feed counts as working.
const LIVE_FRESH_SECS: i64 = 120;

/// How long a guild's territory list waits for its online members when nothing says how many
/// there will be, so they take their place above it instead of pushing it down. A slower answer
/// gives the list back to the reader.
const GUILD_LIST_HOLD: std::time::Duration = std::time::Duration::from_millis(2000);

fn format_confidence(confidence: f32) -> String {
    format!("{:.0}%", (confidence.clamp(0.0, 1.0) * 100.0))
}

fn format_age_seconds(age_secs: i64) -> String {
    let secs = age_secs.max(0);
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        let mins = secs / 60;
        return format!("{mins}m {}s", secs % 60);
    }
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    format!("{hours}h {mins}m")
}

fn visibility_label(provenance: &DataProvenance) -> &'static str {
    match provenance.visibility {
        sequoia_shared::VisibilityClass::Public => "Public",
        sequoia_shared::VisibilityClass::GuildOptIn => "Guild Opt-In",
    }
}

fn live_source_display_label(source: &str) -> Option<&'static str> {
    if source.eq_ignore_ascii_case("wynncraft_api") {
        Some("Wynncraft API")
    } else if source.eq_ignore_ascii_case("iris") || source.eq_ignore_ascii_case("fabric_reporter")
    {
        Some("iris")
    } else {
        None
    }
}

/// A guild's colour as the swatch and card edges use it.
fn guild_css((r, g, b): (u8, u8, u8)) -> String {
    rgba_css(r, g, b, 0.9)
}

/// What a row in the search results or the leaderboard stands for. A click and Enter both open
/// it through [`OpenEntry`], so they cannot disagree about what the row is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SidebarEntry {
    Territory(String),
    Guild(String),
}

/// Opens a sidebar row. Built from context while a component is set up, so it can run from
/// event handlers.
#[derive(Clone, Copy)]
pub(crate) struct OpenEntry {
    territories: RwSignal<ClientTerritoryMap>,
    camera: MapCamera,
    mode: RwSignal<MapMode>,
    selected: RwSignal<Option<String>>,
    selected_guild: RwSignal<Option<String>>,
    detail_return_guild: RwSignal<Option<String>>,
    sidebar_open: RwSignal<bool>,
}

impl OpenEntry {
    pub fn from_context() -> Self {
        let CurrentMode(mode) = expect_context();
        let Selected(selected) = expect_context();
        let SelectedGuild(selected_guild) = expect_context();
        let DetailReturnGuild(detail_return_guild) = expect_context();
        let SidebarOpen(sidebar_open) = expect_context();
        Self {
            territories: expect_context(),
            camera: expect_context::<BrowserMap>().camera(),
            mode,
            selected,
            selected_guild,
            detail_return_guild,
            sidebar_open,
        }
    }

    /// A territory opens its detail with the map on it; a guild opens its panel. Enter can
    /// reach a row while the sidebar is closed, and on the live map the sidebar opens to show it.
    pub fn run(&self, entry: &SidebarEntry) {
        match entry {
            SidebarEntry::Territory(name) => {
                self.detail_return_guild.set(None);
                self.selected.set(Some(name.clone()));
                if let Some(region) = self
                    .territories
                    .with_untracked(|map| map.get(name).map(|ct| ct.territory.location.clone()))
                {
                    self.camera.focus(&region);
                }
            }
            SidebarEntry::Guild(name) => self.selected_guild.set(Some(name.clone())),
        }
        if self.mode.get_untracked() == MapMode::Live && !self.sidebar_open.get_untracked() {
            self.sidebar_open.set(true);
        }
    }
}

/// Sidebar with search, leaderboard, detail panel, and stats.
#[component]
pub fn Sidebar() -> impl IntoView {
    let search_query: RwSignal<String> = expect_context();
    let Selected(selected) = expect_context();
    let SelectedGuild(selected_guild) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let SidebarIndex(sidebar_index) = expect_context();
    let SidebarItems(sidebar_items) = expect_context();
    let ShowSettings(show_settings) = expect_context();

    // Scroll focused item into view when index changes
    Effect::new(move || {
        if !sidebar_open.get() {
            return;
        }
        let idx = sidebar_index.get();
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let Ok(Some(scroll_el)) = document.query_selector(SCROLLER) else {
            return;
        };
        let Ok(Some(item_el)) = scroll_el.query_selector(&format!("[data-sidebar-idx='{idx}']"))
        else {
            return;
        };

        // Keep keyboard-focused rows visible by adjusting only the sidebar's
        // internal vertical scroll position (never root page scroll), below the sticky nav row.
        let scroll_rect = scroll_el.get_bounding_client_rect();
        let item_rect = item_el.get_bounding_client_rect();
        let current_top = scroll_el.scroll_top();
        let top = scroll_rect.top() + scroll_padding_top(&scroll_el);
        if item_rect.top() < top {
            let delta = (item_rect.top() - top).floor() as i32;
            scroll_el.set_scroll_top(current_top + delta);
        } else if item_rect.bottom() > scroll_rect.bottom() {
            let delta = (item_rect.bottom() - scroll_rect.bottom()).ceil() as i32;
            scroll_el.set_scroll_top(current_top + delta);
        }
    });

    #[derive(Clone, Copy, PartialEq)]
    enum View {
        Settings,
        Guild,
        Territory,
        Leaderboard,
        Search,
    }
    let shown = Memo::new(move |_| {
        if show_settings.get() {
            View::Settings
        } else if selected_guild.get().is_some() {
            View::Guild
        } else if selected.get().is_some() {
            View::Territory
        } else if search_query.with(String::is_empty) {
            View::Leaderboard
        } else {
            View::Search
        }
    });

    view! {
        <div class="sidebar-inner" class:sidebar-animate=move || sidebar_open.get() style:display=move || if sidebar_open.get() { "flex" } else { "none" }>
            <HistoryWhen />
            <SearchBar />
            <div data-sidebar-scroll="" class="sb-scroll scrollbar-thin">
                {move || {
                    let view = shown.get();
                    if !matches!(view, View::Leaderboard | View::Search) {
                        sidebar_items.set(Vec::new());
                        sidebar_index.set(0);
                    }
                    match view {
                        View::Settings => view! { <SettingsPanel /> }.into_any(),
                        View::Guild => view! { <GuildPanel /> }.into_any(),
                        View::Territory => view! { <DetailPanel /> }.into_any(),
                        View::Leaderboard => view! { <LeaderboardPanel /> }.into_any(),
                        View::Search => view! { <SearchResults /> }.into_any(),
                    }
                }}
            </div>
            <SidebarToolbar />
        </div>
    }
}

/// On a phone the sheet covers the History bar, so while History is shown the sheet keeps
/// its selected time in view; pressing it closes the sheet onto the bar.
#[component]
fn HistoryWhen() -> impl IntoView {
    let IsMobile(is_mobile) = expect_context();
    let CurrentMode(mode) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let HistoryTimestamp(history_timestamp) = expect_context();
    let when = move || {
        history_timestamp
            .get()
            .map(crate::timeline::format_history_timestamp)
            .unwrap_or_default()
    };
    view! {
        <Show when=move || is_mobile.get() && mode.get() == MapMode::History>
            <button
                type="button"
                class="sb-history-when"
                aria-label=move || format!("History at {}, show the timeline", when())
                on:click=move |_| sidebar_open.set(false)
            >
                <span class="sb-history-when-time">{when}</span>
                <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" width="14" height="14" aria-hidden="true">
                    <path d="M4 6l4 4 4-4" />
                </svg>
            </button>
        </Show>
    }
}

#[component]
fn SearchBar() -> impl IntoView {
    let search_query: RwSignal<String> = expect_context();
    let show_debug_info = expect_context::<ViewerSettings>().value(settings::DIAGNOSTICS);

    view! {
        <div class="sb-search">
            <div class="sb-search-field">
                <svg class="sb-search-icon" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" width="14" height="14" aria-hidden="true">
                    <path fill-rule="evenodd" d="M9 3.5a5.5 5.5 0 100 11 5.5 5.5 0 000-11zM2 9a7 7 0 1112.452 4.391l3.328 3.329a.75.75 0 11-1.06 1.06l-3.329-3.328A7 7 0 012 9z" clip-rule="evenodd" />
                </svg>
                <input
                    data-search-input=""
                    class="sb-search-input"
                    type="text"
                    placeholder="Search territories or guilds..."
                    aria-label="Search territories or guilds"
                    prop:value=move || search_query.get()
                    on:input=move |event| search_query.set(event_target_value(&event))
                />
                <kbd class="sb-search-key" aria-hidden="true">"/"</kbd>
            </div>
            <Show when=move || show_debug_info.get()>
                <LiveDataBadge />
            </Show>
        </div>
    }
}

/// Diagnostics only: whether live territory data is arriving, and from where.
#[component]
fn LiveDataBadge() -> impl IntoView {
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let tick: RwSignal<i64> = expect_context();

    #[derive(Clone, PartialEq)]
    struct Latest {
        observed: i64,
        observed_at: String,
        source: &'static str,
        reporters: u16,
        visibility: &'static str,
        confidence: String,
    }

    let latest = Memo::new(move |_| {
        territories.with(|map| {
            let now = tick.get_untracked();
            map.values()
                .filter_map(|ct| {
                    let provenance = ct.territory.runtime.as_ref()?.provenance.as_ref()?;
                    let source = live_source_display_label(provenance.source.trim())?;
                    Some(Latest {
                        observed: chrono::DateTime::parse_from_rfc3339(&provenance.observed_at)
                            .map_or(now, |time| time.timestamp()),
                        observed_at: provenance.observed_at.clone(),
                        source,
                        reporters: provenance.reporter_count,
                        visibility: visibility_label(provenance),
                        confidence: format_confidence(provenance.confidence),
                    })
                })
                .max_by_key(|latest| latest.observed)
        })
    });
    let status = Memo::new(move |_| {
        let now = tick.get();
        latest.with(|latest| match latest {
            Some(latest) => {
                let age = (now - latest.observed).max(0);
                let fresh = age <= LIVE_FRESH_SECS;
                let working = fresh && (latest.reporters > 0 || latest.source == "Wynncraft API");
                let reason = if working {
                    "recent Wynncraft API territory data is arriving".to_string()
                } else if !fresh {
                    format!(
                        "latest Wynncraft API data is stale ({} old)",
                        format_age_seconds(age)
                    )
                } else {
                    "no active live data source".to_string()
                };
                let facts = vec![
                    format!("Observed: {}", since(&latest.observed_at, now)),
                    format!("Age: {}", format_age_seconds(age)),
                    format!("Source: {}", latest.source),
                    format!("Samples: {}", latest.reporters),
                    format!("Visibility: {}", latest.visibility),
                    format!("Confidence: {}", latest.confidence),
                    format!("Freshness window: {LIVE_FRESH_SECS}s"),
                ];
                (working, reason, facts)
            }
            None => (
                false,
                "no live API data received yet".to_string(),
                vec![
                    "Source: none".to_string(),
                    "Observed: no observations yet".to_string(),
                ],
            ),
        })
    });
    let label = move || {
        status.with(|(working, reason, facts)| {
            let state = if *working { "Working" } else { "Not Working" };
            format!("Live data: {state}\nReason: {reason}\n{}", facts.join("\n"))
        })
    };

    view! {
        <div class="sb-live" data-working=move || status.with(|status| status.0).to_string()>
            <a class="sb-live-badge" href="https://docs.wynncraft.com/" target="_blank" rel="noopener noreferrer" title="Open Wynncraft API documentation" aria-label=label>
                <span class="sb-live-dot" aria-hidden="true" />
                "API"
            </a>
            <div class="sb-live-tip" aria-hidden="true">
                <div class="sb-live-state">{move || format!("API {}", if status.with(|status| status.0) { "Working" } else { "Not Working" })}</div>
                <div class="sb-live-reason">{move || status.with(|status| status.1.clone())}</div>
                <ul class="sb-live-facts">
                    {move || status.with(|status| status.2.iter().map(|fact| view! { <li>{fact.clone()}</li> }).collect_view())}
                </ul>
            </div>
        </div>
    }
}

#[component]
fn SearchResults() -> impl IntoView {
    let search_query: RwSignal<String> = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let open_entry = OpenEntry::from_context();
    let SidebarIndex(sidebar_index) = expect_context();
    let SidebarItems(sidebar_items) = expect_context();

    let filtered = Memo::new(move |_| {
        let query = search_query.get().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let map = territories.get();
        let mut results: Vec<_> = map
            .iter()
            .filter(|(name, ct)| {
                name.to_lowercase().contains(&query)
                    || ct.territory.guild.name.to_lowercase().contains(&query)
                    || ct.territory.guild.prefix.to_lowercase().contains(&query)
            })
            .map(|(name, ct)| {
                (
                    name.clone(),
                    ct.territory.guild.name.clone(),
                    ct.guild_color,
                )
            })
            .collect();
        results.sort_by(|a, b| a.0.cmp(&b.0));
        results.truncate(50);
        results
    });

    // Sync sidebar items for keyboard navigation
    Effect::new(move || {
        let items: Vec<SidebarEntry> = filtered.with(|found| {
            found
                .iter()
                .map(|(name, ..)| SidebarEntry::Territory(name.clone()))
                .collect()
        });
        if items != sidebar_items.get_untracked() {
            sidebar_index.set(0);
        }
        sidebar_items.set(items);
    });

    view! {
        <section class="sb-list-section" aria-label="Search results">
            <PanelNav>
                <span class="sb-list-count" role="status">{move || format!("{} found", filtered.with(Vec::len))}</span>
            </PanelNav>
            <ViewTitle>"Search Results"</ViewTitle>
            <ul class="sb-list">
                <For
                    each=move || filtered.get()
                    // Rows are not redrawn in place, so the key covers the owner and colour they show.
                    key=|row| row.clone()
                    children=move |(name, guild, color)| {
                        // A kept row can move as the query changes, so its place in the list is
                        // read from the current results rather than fixed when it was made.
                        let index = Memo::new({
                            let name = name.clone();
                            move |_| filtered.with(|found| found.iter().position(|(found, ..)| *found == name))
                        });
                        let entry = SidebarEntry::Territory(name.clone());
                        let on_click = move |_| open_entry.run(&entry);
                        view! {
                            <li data-sidebar-idx=move || index.get().map(|index| index.to_string()) class="sb-row" class:sb-row-focused=move || index.get() == Some(sidebar_index.get()) on:click=on_click>
                                <span class="guild-mark" style:--swatch=guild_css(color) aria-hidden="true" />
                                <span class="sb-row-text">
                                    <span class="sb-row-name">{name}</span>
                                    <span class="sb-row-sub">{guild}</span>
                                </span>
                            </li>
                        }
                    }
                />
            </ul>
        </section>
    }
}

/// One leaderboard line.
#[derive(Clone, PartialEq)]
struct GuildRow {
    name: String,
    prefix: String,
    territories: usize,
    color: (u8, u8, u8),
    passive_sr_per_hour: f64,
    season_rating: Option<i64>,
    sr_gain_5m: Option<i64>,
}

#[component]
fn LeaderboardPanel() -> impl IntoView {
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let open_entry = OpenEntry::from_context();
    let SidebarIndex(sidebar_index) = expect_context();
    let SidebarItems(sidebar_items) = expect_context();
    let CurrentMode(mode) = expect_context();
    let settings = expect_context::<ViewerSettings>();
    let show_leaderboard_sr_gain = settings.value(settings::LEADERBOARD_SR_RATE);
    let show_leaderboard_sr_value = settings.value(settings::LEADERBOARD_SEASON_RATING);
    let show_leaderboard_territory_count = settings.value(settings::LEADERBOARD_TERRITORIES);
    let show_leaderboard_online = settings.value(settings::LEADERBOARD_ONLINE);
    let GuildOnlineData(guild_online_data) = expect_context();
    let HistorySeasonLeaderboard(history_sr_leaderboard) = expect_context();
    let LeaderboardSortBySr(sort_by_sr) = expect_context();
    let scalar_state = scalar_state();

    let leaderboard = Memo::new(move |_| {
        let map = territories.get();
        let online_data = guild_online_data.get();
        let history_sr = history_sr_leaderboard.get().unwrap_or_default();
        let sort_sr = sort_by_sr.get();
        let history_mode = mode.get() == MapMode::History;
        let history_sr_map: HashMap<String, (i64, Option<i64>)> = history_sr
            .into_iter()
            .map(|entry| (entry.guild_name, (entry.season_rating, entry.sr_gain_5m)))
            .collect();
        let mut guilds: HashMap<&str, GuildRow> = HashMap::new();
        for ct in map.values() {
            let guild = &ct.territory.guild;
            guilds
                .entry(guild.name.as_str())
                .or_insert_with(|| GuildRow {
                    name: guild.name.clone(),
                    prefix: guild.prefix.clone(),
                    territories: 0,
                    color: ct.guild_color,
                    passive_sr_per_hour: 0.0,
                    season_rating: None,
                    sr_gain_5m: None,
                })
                .territories += 1;
        }
        let rating = |name: &str| {
            if history_mode {
                history_sr_map.get(name).map(|value| value.0)
            } else {
                online_data.get(name).and_then(|data| data.season_rating)
            }
        };
        let mut sorted: Vec<GuildRow> = guilds.into_values().collect();
        sorted.sort_by(|a, b| {
            let by_count = b
                .territories
                .cmp(&a.territories)
                .then_with(|| a.name.cmp(&b.name));
            if !sort_sr {
                return by_count;
            }
            if history_mode {
                match (rating(&a.name), rating(&b.name)) {
                    (Some(a), Some(b)) => b.cmp(&a).then(by_count),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => by_count,
                }
            } else {
                // Live, a guild without a rating ranks as zero.
                let rated = |name: &str| rating(name).unwrap_or(0);
                rated(&b.name).cmp(&rated(&a.name)).then(by_count)
            }
        });
        sorted.truncate(20);

        let scalar = scalar_state.get().value;
        for row in &mut sorted {
            row.passive_sr_per_hour = passive_sr_per_hour(row.territories, scalar);
            row.season_rating = rating(&row.name);
            if history_mode {
                row.sr_gain_5m = history_sr_map.get(&row.name).and_then(|value| value.1);
            }
        }
        sorted
    });

    // Sync sidebar items for keyboard navigation
    Effect::new(move || {
        let items: Vec<SidebarEntry> = leaderboard.with(|rows| {
            rows.iter()
                .map(|row| SidebarEntry::Guild(row.name.clone()))
                .collect()
        });
        if items != sidebar_items.get_untracked() {
            sidebar_index.set(0);
        }
        sidebar_items.set(items);
    });

    let is_empty = Memo::new(move |_| territories.with(|map| map.is_empty()));
    let sort_button = move |by_sr: bool, label: &'static str| {
        view! {
            <button type="button" class="sb-sort" data-sort=if by_sr { "sr" } else { "territories" } aria-pressed=move || (sort_by_sr.get() == by_sr).to_string() on:click=move |_| sort_by_sr.set(by_sr)>
                {label}
            </button>
        }
    };

    view! {
        <section class="sb-list-section" aria-labelledby="sb-top-guilds">
            <PanelNav>
                <div class="sb-sorts" role="group" aria-label="Sort guilds by">
                    {sort_button(false, "Territories")}
                    {sort_button(true, "SR")}
                </div>
            </PanelNav>
            <ViewTitle id="sb-top-guilds">"Top Guilds"</ViewTitle>
            <Show
                when=move || !is_empty.get()
                fallback=|| view! {
                    <div class="sb-empty" role="status">
                        <div class="status-pulse">"Awaiting territory data..."</div>
                        <div class="sb-empty-dots" aria-hidden="true"><span /><span /><span /></div>
                    </div>
                }
            >
                <ol class="sb-list">
                    <For
                        each=move || { leaderboard.get().into_iter().enumerate().collect::<Vec<_>>() }
                        // A row's content is captured when it is built, so every value it
                        // shows is part of its identity: a changed count or rating rebuilds it.
                        key=move |(index, row)| {
                            (
                                *index,
                                row.name.clone(),
                                row.prefix.clone(),
                                row.color,
                                row.territories,
                                row.season_rating,
                                row.sr_gain_5m,
                                row.passive_sr_per_hour.to_bits(),
                                mode.get_untracked() == MapMode::History,
                            )
                        }
                        children=move |(index, row)| {
                            let rank = index + 1;
                            let history = mode.get_untracked() == MapMode::History;
                            let sr_badge = if history {
                                row.sr_gain_5m.map_or_else(|| "--".to_string(), |gain| {
                                    let sign = if gain >= 0 { "+" } else { "" };
                                    format!("{sign}{}/5m", compact(gain))
                                })
                            } else {
                                format!("{}/h", rate(row.passive_sr_per_hour))
                            };
                            let rating = row.season_rating.map(compact);
                            let entry = SidebarEntry::Guild(row.name.clone());
                            let online_name = row.name.clone();
                            let podium = match rank {
                                1 => "text-gold-gradient",
                                2 => "text-silver-gradient",
                                3 => "text-bronze-gradient",
                                _ => "",
                            };
                            view! {
                                <li
                                    data-sidebar-idx=index.to_string()
                                    class="sb-row sb-guild-row"
                                    class:sb-row-podium=rank <= 3
                                    class:sb-row-enter=!history
                                    class:sb-row-focused=move || sidebar_index.get() == index
                                    style:--swatch=guild_css(row.color)
                                    style:--row-delay=format!("{}ms", rank * 30)
                                    on:click=move |_| open_entry.run(&entry)
                                >
                                    <span class=format!("sb-rank {podium}")>{rank}</span>
                                    <span class="guild-mark" aria-hidden="true" />
                                    <span class="sb-row-name guild-name">{row.name}</span>
                                    <span class="sb-row-tag">"[" {row.prefix} "]"</span>
                                    {move || show_leaderboard_sr_gain.get().then(|| view! { <span class="sb-chip sb-chip-rate">{sr_badge.clone()}</span> })}
                                    {move || (sort_by_sr.get() || show_leaderboard_sr_value.get()).then(|| rating.clone().map(|rating| view! { <span class="sb-chip sb-chip-rating">{rating}</span> }))}
                                    {move || show_leaderboard_territory_count.get().then(|| view! { <span class="sb-chip sb-chip-count">{row.territories}</span> })}
                                    {move || {
                                        if !show_leaderboard_online.get() {
                                            return None;
                                        }
                                        guild_online_data.with(|data| data.get(&online_name).map(|info| info.online)).map(|online| view! {
                                            <span class="sb-chip sb-chip-online"><span aria-hidden="true">{"\u{25CF}"}</span>{online}</span>
                                        })
                                    }}
                                </li>
                            }
                        }
                    />
                </ol>
            </Show>
        </section>
    }
}

/// The SR scalar in use, for passive SR figures.
fn scalar_state() -> Memo<EffectiveScalar> {
    let CurrentMode(mode) = expect_context();
    let settings = expect_context::<ViewerSettings>();
    let manual_sr_scalar = settings.value(settings::MANUAL_SR_SCALAR);
    let auto_sr_scalar_enabled = settings.value(settings::ESTIMATE_SR_SCALAR);
    let LiveSeasonScalarSample(live_scalar_sample) = expect_context();
    let HistorySeasonScalarSample(history_scalar_sample) = expect_context();
    Memo::new(move |_| {
        effective_scalar(
            mode.get(),
            auto_sr_scalar_enabled.get(),
            manual_sr_scalar.get(),
            live_scalar_sample.get(),
            history_scalar_sample.get(),
        )
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OnlineMemberRow {
    username: String,
    rank_label: String,
    rank_priority: u8,
    server: String,
}

pub(crate) fn rank_label_and_priority(rank_key: &str) -> Option<(&'static str, u8)> {
    match rank_key {
        "owner" => Some(("Owner", 0)),
        "chief" => Some(("Chief", 1)),
        "strategist" => Some(("Strategist", 2)),
        "captain" => Some(("Captain", 3)),
        "recruiter" => Some(("Recruiter", 4)),
        "recruit" => Some(("Recruit", 5)),
        _ => None,
    }
}

fn extract_online_members(json: &serde_json::Value) -> Vec<OnlineMemberRow> {
    let Some(members) = json.get("members").and_then(|m| m.as_object()) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for rank in &[
        "owner",
        "chief",
        "strategist",
        "captain",
        "recruiter",
        "recruit",
    ] {
        let Some((rank_label, rank_priority)) = rank_label_and_priority(rank) else {
            continue;
        };
        let Some(rank_obj) = members.get(*rank).and_then(|v| v.as_object()) else {
            continue;
        };
        for (username, info) in rank_obj {
            if info.get("online").and_then(|v| v.as_bool()) == Some(true) {
                let server = info
                    .get("server")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                result.push(OnlineMemberRow {
                    username: username.clone(),
                    rank_label: rank_label.to_string(),
                    rank_priority,
                    server,
                });
            }
        }
    }
    result.sort_by(|a, b| {
        a.rank_priority
            .cmp(&b.rank_priority)
            .then_with(|| {
                a.username
                    .to_ascii_lowercase()
                    .cmp(&b.username.to_ascii_lowercase())
            })
            .then_with(|| a.username.cmp(&b.username))
    });
    result
}

/// What the guild endpoint says about a guild, as the panel shows it.
#[derive(Clone, PartialEq)]
struct GuildFacts {
    prefix: String,
    level: u64,
    online: u64,
    members: u64,
    wars: u64,
    season_rating: Option<(i32, i64)>,
    created: String,
    online_members: Vec<OnlineMemberRow>,
}

impl GuildFacts {
    fn from_json(json: &serde_json::Value) -> Self {
        let text = |key: &str| {
            json.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let number =
            |value: Option<&serde_json::Value>| value.and_then(|v| v.as_u64()).unwrap_or(0);
        let season_rating = json
            .get("seasonRanks")
            .and_then(|v| v.as_object())
            .and_then(|ranks| {
                ranks
                    .iter()
                    .filter_map(|(k, v)| k.parse::<i32>().ok().map(|id| (id, v)))
                    .max_by_key(|(id, _)| *id)
            })
            .and_then(|(season_id, v)| {
                v.get("rating")
                    .and_then(|r| r.as_i64())
                    .map(|r| (season_id, r))
            });
        Self {
            prefix: text("prefix"),
            level: number(json.get("level")),
            online: number(json.get("online")),
            members: number(json.get("members").and_then(|m| m.get("total"))),
            wars: number(json.get("wars")),
            season_rating,
            created: text("created"),
            online_members: extract_online_members(json),
        }
    }
}

/// Where the guild endpoint's answer for the selected guild stands.
#[derive(Clone, PartialEq)]
enum GuildAnswer {
    Waiting,
    Ready(GuildFacts),
    Unavailable,
}

impl GuildAnswer {
    fn facts(&self) -> Option<&GuildFacts> {
        match self {
            Self::Ready(facts) => Some(facts),
            Self::Waiting | Self::Unavailable => None,
        }
    }

    /// One figure: an ellipsis while it is on its way, a dash when it is missing.
    fn figure(&self, pick: impl FnOnce(&GuildFacts) -> String) -> String {
        match self {
            Self::Waiting => return "\u{2026}".to_string(),
            Self::Ready(facts) => Some(pick(facts)).filter(|figure| !figure.is_empty()),
            Self::Unavailable => None,
        }
        .unwrap_or_else(|| "\u{2013}".to_string())
    }
}

#[component]
fn GuildPanel() -> impl IntoView {
    let SelectedGuild(selected_guild) = expect_context();
    let Selected(selected) = expect_context();
    let DetailReturnGuild(detail_return_guild) = expect_context();
    let SidebarTransient(sidebar_transient) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let camera = expect_context::<BrowserMap>().camera();
    let tick: RwSignal<i64> = expect_context();
    let crate::app::WarFeedVisible(viewer_has_war_access) = expect_context();

    let GuildOnlineData(guild_online_data) = expect_context();

    let answer = RwSignal::new(GuildAnswer::Waiting);
    let hold_over = RwSignal::new(false);
    let request_nonce = StoredValue::new(0u64);

    Effect::new(move || {
        let nonce = request_nonce.get_value().wrapping_add(1);
        request_nonce.set_value(nonce);
        answer.set(GuildAnswer::Waiting);
        hold_over.set(false);
        let Some(name) = selected_guild.get() else {
            return;
        };
        wasm_bindgen_futures::spawn_local(async move {
            gloo_timers::future::sleep(GUILD_LIST_HOLD).await;
            if request_nonce.try_get_value() == Some(nonce) {
                hold_over.set(true);
            }
        });
        wasm_bindgen_futures::spawn_local(async move {
            let url = format!("/api/guild/{}", crate::encode_uri_component(&name));
            let detail = match gloo_net::http::Request::get(&url).send().await {
                Ok(resp) if resp.ok() => resp.json::<serde_json::Value>().await.ok(),
                _ => None,
            };
            // Answers for an earlier guild, or for a panel that has closed, are dropped.
            if request_nonce.try_get_value() != Some(nonce) {
                return;
            }
            answer.set(match detail {
                Some(json) => GuildAnswer::Ready(GuildFacts::from_json(&json)),
                None => GuildAnswer::Unavailable,
            });
        });
    });

    // The map already knows a guild that holds territory, so its colour and tag show at once.
    let on_map = Memo::new(move |_| {
        let name = selected_guild.get()?;
        territories.with(|map| {
            map.values()
                .find(|ct| ct.territory.guild.name == name)
                .map(|ct| (ct.guild_color, ct.territory.guild.prefix.clone()))
        })
    });
    let guild_color = move || on_map.with(|guild| guild.as_ref().map(|(color, _)| *color));
    let prefix = move || {
        on_map
            .with(|guild| guild.as_ref().map(|(_, prefix)| prefix.clone()))
            .or_else(|| answer.with(|answer| answer.facts().map(|facts| facts.prefix.clone())))
    };
    // Rows held for the members the leaderboard last counted online, so the list fills its
    // place when the details arrive instead of pushing the territories down.
    let expected_online = move || {
        let name = selected_guild.get()?;
        guild_online_data.with(|data| data.get(&name).map(|info| info.online as usize))
    };
    let guild_territories = Memo::new(move |_| {
        let name = selected_guild.get().unwrap_or_default();
        let mut owned: Vec<String> = territories.with(|map| {
            map.iter()
                .filter(|(_, ct)| ct.territory.guild.name == name)
                .map(|(territory, _)| territory.clone())
                .collect()
        });
        owned.sort();
        owned
    });
    let close = Callback::new(move |()| {
        selected_guild.set(None);
        sidebar_transient.set(false);
    });
    let open_territory = Callback::new(move |territory: String| {
        detail_return_guild.set(selected_guild.get_untracked());
        if let Some(region) = territories
            .with_untracked(|map| map.get(&territory).map(|ct| ct.territory.location.clone()))
        {
            camera.focus(&region);
        }
        selected.set(Some(territory));
        if !sidebar_open.get_untracked() {
            sidebar_open.set(true);
        }
    });
    let unavailable = move || answer.with(|answer| matches!(answer, GuildAnswer::Unavailable));
    // With a count the members' rows are held above the list, so it can show at once.
    let list_placed = move || {
        expected_online().is_some()
            || hold_over.get()
            || answer.with(|answer| !matches!(answer, GuildAnswer::Waiting))
    };
    // Until the details arrive, each figure waits in its place rather than the panel growing.
    let fact =
        move |pick: fn(&GuildFacts) -> String| move || answer.with(|answer| answer.figure(pick));

    view! {
        <div class="sb-panel panel-reveal" style:--tcard-guild=move || guild_color().map(guild_css)>
            <PanelNav close=("Close guild", close) />
            <article class="tcard tcard-sidebar" aria-busy=move || answer.with(|answer| matches!(answer, GuildAnswer::Waiting)).to_string()>
                <header class="tcard-head">
                    <span class="guild-mark" aria-hidden="true" />
                    <h2 class="tcard-name guild-name">
                        {move || {
                            let name = selected_guild.get().unwrap_or_default();
                            let url = crate::guild_stats_url(&name);
                            view! {
                                <a class="tcard-name-link" href=url target="_blank" rel="noopener noreferrer" title="Open Wynncraft guild stats">{name}</a>
                            }
                        }}
                    </h2>
                </header>
                <div class="tcard-owner">
                    // A tag nobody can supply any more is left out rather than shown as waiting.
                    {move || {
                        let prefix = prefix();
                        let unavailable = unavailable();
                        (prefix.is_some() || !unavailable).then(|| view! {
                            <span class="tcard-tag">
                                "[" {prefix.unwrap_or_else(|| "\u{2026}".to_string())} "]"
                                {(!unavailable).then(|| view! { " \u{00b7} Lv. " {fact(|facts| facts.level.to_string())} })}
                            </span>
                        })
                    }}
                    {move || if unavailable() {
                        view! { <span class="tcard-tag" role="status">"Guild details are unavailable"</span> }.into_any()
                    } else {
                        view! {
                            <span class="sb-online"><span aria-hidden="true">{"\u{25CF}"}</span>{fact(|facts| facts.online.to_string())} " online"</span>
                        }
                        .into_any()
                    }}
                </div>
                <dl class="tcard-stats tcard-stats-wide">
                    <div><dt>"Territories"</dt><dd class="sb-gold">{move || guild_territories.with(Vec::len)}</dd></div>
                    <div><dt>"Members"</dt><dd>{fact(|facts| facts.members.to_string())}</dd></div>
                    <div><dt>"Wars"</dt><dd>{fact(|facts| facts.wars.to_string())}</dd></div>
                    <div><dt>"Season rating"</dt><dd class="tcard-sr">{fact(|facts| facts.season_rating.map_or_else(|| "\u{2013}".to_string(), |(season, rating)| format!("S{season}: {}", compact(rating))))}</dd></div>
                    <div class="tcard-stat-span"><dt>"Created"</dt><dd>{move || answer.with(|answer| answer.figure(|facts| since(&facts.created, tick.get())))}</dd></div>
                </dl>
                {move || {
                    // Playercards only cover our own roster, so every other guild keeps
                    // linking out to Wynncraft.
                    let ours = prefix().is_some_and(|prefix| prefix.eq_ignore_ascii_case(crate::SEQUOIA_GUILD_PREFIX));
                    // Members open each other's war records in place; everyone else links out.
                    let in_place = ours && viewer_has_war_access.get();
                    let list = answer.with(|answer| match answer {
                        GuildAnswer::Waiting => {
                            // Without a count, one row says the members are on their way.
                            let held = expected_online().unwrap_or(1);
                            if held == 0 {
                                return None;
                            }
                            Some(view! {
                                <ul class="sb-members" class:mrows=in_place aria-hidden="true">
                                    {(0..held).map(|_| if in_place {
                                        view! { <li class="mrow"><div class="mrow-line"><span class="mrow-rank">{"\u{2026}"}</span></div></li> }.into_any()
                                    } else {
                                        // The name's line sets a row's height, so a held row keeps one.
                                        view! { <li><span class="sb-member-rank">{"\u{2026}"}</span><span class="sb-member-name">{"\u{a0}"}</span></li> }.into_any()
                                    }).collect_view()}
                                </ul>
                            }
                            .into_any())
                        }
                        GuildAnswer::Ready(facts) if facts.online_members.is_empty() => None,
                        GuildAnswer::Ready(facts) if in_place => Some(view! {
                            <ul class="sb-members mrows panel-reveal">
                                {facts.online_members.iter().cloned().map(|member| view! {
                                    <crate::member_war::MemberRow username=member.username rank_label=member.rank_label server=member.server />
                                }).collect_view()}
                            </ul>
                        }
                        .into_any()),
                        GuildAnswer::Ready(facts) => Some(view! {
                            <ul class="sb-members panel-reveal">
                                {facts.online_members.iter().cloned().map(|member| {
                                    let (url, title) = if ours {
                                        (crate::player_card_url(&member.username), "Open Sequoia playercard")
                                    } else {
                                        (format!("https://wynncraft.com/stats/player/{}", member.username), "Open Wynncraft player stats")
                                    };
                                    view! {
                                        <li>
                                            <span class="sb-member-rank">{member.rank_label}</span>
                                            <a class="sb-member-name" href=url target="_blank" rel="noopener noreferrer" title=title>{member.username}</a>
                                            <span class="sb-member-server">{member.server}</span>
                                        </li>
                                    }
                                }).collect_view()}
                            </ul>
                        }
                        .into_any()),
                        GuildAnswer::Unavailable => None,
                    })?;
                    Some(view! {
                        <section class="tcard-section" aria-label="Online members">
                            <h3 class="tcard-section-title">"Online members"</h3>
                            {list}
                        </section>
                    })
                }}
                // A waiting list keeps its room, so neither it nor a phone's sheet moves when it shows.
                <Show when=move || guild_territories.with(|owned| !owned.is_empty())>
                    <section class="tcard-section sb-territories-section" data-held=move || (!list_placed()).to_string() aria-label="Territories">
                        <h3 class="tcard-section-title">"Territories"</h3>
                        <ul class="sb-territories">
                            <For
                                each=move || guild_territories.get()
                                key=|territory| territory.clone()
                                children=move |territory| {
                                    let open = territory.clone();
                                    let title = format!("Open {territory}");
                                    view! {
                                        <li>
                                            <button type="button" class="sb-territory" title=title on:click=move |_| open_territory.run(open.clone())>
                                                <span class="guild-mark" aria-hidden="true" />
                                                <span class="sb-row-name">{territory}</span>
                                            </button>
                                        </li>
                                    }
                                }
                            />
                        </ul>
                    </section>
                </Show>
            </article>
        </div>
    }
}

/// What the territory detail shows that does not change by the second. The view is rebuilt
/// only when this does; the clock-driven figures update in place.
#[derive(Clone, PartialEq)]
struct TerritoryInfo {
    name: String,
    guild_name: String,
    guild_prefix: String,
    acquired: i64,
    color: (u8, u8, u8),
    runtime: Option<TerritoryRuntimeData>,
    resources: Vec<ResourceRow>,
    connections: usize,
    guild_territories: usize,
    takes_in_window: Option<u64>,
}

/// The scalar behind the passive SR figure, for diagnostics.
fn scalar_note(scalar: &EffectiveScalar, now: i64) -> String {
    let estimate = |kind: &str, sample: &sequoia_shared::SeasonScalarSample| {
        format!(
            "{kind} estimate S{}/ weighted {:.2}, raw {:.2}, conf {:.0}% ({})",
            sample.season_id,
            sample.scalar_weighted,
            sample.scalar_raw,
            sample.confidence * 100.0,
            since(&sample.sampled_at, now)
        )
    };
    match (scalar.source, scalar.sample.as_ref()) {
        (ScalarSource::Manual, _) => format!("Manual scalar {:.2} in use", scalar.value),
        (ScalarSource::LiveEstimate, Some(sample)) => estimate("Live", sample),
        (ScalarSource::HistoryEstimate, Some(sample)) => estimate("History", sample),
        (_, None) => format!(
            "Manual fallback scalar {:.2} (estimate unavailable)",
            scalar.value
        ),
    }
}

/// Where a territory's live figures came from, for diagnostics.
fn provenance_note(provenance: &DataProvenance, now: i64) -> (String, String) {
    let source = if provenance.source.trim().is_empty() {
        "unknown"
    } else {
        provenance.source.as_str()
    };
    (
        format!(
            "{} source \u{00b7} {source} \u{00b7} conf {}",
            visibility_label(provenance),
            format_confidence(provenance.confidence)
        ),
        format!(
            "Observed {} \u{00b7} reporters {}",
            since(&provenance.observed_at, now),
            provenance.reporter_count
        ),
    )
}

#[component]
fn DetailPanel() -> impl IntoView {
    let Selected(selected) = expect_context();
    let SelectedGuild(selected_guild) = expect_context();
    let DetailReturnGuild(detail_return_guild) = expect_context();
    let SidebarTransient(sidebar_transient) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let IsMobile(is_mobile) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let settings = expect_context::<ViewerSettings>();
    let heat_mode_enabled = settings.value(settings::HEAT_MAP);
    let HeatEntriesByTerritory(heat_entries_by_territory) = expect_context();
    let show_debug_info = expect_context::<ViewerSettings>().value(settings::DIAGNOSTICS);
    let camera = expect_context::<BrowserMap>().camera();
    let tower_state: crate::tower::TowerState = expect_context();
    let scalar_state = scalar_state();

    // The detail is rebuilt when its territory's data changes; its disclosures keep their
    // state here and close again for the next territory.
    let queue_details_open = RwSignal::new(false);
    Effect::new(move || {
        selected.track();
        queue_details_open.set(false);
    });

    let MapClock(now) = expect_context();

    // Guild-aware connection counts — only recomputes when territories/selection change, not every tick
    let guild_counts = Memo::new(move |_| {
        let name = selected.get()?;
        territories.with(|map| {
            let ct = map.get(&name)?;
            Some(sequoia_shared::tower::count_guild_connections(
                &name,
                ct.territory.connections.as_slice(),
                ct.territory.guild.uuid.as_str(),
                |n| {
                    let ct2 = map.get(n)?;
                    Some((
                        ct2.territory.guild.uuid.as_str(),
                        ct2.territory.connections.as_slice(),
                    ))
                },
            ))
        })
    });

    // Sync tower connection counts when territory changes
    Effect::new(move || {
        if let Some((gc, _, ext)) = guild_counts.get() {
            tower_state.connections.set(gc);
            tower_state.externals.set(ext);
        }
    });

    let info = Memo::new(move |_| {
        let name = selected.get()?;
        let takes_in_window = heat_mode_enabled
            .get()
            .then(|| heat_entries_by_territory.with(|heat| heat.get(&name).copied().unwrap_or(0)));
        territories.with(|map| {
            let ct = map.get(&name)?;
            let guild = &ct.territory.guild;
            let guild_territories = map
                .values()
                .filter(|candidate| {
                    if guild.uuid.is_empty() {
                        candidate.territory.guild.name == guild.name
                    } else {
                        candidate.territory.guild.uuid == guild.uuid
                    }
                })
                .count();
            Some(TerritoryInfo {
                name: name.clone(),
                guild_name: guild.name.clone(),
                guild_prefix: guild.prefix.clone(),
                acquired: ct.territory.acquired.timestamp(),
                color: ct.guild_color,
                runtime: ct.territory.runtime.clone(),
                resources: territory_resource_rows(
                    &ct.territory.resources,
                    ct.territory.runtime.as_ref(),
                ),
                connections: ct.territory.connections.len(),
                guild_territories,
                takes_in_window,
            })
        })
    });
    let region = Memo::new(move |_| {
        let name = selected.get()?;
        territories.with(|map| map.get(&name).map(|ct| ct.territory.location.clone()))
    });

    let back = Callback::new(move |()| {
        let owner = info.with_untracked(|info| info.as_ref().map(|info| info.guild_name.clone()));
        selected.set(None);
        if let Some(guild_name) = owner {
            selected_guild.set(Some(guild_name));
        }
        detail_return_guild.set(None);
        sidebar_transient.set(false);
    });
    let close = Callback::new(move |()| {
        // On mobile, closing the sheet hands the territory back to its peek card.
        if is_mobile.get_untracked() && detail_return_guild.get_untracked().is_none() {
            sidebar_transient.set(false);
            sidebar_open.set(false);
            return;
        }
        selected.set(None);
        if let Some(return_guild) = detail_return_guild.get_untracked() {
            selected_guild.set(Some(return_guild));
        }
        detail_return_guild.set(None);
        sidebar_transient.set(false);
    });
    // The map behind the sidebar moves to the territory; a phone's sheet steps aside for it.
    let show_on_map = move |_| {
        if let Some(region) = region.get_untracked() {
            camera.focus(&region);
        }
        if is_mobile.get_untracked() {
            sidebar_transient.set(false);
            sidebar_open.set(false);
        }
    };

    view! {
        <div class="sb-panel panel-reveal tcard-detail" style:--tcard-guild=move || info.with(|info| info.as_ref().map(|info| guild_css(info.color)))>
            <PanelNav back=("Back to guild", back) close=("Close details", close)>
                <button type="button" class="ui-button ui-icon-button sb-show-on-map" aria-label="Show on map" title="Show on map" on:click=show_on_map>
                    <MapPinIcon />
                </button>
            </PanelNav>
            {move || info.get().map(|info| {
                let runtime = info.runtime.clone();
                let acquired = info.acquired;
                let held = move || now.get() - acquired;
                let acquired_title = chrono::DateTime::from_timestamp(acquired, 0).map(|time| time.to_rfc3339()).unwrap_or_default();
                let treasury_runtime = runtime.clone();
                let provenance = runtime.as_ref().and_then(|runtime| runtime.provenance.clone());
                let guild_territories = info.guild_territories;
                let connections = info.connections;
                view! {
                    <article class="tcard tcard-sidebar">
                        <TerritoryHead name=info.name.clone() hq=runtime.as_ref().and_then(|runtime| runtime.headquarters) == Some(true) heading=true />
                        <div class="tcard-owner">
                            <a class="tcard-guild" href=crate::guild_stats_url(&info.guild_name) target="_blank" rel="noopener noreferrer" title="Open Wynncraft guild stats">
                                {info.guild_name.clone()}
                            </a>
                            <span class="tcard-tag">"[" {info.guild_prefix.clone()} "]"</span>
                        </div>

                        <TerritoryWar territory=Signal::stored(info.name.clone()) details_open=queue_details_open />

                        {move || cooldown(held()).map(|(left, share)| view! {
                            <div class="tcard-cooldown-row">
                                <span>"Cooldown"</span>
                                <span class="tcard-cooldown">{left}</span>
                                <span class="tcard-meter" aria-hidden="true">
                                    <span style:width=format!("{:.1}%", share * 100.0) />
                                </span>
                            </div>
                        })}

                        <dl class="tcard-stats tcard-stats-wide">
                            <div><dt>"Acquired"</dt><dd title=acquired_title>{move || crate::format::ago(held())}</dd></div>
                            <div><dt>"Treasury"</dt>{move || view! { <TreasuryValue level=treasury(treasury_runtime.as_ref(), held()) buff=true /> }}</div>
                            {runtime.as_ref().and_then(|runtime| runtime.defense_tier.clone()).map(|tier| view! {
                                <div><dt>"Defense"</dt><DefenseValue tier /></div>
                            })}
                            <div>
                                <dt>"Connections"</dt>
                                <dd>{move || guild_counts.get().map_or_else(|| connections.to_string(), |(owned, total, _)| format!("{owned}/{total}"))}</dd>
                            </div>
                            <div class="tcard-stat-span">
                                <dt>"Passive SR"</dt>
                                <dd class="tcard-sr">{move || {
                                    let scalar = scalar_state.get().value;
                                    format!(
                                        "{}/h \u{00b7} {}/5s",
                                        rate(passive_sr_per_hour(guild_territories, scalar)),
                                        rate(passive_sr_per_5s(guild_territories, scalar))
                                    )
                                }}</dd>
                            </div>
                            {info.takes_in_window.map(|count| view! {
                                <div><dt>"Takes in window"</dt><dd>{count}</dd></div>
                            })}
                        </dl>
                        {move || show_debug_info.get().then(|| {
                            let note = scalar_note(&scalar_state.get(), now.get());
                            let source = provenance.as_ref().map(|provenance| provenance_note(provenance, now.get()));
                            view! {
                                <p class="tcard-debug">{note}</p>
                                {source.map(|(primary, secondary)| view! { <p class="tcard-debug">{primary}<br />{secondary}</p> })}
                            }
                        })}

                        <section class="tcard-section" aria-label="Resources">
                            <h3 class="tcard-section-title">"Resources"</h3>
                            <ResourceTable rows=info.resources.clone() icon_px=14 />
                        </section>
                    </article>
                }
            })}
            // Keyed by territory alone, so the calculator, the disclosures and any loaded
            // activity survive the detail above being rebuilt.
            {move || selected.get().map(|name| view! { <TerritoryMore territory=name /> })}
        </div>
    }
}

/// Below the territory's own figures: the tower calculator, then where it is and what has
/// happened there, each closed until asked for.
#[component]
fn TerritoryMore(territory: String) -> impl IntoView {
    let room = expect_context::<crate::war_room::WarRoom>();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    // The disclosure's state lives here for the territory's whole stay; its facts follow the
    // territory's geometry, which a live correction can change under the same name.
    let location_open = RwSignal::new(false);
    let location = Memo::new({
        let territory = territory.clone();
        move |_| territories.with(|map| map.get(&territory).map(|ct| ct.territory.location.clone()))
    });
    let region = move || {
        location.with(|location| {
            location.as_ref().map_or_else(String::new, |location| {
                format!(
                    "({}, {}) \u{2192} ({}, {})",
                    location.start[0], location.start[1], location.end[0], location.end[1]
                )
            })
        })
    };
    let size = move || {
        location.with(|location| {
            location.as_ref().map_or_else(String::new, |location| {
                format!("{}\u{00d7}{}", location.width(), location.height())
            })
        })
    };
    let territory = Signal::stored(territory);
    view! {
        <div class="tcard tcard-sidebar tcard-sidebar-tail">
            <TowerCalculator />
            <div class="tcard-more">
                <Show when=move || location.with(Option::is_some)>
                    <Disclosure open=location_open summary="Location">
                        <dl class="tcard-facts">
                            <div><dt>"Region"</dt><dd>{region}</dd></div>
                            <div><dt>"Size"</dt><dd>{size}</dd></div>
                        </dl>
                    </Disclosure>
                </Show>
                <Show when=move || room.board.with(Option::is_some)>
                    <ActivityDisclosure territory />
                </Show>
            </div>
        </div>
    }
}

#[component]
fn SidebarToolbar() -> impl IntoView {
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let connection: RwSignal<ConnectionStatus> = expect_context();
    let ShowSettings(show_settings) = expect_context();
    let CurrentMode(mode) = expect_context();
    let PlaybackActive(playback_active) = expect_context();
    let HistoryAvailable(history_available) = expect_context();
    let HistoryTimestamp(history_timestamp) = expect_context();
    let HistoryMapTime(history_map_time) = expect_context();
    let HistoryBoundsSignal(history_bounds) = expect_context();
    let HistoryFetchNonce(history_fetch_nonce) = expect_context();
    let HistoryLegacyGeometryActive(history_legacy_geometry_active) = expect_context();
    let LastLiveSeq(last_live_seq) = expect_context();
    let HistoryBufferedUpdates(history_buffered_updates) = expect_context();
    let HistoryBufferModeActive(history_buffer_mode_active) = expect_context();
    let HistorySeasonScalarSample(history_scalar_sample) = expect_context();
    let HistorySeasonLeaderboard(history_sr_leaderboard) = expect_context();
    let NeedsLiveResync(needs_live_resync) = expect_context();
    let LiveHandoffResyncCount(live_handoff_resync_count) = expect_context();
    let TerritoryGeometryStore(geo_store) = expect_context();
    let GuildColorStore(guild_color_store) = expect_context();
    let IsMobile(is_mobile) = expect_context();
    let settings = expect_context::<ViewerSettings>();
    let heat_mode_enabled = settings.value(settings::HEAT_MAP);

    // A healthy feed shows nothing; only connecting and reconnecting get a lamp.
    let status_text = Memo::new(move |_| match connection.get() {
        ConnectionStatus::Live => None,
        ConnectionStatus::Connecting => Some("Connecting..."),
        ConnectionStatus::Reconnecting => Some("Reconnecting..."),
    });

    let is_history = Memo::new(move |_| mode.get() == MapMode::History);
    let toggle_history = move |_| {
        if is_history.get_untracked() {
            history::exit_history_mode(history::ExitHistoryModeInput {
                mode,
                playback_active,
                history_fetch_nonce,
                history_timestamp,
                history_legacy_geometry_active,
                history_buffered_updates,
                history_buffer_mode_active,
                last_live_seq,
                needs_live_resync,
                live_handoff_resync_count,
                history_sr_leaderboard,
                territories,
            });
        } else {
            history::enter_history_mode(history::EnterHistoryModeInput {
                history_map_time,
                mode,
                history_timestamp,
                history_bounds,
                history_fetch_nonce,
                history_legacy_geometry_active,
                history_buffered_updates,
                history_buffer_mode_active,
                needs_live_resync,
                history_scalar_sample,
                history_sr_leaderboard,
                geo_store,
                guild_color_store,
                territories,
            });
        }
    };

    view! {
        <div class="sb-toolbar sb-bar">
            <KeybindHelp />
            // Phones switch these from the map's own buttons and Settings.
            <Show when=move || !is_mobile.get()>
                <div class="sb-modes" role="group" aria-label="Map modes">
                    <Show when=move || history_available.get()>
                        <button
                            type="button"
                            class="ui-button sb-mode"
                            data-mode="history"
                            aria-pressed=move || is_history.get().to_string()
                            title=move || if is_history.get() { "Disable history mode (h)" } else { "Enable history mode (h)" }
                            on:click=toggle_history
                        >"History"</button>
                    </Show>
                    <button
                        type="button"
                        class="ui-button sb-mode"
                        data-mode="heat"
                        aria-pressed=move || heat_mode_enabled.get().to_string()
                        title=move || if heat_mode_enabled.get() { "Disable heat map" } else { "Enable heat map" }
                        on:click=move |_| settings.toggle(settings::HEAT_MAP)
                    >"Heat"</button>
                </div>
            </Show>
            <div class="sb-toolbar-end">
                {move || status_text.get().map(|text| view! {
                    <span class="sb-toolbar-status" role="status" title=text aria-label=text>
                        <span aria-hidden="true" />
                    </span>
                })}
                <button
                    type="button"
                    class="ui-button ui-icon-button sb-settings"
                    aria-label="Settings"
                    title="Settings"
                    aria-expanded=move || show_settings.get().to_string()
                    on:click=move |_| show_settings.update(|open| *open = !*open)
                >
                    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" width="16" height="16" aria-hidden="true">
                        <path fill-rule="evenodd" d="M7.84 1.804A1 1 0 018.82 1h2.36a1 1 0 01.98.804l.331 1.652a6.993 6.993 0 011.929 1.115l1.598-.54a1 1 0 011.186.447l1.18 2.044a1 1 0 01-.205 1.251l-1.267 1.113a7.047 7.047 0 010 2.228l1.267 1.113a1 1 0 01.206 1.25l-1.18 2.045a1 1 0 01-1.187.447l-1.598-.54a6.993 6.993 0 01-1.929 1.115l-.33 1.652a1 1 0 01-.98.804H8.82a1 1 0 01-.98-.804l-.331-1.652a6.993 6.993 0 01-1.929-1.115l-1.598.54a1 1 0 01-1.186-.447l-1.18-2.044a1 1 0 01.205-1.251l1.267-1.114a7.05 7.05 0 010-2.227L1.821 7.773a1 1 0 01-.206-1.25l1.18-2.045a1 1 0 011.187-.447l1.598.54A6.993 6.993 0 017.51 3.456l.33-1.652zM10 13a3 3 0 100-6 3 3 0 000 6z" clip-rule="evenodd" />
                    </svg>
                </button>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::{GuildFacts, extract_online_members};
    use serde_json::json;

    #[test]
    fn extract_online_members_includes_only_online_members() {
        let payload = json!({
            "members": {
                "owner": {
                    "OwnerOne": { "online": true, "server": "NA1" },
                    "OwnerOffline": { "online": false, "server": "NA2" }
                },
                "chief": {
                    "ChiefOne": { "online": true, "server": "EU1" }
                },
                "recruit": {
                    "RecruitOne": { "online": true, "server": null }
                }
            }
        });

        let rows = extract_online_members(&payload);
        let usernames: Vec<&str> = rows.iter().map(|row| row.username.as_str()).collect();

        assert_eq!(usernames, vec!["OwnerOne", "ChiefOne", "RecruitOne"]);
        assert_eq!(rows[2].server, "");
    }

    #[test]
    fn extract_online_members_orders_by_rank_priority() {
        let payload = json!({
            "members": {
                "recruit": { "RecruitOne": { "online": true, "server": "AS1" } },
                "captain": { "CaptainOne": { "online": true, "server": "EU2" } },
                "owner": { "OwnerOne": { "online": true, "server": "NA1" } },
                "chief": { "ChiefOne": { "online": true, "server": "NA2" } },
                "recruiter": { "RecruiterOne": { "online": true, "server": "AS2" } },
                "strategist": { "StrategistOne": { "online": true, "server": "EU1" } }
            }
        });

        let rows = extract_online_members(&payload);
        let rank_labels: Vec<&str> = rows.iter().map(|row| row.rank_label.as_str()).collect();

        assert_eq!(
            rank_labels,
            vec![
                "Owner",
                "Chief",
                "Strategist",
                "Captain",
                "Recruiter",
                "Recruit"
            ]
        );
    }

    #[test]
    fn extract_online_members_orders_usernames_case_insensitively_within_rank() {
        let payload = json!({
            "members": {
                "chief": {
                    "beta": { "online": true, "server": "NA1" },
                    "Alpha": { "online": true, "server": "NA2" },
                    "alpha2": { "online": true, "server": "NA3" }
                }
            }
        });

        let rows = extract_online_members(&payload);
        let usernames: Vec<&str> = rows.iter().map(|row| row.username.as_str()).collect();

        assert_eq!(usernames, vec!["Alpha", "alpha2", "beta"]);
    }

    #[test]
    fn guild_facts_take_the_latest_season_and_the_member_total() {
        let facts = GuildFacts::from_json(&json!({
            "prefix": "SEQ",
            "level": 268,
            "online": 16,
            "members": { "total": 146, "chief": { "GaztheCat": { "online": true, "server": "NA24" } } },
            "wars": 173_747,
            "seasonRanks": { "9": { "rating": 1_000 }, "33": { "rating": 2_900_000 } },
            "created": "2022-10-14T00:00:00Z"
        }));
        assert_eq!(
            (facts.level, facts.online, facts.members, facts.wars),
            (268, 16, 146, 173_747)
        );
        assert_eq!(facts.season_rating, Some((33, 2_900_000)));
        assert_eq!(facts.online_members.len(), 1);
        assert_eq!(GuildFacts::from_json(&json!({})).season_rating, None);
    }
}
