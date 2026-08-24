//! Per-war detail cards along the bottom of the map: one box per war in progress, showing the
//! tower's remaining EHP and outgoing DPS, who is fighting it on which class, and when it is
//! expected to fall.
//!
//! The companion to [`crate::warcontroller::WarQueuePanel`], which lists the same feed's
//! *queue* from the top left. This module reads the halves that panel ignores -
//! `WarControllerState::wars` and `::players` - and is gated the same two ways: Sequoia members
//! only, live mode only.

use std::cmp::Ordering;
use std::collections::HashMap;

use leptos::prelude::*;
use sequoia_shared::tower::format_stat;
use sequoia_shared::{
    ActiveWar, PlayerClass, QueueStatus, WarControllerState, WarPlayer, WarQueueEntry,
};
use crate::app::{
    CurrentMode, IsMobile, MapMode, ShowMinimap, SidebarOpen, SidebarWidth, WarControllerData,
    WarFeedVisible, WindowWidth,
};
use crate::icons::class_icon_url;
use crate::warcontroller::{difficulty_color, format_eta};

/// Members listed per card before the roster is summarized as "+N more".
const MAX_MEMBERS: usize = 5;

/// Cards are a fixed width so the strip's capacity is arithmetic rather than a DOM measurement.
const BOX_WIDTH: f64 = 208.0;
const BOX_GAP: f64 = 8.0;
/// Width reserved for the overflow chip once anything has to hide.
const CHIP_WIDTH: f64 = 44.0;

/// Minimap frame geometry, mirrored from the backdrop in [`crate::app`] so the strip starts
/// clear of it. `canvas.rs` draws the minimap itself at the same rect.
const MINIMAP_LEFT: f64 = 16.0;
const MINIMAP_WIDTH: f64 = 200.0;
const STRIP_GUTTER: f64 = 12.0;
/// Clears the sidebar's collapse toggle when the sidebar itself is shut: 16px margin, the
/// 32px button, and 16px of air. The same number `DefenseLegend` uses.
const SIDEBAR_CLOSED_INSET: f64 = 64.0;

/// Bottom margins, matching the minimap's: the history timeline occupies the lower 68px.
const STRIP_BOTTOM: f64 = 16.0;
const STRIP_BOTTOM_HISTORY: f64 = 68.0;

/// Health bar colours: the fuller the tower, the greener the bar. Only [`health_color`] uses
/// these - the card's dot is the territory's difficulty, via [`difficulty_color`].
const COLOR_AT_WAR: &str = "#ff4545";
const COLOR_ENTERED: &str = "#f5c542";
const COLOR_HEALTHY: &str = "#50c878";

/// One member line. The class is kept parsed rather than as a URL because resolving the icon
/// goes through `window`, which is absent when these functions are unit-tested natively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WarBoxMember {
    pub username: String,
    pub class: Option<PlayerClass>,
}

/// One card's worth of state, fully derived so the view stays declarative.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WarBox {
    pub territory: String,
    /// Raw Wynncraft difficulty tier, e.g. `VERY_HIGH`; the card's dot colour. Kept unparsed
    /// so an unrecognised tier still round-trips instead of collapsing into a default.
    pub difficulty: String,
    /// Seconds remaining as of the feed snapshot, via [`WarQueueEntry::eta_secs`].
    pub eta: Option<i64>,
    /// Already truncated to [`MAX_MEMBERS`].
    pub members: Vec<WarBoxMember>,
    /// Roster size beyond the cap; zero when everyone fits.
    pub extra_members: usize,
    /// `None` for an `ENTERED` queue entry with no war row yet: no tower stats, no bar.
    pub health: Option<f32>,
    /// The tower's *total* effective HP. Remaining is [`Self::remaining_ehp`].
    pub ehp: Option<i64>,
    /// The tower's *outgoing* DPS - damage it deals, not damage it takes. Never a
    /// time-to-kill input.
    pub dps: Option<i64>,
}

impl WarBox {
    /// Effective HP still standing: the tower's total scaled by the remaining fraction.
    pub fn remaining_ehp(&self) -> Option<f64> {
        Some(self.ehp? as f64 * self.health?.clamp(0.0, 1.0) as f64)
    }

    /// An `ENTERED` territory whose war has not started, so it has no tower stats yet.
    ///
    /// The card view stopped branching on this when the dot became difficulty-coloured; it
    /// survives as the named form of the invariant the box builders' tests assert.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_awaiting_start(&self) -> bool {
        self.health.is_none()
    }
}

/// Builds one card per war in progress, plus one per `ENTERED` territory still waiting to
/// start, ordered by how dangerous the tower is.
pub(crate) fn build_war_boxes(state: &WarControllerState) -> Vec<WarBox> {
    let queues = index_queues(&state.queues);
    let members = index_members(&state.players);
    let at_war = state.territories_at_war();

    let mut boxes: Vec<WarBox> = state
        .wars
        .iter()
        .map(|war| war_box(war, &queues, &members, state.timestamp))
        .chain(
            state
                .queues
                .iter()
                .filter(|entry| {
                    QueueStatus::from_api_status(&entry.status) == Some(QueueStatus::Entered)
                        && !at_war.contains(&entry.territory)
                })
                .map(|entry| entered_box(entry, &members)),
        )
        .collect();

    boxes.sort_by(compare_boxes);
    boxes
}

/// The most recent queue entry per territory. The feed should not repeat a territory, but a
/// stale row must not shadow the live one if it ever does.
fn index_queues(queues: &[WarQueueEntry]) -> HashMap<&str, &WarQueueEntry> {
    let mut index: HashMap<&str, &WarQueueEntry> = HashMap::new();
    for entry in queues {
        index
            .entry(entry.territory.as_str())
            .and_modify(|existing| {
                if entry.timestamp > existing.timestamp {
                    *existing = entry;
                }
            })
            .or_insert(entry);
    }
    index
}

/// Players bucketed by the territory they are standing in, each bucket sorted by name.
///
/// Sorted rather than left in feed order on purpose: a card shows only the first
/// [`MAX_MEMBERS`], so an unstable order would reshuffle which five are visible on every poll.
/// Roaming players carry no territory and belong to no war.
fn index_members(players: &[WarPlayer]) -> HashMap<&str, Vec<WarBoxMember>> {
    let mut index: HashMap<&str, Vec<WarBoxMember>> = HashMap::new();
    for player in players {
        let Some(territory) = player.territory.as_deref() else {
            continue;
        };
        index
            .entry(territory)
            .or_default()
            .push(WarBoxMember {
                username: player.username.clone(),
                class: PlayerClass::from_api_class(&player.class),
            });
    }
    for bucket in index.values_mut() {
        bucket.sort_by_key(|member| member.username.to_lowercase());
    }
    index
}

fn take_members(
    members: &HashMap<&str, Vec<WarBoxMember>>,
    territory: &str,
) -> (Vec<WarBoxMember>, usize) {
    let Some(roster) = members.get(territory) else {
        return (Vec::new(), 0);
    };
    let extra = roster.len().saturating_sub(MAX_MEMBERS);
    (roster.iter().take(MAX_MEMBERS).cloned().collect(), extra)
}

fn war_box(
    war: &ActiveWar,
    queues: &HashMap<&str, &WarQueueEntry>,
    members: &HashMap<&str, Vec<WarBoxMember>>,
    feed_timestamp: i64,
) -> WarBox {
    let (roster, extra_members) = take_members(members, &war.territory);
    WarBox {
        // The war carries no ETA of its own; it rides on the matching queue entry.
        eta: queues
            .get(war.territory.as_str())
            .and_then(|entry| entry.eta_secs(feed_timestamp)),
        territory: war.territory.clone(),
        difficulty: war.difficulty.clone(),
        members: roster,
        extra_members,
        health: Some(war.health),
        ehp: war.ehp,
        dps: war.dps,
    }
}

fn entered_box(entry: &WarQueueEntry, members: &HashMap<&str, Vec<WarBoxMember>>) -> WarBox {
    let (roster, extra_members) = take_members(members, &entry.territory);
    WarBox {
        territory: entry.territory.clone(),
        difficulty: entry.difficulty.clone(),
        // Nothing in the feed predicts when a war that has not started will be won.
        eta: None,
        members: roster,
        extra_members,
        health: None,
        ehp: None,
        dps: None,
    }
}

/// Most dangerous tower first.
///
/// `dps` is the tower's *outgoing* damage, so this ranks wars by how much they hurt to stand
/// in. Wars the backend has not measured sink below even a measured zero. The territory
/// tie-break makes the order total, so two identical wars cannot swap places between polls.
fn compare_boxes(a: &WarBox, b: &WarBox) -> Ordering {
    b.dps
        .unwrap_or(i64::MIN)
        .cmp(&a.dps.unwrap_or(i64::MIN))
        .then_with(|| {
            b.remaining_ehp()
                .unwrap_or(f64::MIN)
                .partial_cmp(&a.remaining_ehp().unwrap_or(f64::MIN))
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| a.territory.cmp(&b.territory))
}

/// Where the strip starts: clear of the minimap when it is shown, at the margin when it is not.
fn strip_left_px(show_minimap: bool) -> f64 {
    if show_minimap {
        MINIMAP_LEFT + MINIMAP_WIDTH + STRIP_GUTTER
    } else {
        MINIMAP_LEFT
    }
}

/// Lifts over the history timeline, matching the minimap's own bottom margin.
fn strip_bottom_px(history: bool) -> f64 {
    if history {
        STRIP_BOTTOM_HISTORY
    } else {
        STRIP_BOTTOM
    }
}

/// Where the strip stops: clear of the sidebar, or of its collapse toggle when it is shut.
fn strip_right_px(sidebar_open: bool, sidebar_width: f64) -> f64 {
    if sidebar_open {
        sidebar_width + 16.0
    } else {
        SIDEBAR_CLOSED_INSET
    }
}

fn strip_available_px(window_width: f64, left: f64, right: f64) -> f64 {
    (window_width - left - right).max(0.0)
}

/// How many whole cards fit in `available`, ignoring the chip.
fn fits(available: f64) -> usize {
    if available < BOX_WIDTH {
        0
    } else {
        ((available + BOX_GAP) / (BOX_WIDTH + BOX_GAP)).floor() as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StripLayout {
    pub visible: usize,
    pub hidden: usize,
    /// Width to clip the scroller to. Always a whole number of cards.
    pub scroller_width: f64,
}

/// Splits `total` cards into what is shown and what the chip accounts for.
///
/// The chip's own width is subtracted only once something is known to overflow, so the answer
/// cannot oscillate between "the chip fits" and "the chip does not fit". At least one card is
/// always shown, even in a strip too narrow for it - a clipped card reads better than an empty
/// strip with a chip floating in it.
fn strip_layout(available: f64, total: usize) -> StripLayout {
    if total == 0 {
        return StripLayout {
            visible: 0,
            hidden: 0,
            scroller_width: 0.0,
        };
    }
    let uncrowded = fits(available);
    let (visible, hidden) = if total <= uncrowded {
        (total, 0)
    } else {
        let crowded = fits(available - CHIP_WIDTH - BOX_GAP).max(1);
        (crowded.min(total), total.saturating_sub(crowded))
    };
    StripLayout {
        visible,
        hidden,
        // A whole number of cards, so the strip clips on a card boundary rather than
        // leaving half of one peeking at the edge.
        scroller_width: visible as f64 * (BOX_WIDTH + BOX_GAP) - BOX_GAP,
    }
}

/// Health bar colour, stepping through the same palette the queue panel uses for its stages.
fn health_color(health: f32) -> &'static str {
    match health {
        value if value > 0.5 => COLOR_HEALTHY,
        value if value > 0.2 => COLOR_ENTERED,
        _ => COLOR_AT_WAR,
    }
}

/// `format_stat` over an optional figure, with the panel's em-dash placeholder for absent ones.
fn format_figure(value: Option<f64>) -> String {
    value.map(format_stat).unwrap_or_else(|| "\u{2014}".into())
}

/// The bottom strip of per-war cards.
///
/// Gated exactly as [`crate::warcontroller::WarQueuePanel`] is: the feed is guild-internal, and
/// it describes right now, so it must never be shown to an outsider or against a past snapshot.
/// Desktop only - there is no room for a horizontal card strip on a phone.
#[component]
pub(crate) fn WarStatsStrip() -> impl IntoView {
    let WarControllerData(warcontroller_state) = expect_context();
    let WarFeedVisible(war_feed_visible) = expect_context();
    let CurrentMode(map_mode) = expect_context();
    let IsMobile(is_mobile) = expect_context();
    let ShowMinimap(show_minimap) = expect_context();
    let SidebarOpen(sidebar_open) = expect_context();
    let SidebarWidth(sidebar_width) = expect_context();
    let WindowWidth(window_width) = expect_context();

    let visible = Memo::new(move |_| {
        war_feed_visible.get() && map_mode.get() == MapMode::Live && !is_mobile.get()
    });

    // `PartialEq` on `WarBox` is load-bearing here: the feed re-broadcasts every few seconds,
    // and without the memo's equality check every one of those would rebuild the whole strip.
    let boxes: Memo<Vec<WarBox>> = Memo::new(move |_| {
        if !visible.get() {
            return Vec::new();
        }
        warcontroller_state.with(|state| {
            state
                .as_ref()
                .map(build_war_boxes)
                .unwrap_or_default()
        })
    });
    // ETAs are seconds remaining as of the moment the backend built the payload, so the
    // countdown is measured from that instant rather than from when a card was rendered.
    let feed_timestamp = Memo::new(move |_| {
        warcontroller_state.with(|state| state.as_ref().map(|state| state.timestamp).unwrap_or(0))
    });

    let left = Memo::new(move |_| strip_left_px(show_minimap.get()));
    let right = Memo::new(move |_| strip_right_px(sidebar_open.get(), sidebar_width.get()));
    let layout = Memo::new(move |_| {
        strip_layout(
            strip_available_px(window_width.get(), left.get(), right.get()),
            boxes.with(Vec::len),
        )
    });

    // Its own memo rather than an inline comparison: the view macro reads a bare `>` in a
    // `when` closure as the tag close.
    let has_hidden = Memo::new(move |_| layout.get().hidden > 0);

    let scroller_ref = NodeRef::<leptos::html::Div>::new();

    view! {
        <Show when=move || visible.get() && !boxes.with(Vec::is_empty)>
            <div
                class="war-stats-strip"
                style:left=move || format!("{:.0}px", left.get())
                style:right=move || format!("{:.0}px", right.get())
                style:bottom=move || {
                    format!("{:.0}px", strip_bottom_px(map_mode.get() == MapMode::History))
                }
            >
                <div
                    class="war-stats-scroller scrollbar-thin-x"
                    node_ref=scroller_ref
                    style:width=move || format!("{:.0}px", layout.get().scroller_width)
                >
                    // Every card is rendered, not just the visible run: the hidden ones have
                    // to exist for the chip to be able to scroll to them.
                    {move || {
                        boxes
                            .get()
                            .into_iter()
                            .map(|war| view! { <WarStatsCard war feed_timestamp /> })
                            .collect_view()
                    }}
                </div>
                <Show when=move || has_hidden.get()>
                    <button
                        class="war-stats-chip"
                        title=move || {
                            format!("{} more wars \u{2014} click to scroll", layout.get().hidden)
                        }
                        on:click=move |_| {
                            let Some(element) = scroller_ref.get() else { return };
                            let page = element.client_width();
                            let max = (element.scroll_width() - page).max(0);
                            // Wrapping makes the chip a cycle rather than a dead end once the
                            // last card has been reached.
                            let next = if element.scroll_left() >= max {
                                0
                            } else {
                                (element.scroll_left() + page).min(max)
                            };
                            element.set_scroll_left(next);
                        }
                    >
                        {move || format!("+{}", layout.get().hidden)}
                    </button>
                </Show>
            </div>
        </Show>
    }
}

#[component]
fn WarStatsCard(war: WarBox, feed_timestamp: Memo<i64>) -> impl IntoView {
    // The shared 1-second clock, so the ETA keeps ticking down between feed updates. Read
    // only here, never in the box memo, or the whole strip would rebuild once a second.
    let tick: RwSignal<i64> = expect_context();

    // The dot is the territory's difficulty, not its stage: a card only ever exists for a
    // war that has started or is about to, so stage carried almost no information here.
    let dot_color = difficulty_color(&war.difficulty);
    let territory = war.territory.clone();
    // Long names are ellipsized in the header, so the full one lives in the tooltip; the view
    // macro consumes the body first, hence the clone up here.
    let full_name = war.territory.clone();
    let eta = war.eta;
    let health = war.health;
    let ehp_label = format_figure(war.remaining_ehp());
    let dps_label = format_figure(war.dps.map(|dps| dps as f64));
    let members = war.members;
    let extra_members = war.extra_members;

    view! {
        <div class="war-stats-card">
            <div style="display: flex; align-items: center; gap: 6px; margin-bottom: 6px;">
                <span style=format!(
                    "flex-shrink: 0; width: 6px; height: 6px; border-radius: 50%; background: {dot_color};",
                ) />
                <span
                    title=full_name
                    style="flex: 1; min-width: 0; font-family: var(--font-display); font-size: 0.74rem; color: #d8d5cb; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;"
                >
                    {territory}
                </span>
                <span style="flex-shrink: 0; font-family: var(--font-mono); font-size: 0.72rem; color: #6f748f; font-variant-numeric: tabular-nums;">
                    {move || format_eta(eta, feed_timestamp.get(), tick.get())}
                </span>
            </div>

            {members
                .into_iter()
                .map(|member| view! { <WarStatsMember member /> })
                .collect_view()}
            {(extra_members > 0)
                .then(|| {
                    view! {
                        <div style="font-family: var(--font-mono); font-size: 0.68rem; color: #6f748f; padding-left: 20px;">
                            {format!("+{extra_members} more")}
                        </div>
                    }
                })}

            {match health {
                // A war with null ehp/dps still has a health fraction to draw, so the footer
                // is gated on health rather than on the figures being present.
                Some(health) => {
                    view! {
                        <div style="border-top: 1px solid rgba(40,44,62,0.65); padding-top: 7px; margin-top: 7px;">
                            <div style="display: flex; align-items: baseline; justify-content: space-between; gap: 8px; margin-bottom: 5px; font-family: var(--font-mono); font-size: 0.7rem; color: #d8d5cb; font-variant-numeric: tabular-nums;">
                                <span>
                                    <span style="color: #6f748f; font-size: 0.62rem; letter-spacing: 0.06em;">
                                        "EHP LEFT "
                                    </span>
                                    {ehp_label}
                                </span>
                                <span>
                                    <span style="color: #6f748f; font-size: 0.62rem; letter-spacing: 0.06em;">
                                        "TOWER DPS "
                                    </span>
                                    {dps_label}
                                </span>
                            </div>
                            <div class="war-stats-bar">
                                <div
                                    class="war-stats-bar-fill"
                                    style:width=format!("{:.1}%", health.clamp(0.0, 1.0) * 100.0)
                                    style:background=health_color(health)
                                />
                            </div>
                        </div>
                    }
                        .into_any()
                }
                None => {
                    view! {
                        <div style="font-family: var(--font-mono); font-size: 0.68rem; color: #6f748f; margin-top: 6px;">
                            "Awaiting start"
                        </div>
                    }
                        .into_any()
                }
            }}
        </div>
    }
}

#[component]
fn WarStatsMember(member: WarBoxMember) -> impl IntoView {
    // Resolved here rather than in `build_war_boxes`: this reads `window`, which is absent
    // when the builder is unit-tested on the native target.
    let icon = member
        .class
        .and_then(|class| class_icon_url(class.label()).map(|url| (url, class.label())));

    view! {
        <div style="display: flex; align-items: center; gap: 6px; padding: 1px 0;">
            {match icon {
                Some((url, label)) => {
                    view! {
                        <img
                            src=url
                            alt=label
                            width="14"
                            height="14"
                            style="flex-shrink: 0; image-rendering: pixelated;"
                        />
                    }
                        .into_any()
                }
                // A spacer, so an unknown class does not knock the names out of alignment.
                None => view! { <span style="flex-shrink: 0; width: 14px;" /> }.into_any(),
            }}
            <span style="flex: 1; min-width: 0; font-family: var(--font-mono); font-size: 0.74rem; color: #d8d5cb; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;">
                {member.username}
            </span>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn war(territory: &str, health: f32, ehp: Option<i64>, dps: Option<i64>) -> ActiveWar {
        ActiveWar {
            territory: territory.to_string(),
            difficulty: "VERY_HIGH".to_string(),
            health,
            start: 1_000,
            ehp,
            dps,
        }
    }

    fn queue(territory: &str, status: &str, timestamp: i64) -> WarQueueEntry {
        WarQueueEntry {
            territory: territory.to_string(),
            difficulty: "VERY_HIGH".to_string(),
            status: status.to_string(),
            timestamp,
            eta: None,
        }
    }

    fn player(username: &str, class: &str, territory: Option<&str>) -> WarPlayer {
        WarPlayer {
            username: username.to_string(),
            class: class.to_string(),
            territory: territory.map(str::to_string),
            pos: None,
        }
    }

    fn state(
        queues: Vec<WarQueueEntry>,
        wars: Vec<ActiveWar>,
        players: Vec<WarPlayer>,
    ) -> WarControllerState {
        WarControllerState {
            timestamp: 1_000,
            queues,
            wars,
            players,
        }
    }

    fn names(boxes: &[WarBox]) -> Vec<&str> {
        boxes.iter().map(|b| b.territory.as_str()).collect()
    }

    // --- ordering ---

    #[test]
    fn boxes_sort_by_tower_dps_descending() {
        let boxes = build_war_boxes(&state(
            vec![],
            vec![
                war("Quiet", 1.0, Some(1_000), Some(500)),
                war("Loud", 1.0, Some(1_000), Some(9_000)),
                war("Middling", 1.0, Some(1_000), Some(3_000)),
            ],
            vec![],
        ));

        assert_eq!(names(&boxes), vec!["Loud", "Middling", "Quiet"]);
    }

    #[test]
    fn equal_dps_breaks_the_tie_on_remaining_ehp() {
        let boxes = build_war_boxes(&state(
            vec![],
            // Same DPS; 1_000_000 * 0.9 beats 1_000_000 * 0.5.
            vec![
                war("Thin", 0.5, Some(1_000_000), Some(4_000)),
                war("Thick", 0.9, Some(1_000_000), Some(4_000)),
            ],
            vec![],
        ));

        assert_eq!(names(&boxes), vec!["Thick", "Thin"]);
    }

    #[test]
    fn a_missing_dps_sinks_below_every_measured_war() {
        let boxes = build_war_boxes(&state(
            vec![],
            vec![
                war("Unmeasured", 1.0, Some(99_000_000), None),
                war("Feeble", 1.0, Some(1), Some(1)),
            ],
            vec![],
        ));

        assert_eq!(names(&boxes), vec!["Feeble", "Unmeasured"]);
    }

    #[test]
    fn entered_only_boxes_land_at_the_bottom() {
        let boxes = build_war_boxes(&state(
            vec![queue("Waiting", "ENTERED", 900)],
            vec![war("Fighting", 1.0, Some(10), Some(1))],
            vec![],
        ));

        assert_eq!(names(&boxes), vec!["Fighting", "Waiting"]);
    }

    #[test]
    fn identical_wars_fall_back_to_the_territory_name() {
        // Without a total order, two indistinguishable wars could swap places on every poll.
        let boxes = build_war_boxes(&state(
            vec![],
            vec![
                war("Zeta", 1.0, Some(500), Some(500)),
                war("Alpha", 1.0, Some(500), Some(500)),
            ],
            vec![],
        ));

        assert_eq!(names(&boxes), vec!["Alpha", "Zeta"]);
    }

    // --- remaining EHP ---

    #[test]
    fn remaining_ehp_scales_the_total_by_current_health() {
        let boxes = build_war_boxes(&state(
            vec![],
            vec![war("Entrance to Olux", 0.8731, Some(24_135_275), Some(1))],
            vec![],
        ));

        let remaining = boxes[0].remaining_ehp().expect("measured war");
        assert!((remaining - 21_072_508.5).abs() < 1.0, "got {remaining}");
        assert_eq!(format_stat(remaining), "21.1M");
    }

    #[test]
    fn remaining_ehp_is_absent_without_a_total_ehp() {
        // The feed really does send null ehp/dps on a live war.
        let boxes = build_war_boxes(&state(vec![], vec![war("Overtaken Outpost", 1.0, None, None)], vec![]));

        assert_eq!(boxes[0].remaining_ehp(), None);
        // ...but the bar still has a health fraction to draw.
        assert_eq!(boxes[0].health, Some(1.0));
        assert!(!boxes[0].is_awaiting_start());
    }

    #[test]
    fn remaining_ehp_clamps_health_outside_zero_to_one() {
        let over = war("Over", 1.4, Some(1_000), Some(1));
        let under = war("Under", -0.2, Some(1_000), Some(1));
        let boxes = build_war_boxes(&state(vec![], vec![over, under], vec![]));

        for war_box in &boxes {
            let remaining = war_box.remaining_ehp().expect("measured war");
            assert!((0.0..=1_000.0).contains(&remaining), "got {remaining}");
        }
    }

    #[test]
    fn entered_only_boxes_have_no_tower_stats() {
        let boxes = build_war_boxes(&state(vec![queue("Waiting", "ENTERED", 900)], vec![], vec![]));

        assert!(boxes[0].is_awaiting_start());
        assert_eq!(boxes[0].remaining_ehp(), None);
        assert_eq!(boxes[0].dps, None);
    }

    // --- members ---

    #[test]
    fn members_come_only_from_the_matching_territory() {
        let boxes = build_war_boxes(&state(
            vec![],
            vec![war("Here", 1.0, Some(1), Some(1))],
            vec![
                player("inside", "WARRIOR", Some("Here")),
                player("elsewhere", "MAGE", Some("There")),
            ],
        ));

        assert_eq!(boxes[0].members.len(), 1);
        assert_eq!(boxes[0].members[0].username, "inside");
    }

    #[test]
    fn roaming_players_without_a_territory_are_ignored() {
        let boxes = build_war_boxes(&state(
            vec![],
            vec![war("Here", 1.0, Some(1), Some(1))],
            vec![player("Yearnm", "MAGE", None)],
        ));

        assert!(boxes[0].members.is_empty());
        assert_eq!(boxes[0].extra_members, 0);
    }

    #[test]
    fn members_cap_at_five_and_report_the_overflow() {
        let roster: Vec<WarPlayer> = ["a", "b", "c", "d", "e", "f", "g"]
            .iter()
            .map(|name| player(name, "WARRIOR", Some("Crowded")))
            .collect();
        let boxes = build_war_boxes(&state(
            vec![],
            vec![war("Crowded", 1.0, Some(1), Some(1))],
            roster,
        ));

        assert_eq!(boxes[0].members.len(), MAX_MEMBERS);
        assert_eq!(boxes[0].extra_members, 2);
    }

    #[test]
    fn members_sort_by_username_so_the_visible_five_do_not_reshuffle() {
        // Same roster, opposite feed order: the visible five must be identical.
        let forward: Vec<WarPlayer> = ["delta", "Alpha", "charlie", "bravo"]
            .iter()
            .map(|name| player(name, "MAGE", Some("T")))
            .collect();
        let mut backward = forward.clone();
        backward.reverse();

        let a = build_war_boxes(&state(vec![], vec![war("T", 1.0, Some(1), Some(1))], forward));
        let b = build_war_boxes(&state(vec![], vec![war("T", 1.0, Some(1), Some(1))], backward));

        let usernames: Vec<&str> = a[0].members.iter().map(|m| m.username.as_str()).collect();
        assert_eq!(usernames, vec!["Alpha", "bravo", "charlie", "delta"]);
        assert_eq!(a[0].members, b[0].members);
    }

    #[test]
    fn an_unknown_class_keeps_the_member_without_an_icon() {
        let boxes = build_war_boxes(&state(
            vec![],
            vec![war("T", 1.0, Some(1), Some(1))],
            vec![player("bard_main", "BARD", Some("T"))],
        ));

        assert_eq!(boxes[0].members[0].username, "bard_main");
        assert_eq!(boxes[0].members[0].class, None);
    }

    #[test]
    fn underscored_class_names_still_resolve() {
        // `class_icon_name` would reject "DARK_WIZARD"; the shared parser accepts it, which is
        // why members carry the parsed enum rather than the raw string.
        let boxes = build_war_boxes(&state(
            vec![],
            vec![war("T", 1.0, Some(1), Some(1))],
            vec![player("wizard", "DARK_WIZARD", Some("T"))],
        ));

        assert_eq!(boxes[0].members[0].class, Some(PlayerClass::Mage));
    }

    // --- entered folding and ETA ---

    #[test]
    fn entered_entries_without_a_war_row_get_their_own_box() {
        let boxes = build_war_boxes(&state(vec![queue("Waiting", "ENTERED", 900)], vec![], vec![]));

        assert_eq!(names(&boxes), vec!["Waiting"]);
    }

    #[test]
    fn an_entered_entry_with_a_live_war_does_not_duplicate_the_box() {
        let boxes = build_war_boxes(&state(
            vec![queue("Contested", "ENTERED", 900)],
            vec![war("Contested", 1.0, Some(1), Some(1))],
            vec![],
        ));

        assert_eq!(names(&boxes), vec!["Contested"]);
        assert!(!boxes[0].is_awaiting_start());
    }

    #[test]
    fn queued_entries_without_a_war_row_are_skipped() {
        let boxes = build_war_boxes(&state(
            vec![queue("Queued", "QUEUED", 900), queue("Odd", "LEFT", 900)],
            vec![],
            vec![],
        ));

        assert!(boxes.is_empty());
    }

    #[test]
    fn war_eta_comes_from_the_matching_started_queue_entry() {
        // A STARTED entry's timestamp is the expected win instant; the snapshot is at 1_000.
        let boxes = build_war_boxes(&state(
            vec![queue("Olux", "STARTED", 1_180)],
            vec![war("Olux", 1.0, Some(1), Some(1))],
            vec![],
        ));

        assert_eq!(boxes[0].eta, Some(180));
    }

    #[test]
    fn a_war_without_a_queue_entry_has_no_eta() {
        let boxes = build_war_boxes(&state(vec![], vec![war("Orphan", 1.0, Some(1), Some(1))], vec![]));

        assert_eq!(boxes[0].eta, None);
    }

    #[test]
    fn entered_only_boxes_have_no_eta() {
        let boxes = build_war_boxes(&state(vec![queue("Waiting", "ENTERED", 1_180)], vec![], vec![]));

        assert_eq!(boxes[0].eta, None);
    }

    // --- overflow ---

    #[test]
    fn strip_layout_shows_every_box_when_they_all_fit() {
        // 4 cards need 4*208 + 3*8 = 856.
        let layout = strip_layout(1_000.0, 4);

        assert_eq!(layout.visible, 4);
        assert_eq!(layout.hidden, 0);
        assert_eq!(layout.scroller_width, 856.0);
    }

    #[test]
    fn strip_layout_reserves_chip_space_only_once_something_overflows() {
        // 872px fits exactly 4 cards (856) with 16 to spare - not enough for a chip. With 4
        // cards nothing hides, so all 4 show; with 5, the chip's slot costs one of them.
        assert_eq!(strip_layout(872.0, 4).visible, 4);
        assert_eq!(strip_layout(872.0, 4).hidden, 0);
        assert_eq!(strip_layout(872.0, 5).visible, 3);
        assert_eq!(strip_layout(872.0, 5).hidden, 2);
    }

    #[test]
    fn strip_layout_counts_every_box_past_the_visible_run() {
        let layout = strip_layout(1_000.0, 12);

        assert_eq!(layout.visible + layout.hidden, 12);
        assert!(layout.hidden > 0);
    }

    #[test]
    fn strip_layout_keeps_one_box_visible_in_a_cramped_strip() {
        let layout = strip_layout(150.0, 3);

        assert_eq!(layout.visible, 1);
        assert_eq!(layout.hidden, 2);
    }

    #[test]
    fn strip_layout_is_empty_without_any_wars() {
        assert_eq!(
            strip_layout(1_000.0, 0),
            StripLayout {
                visible: 0,
                hidden: 0,
                scroller_width: 0.0
            }
        );
    }

    #[test]
    fn strip_scroller_width_lands_on_a_box_boundary() {
        // No half card may peek past the clip.
        for total in 1..12 {
            let layout = strip_layout(700.0, total);
            let pitch = BOX_WIDTH + BOX_GAP;
            assert_eq!(layout.scroller_width, layout.visible as f64 * pitch - BOX_GAP);
        }
    }

    // --- offsets ---

    #[test]
    fn strip_starts_right_of_the_minimap_and_hugs_the_edge_without_it() {
        assert_eq!(strip_left_px(true), 228.0);
        assert_eq!(strip_left_px(false), 16.0);
    }

    #[test]
    fn strip_lifts_over_the_history_timeline() {
        assert_eq!(strip_bottom_px(true), 68.0);
        assert_eq!(strip_bottom_px(false), 16.0);
    }

    #[test]
    fn strip_right_edge_follows_the_open_sidebar() {
        assert_eq!(strip_right_px(true, 420.0), 436.0);
        // Closed, it still has to clear the collapse toggle.
        assert_eq!(strip_right_px(false, 420.0), 64.0);
    }

    #[test]
    fn available_width_never_goes_negative() {
        let left = strip_left_px(true);
        let right = strip_right_px(true, 620.0);
        assert_eq!(strip_available_px(600.0, left, right), 0.0);
    }

    // --- presentation helpers ---

    #[test]
    fn health_colour_steps_from_green_through_gold_to_red() {
        assert_eq!(health_color(1.0), COLOR_HEALTHY);
        assert_eq!(health_color(0.51), COLOR_HEALTHY);
        assert_eq!(health_color(0.5), COLOR_ENTERED);
        assert_eq!(health_color(0.21), COLOR_ENTERED);
        assert_eq!(health_color(0.2), COLOR_AT_WAR);
        assert_eq!(health_color(0.0), COLOR_AT_WAR);
    }

    #[test]
    fn an_absent_figure_renders_as_a_dash() {
        assert_eq!(format_figure(None), "\u{2014}");
        assert_eq!(format_figure(Some(32_143.0)), "32k");
    }
}
