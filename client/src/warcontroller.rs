//! The live war controller feed, as seen from the map client: the fetch that seeds it and
//! the top-left overview panel that renders it.
//!
//! Both are for Sequoia members only. The server refuses `/api/warcontroller` and withholds
//! the SSE `warcontroller` frames from everyone else; [`crate::app::WarFeedVisible`] is the
//! display half of that same rule.
//!
//! The two things a row encodes are split: the section header it sits under is its queue
//! stage, and its square is the territory's difficulty. The square used to repeat the stage,
//! which the header already said.

use std::rc::Rc;

use leptos::prelude::*;
use sequoia_shared::{QueueStatus, TreasuryLevel, WarControllerState, WarQueueEntry};
use wasm_bindgen::JsCast;

use crate::app::{
    CurrentMode, IsMobile, MapMode, ShowWarQueue, WarControllerData, WarFeedVisible, WarPanelOpen,
    WarPanelWidth, clamp_war_panel_width,
};

const WARCONTROLLER_ENDPOINT: &str = "/api/warcontroller";
/// Touch layouts get one fixed width instead of the drag-resizable desktop one.
const MOBILE_WAR_PANEL_WIDTH: f64 = 244.0;

/// Queue stages, most urgent first: the order the panel lists its sections in.
///
/// Stage no longer carries a colour - the row's square encodes difficulty instead, via
/// [`difficulty_color`].
const SECTIONS: [QueueStatus; 3] = [
    QueueStatus::Started,
    QueueStatus::Entered,
    QueueStatus::Queued,
];

pub async fn fetch_warcontroller() -> Result<WarControllerState, String> {
    let response = gloo_net::http::Request::get(WARCONTROLLER_ENDPOINT)
        .send()
        .await
        .map_err(|error| format!("request failed: {error}"))?;
    if !response.ok() {
        return Err(format!("status {}", response.status()));
    }
    response
        .json::<WarControllerState>()
        .await
        .map_err(|error| format!("decode failed: {error}"))
}

/// Collapsible overview of the war queue, pinned to the top-left of the map.
///
/// Collapses toward the left edge the way the sidebar collapses toward the right, leaving
/// only its chevron behind.
#[component]
pub(crate) fn WarQueuePanel() -> impl IntoView {
    let WarControllerData(warcontroller_state) = expect_context();
    let WarFeedVisible(war_feed_visible) = expect_context();
    let WarPanelOpen(panel_open) = expect_context();
    let WarPanelWidth(panel_width) = expect_context();
    let IsMobile(is_mobile) = expect_context();
    let CurrentMode(map_mode) = expect_context();
    let ShowWarQueue(show_war_queue) = expect_context();

    // The feed describes right now, so it must never be shown against a past snapshot -
    // the same rule the tooltip's "In War" line follows. The setting sits on top of that:
    // it hides the panel outright, chevron included, unlike the collapse toggle.
    let visible = Memo::new(move |_| {
        show_war_queue.get() && war_feed_visible.get() && map_mode.get() == MapMode::Live
    });

    let rows = Memo::new(move |_| {
        warcontroller_state.with(|state| {
            state
                .as_ref()
                .map(|state| grouped_queue(&state.queues))
                .unwrap_or_default()
        })
    });
    let total = Memo::new(move |_| {
        rows.with(|rows| rows.iter().map(|(_, entries)| entries.len()).sum::<usize>())
    });
    // The ETAs are seconds remaining as of the moment the backend built the payload, so the
    // countdown is measured from that instant rather than from when a row was rendered.
    let feed_timestamp = Memo::new(move |_| {
        warcontroller_state.with(|state| state.as_ref().map(|state| state.timestamp).unwrap_or(0))
    });

    // Transitions only after mount, so a panel restored as collapsed does not animate
    // shut on first paint. Mirrors `.sidebar-ready`.
    let ready: RwSignal<bool> = RwSignal::new(false);
    Effect::new(move |_| ready.set(true));

    view! {
        <Show when=move || visible.get()>
            <div
                class="war-panel"
                class:war-panel-ready=move || ready.get()
                class:war-panel-open=move || panel_open.get()
                style:width=move || {
                    if is_mobile.get() {
                        // Fixed on touch: the panel already sits near the screen edge, and
                        // there is no pointer to drag its handle with.
                        format!("{MOBILE_WAR_PANEL_WIDTH:.0}px")
                    } else {
                        format!("{:.0}px", panel_width.get())
                    }
                }
            >
                <WarPanelToggle panel_open />
                <WarPanelResizeHandle />
                <div class="war-panel-card">
                    <div style="display: flex; align-items: center; justify-content: space-between; gap: 8px; margin-bottom: 9px;">
                        <span style="font-family: var(--font-display); font-size: 0.8rem; letter-spacing: 0.12em; text-transform: uppercase; color: var(--color-text-secondary);">
                            "War Queue"
                        </span>
                        <span style="font-family: var(--font-mono); font-size: 0.74rem; color: #6f748f; font-variant-numeric: tabular-nums;">
                            {move || total.get().to_string()}
                        </span>
                    </div>
                    <div class="scrollbar-thin" style="max-height: min(46vh, 380px); overflow-y: auto;">
                        {move || {
                            if total.get() == 0 {
                                return view! {
                                    <div style="font-family: var(--font-mono); font-size: 0.76rem; color: #6f748f; padding: 2px 0 1px;">
                                        "No active wars"
                                    </div>
                                }.into_any();
                            }
                            rows.get()
                                .into_iter()
                                .map(|(status, entries)| {
                                    view! { <WarQueueSection status entries feed_timestamp /> }
                                })
                                .collect_view()
                                .into_any()
                        }}
                    </div>
                </div>
            </div>
        </Show>
    }
}

#[component]
fn WarQueueSection(
    status: QueueStatus,
    entries: Vec<WarQueueEntry>,
    feed_timestamp: Memo<i64>,
) -> impl IntoView {
    // The shared 1-second clock, so the ETAs keep ticking down between feed updates
    // instead of freezing until the next poll lands.
    let tick: RwSignal<i64> = expect_context();
    let count = entries.len();

    view! {
        <div style="margin-bottom: 9px;">
            <div style="display: flex; align-items: center; justify-content: space-between; gap: 8px; border-top: 1px solid rgba(40,44,62,0.65); padding-top: 7px; margin-bottom: 4px;">
                <span style="font-family: var(--font-mono); font-size: 0.68rem; letter-spacing: 0.1em; text-transform: uppercase; color: #6f748f;">
                    {status.label()}
                </span>
                <span style="font-family: var(--font-mono); font-size: 0.68rem; color: #6f748f; font-variant-numeric: tabular-nums;">
                    {count.to_string()}
                </span>
            </div>
            {entries
                .into_iter()
                .map(|entry| {
                    let territory = entry.territory.clone();
                    // The square is the territory's difficulty, not its queue stage - the
                    // section header above already says the stage.
                    let color = difficulty_color(entry.difficulty.as_deref());
                    let title = row_title(&entry.territory, entry.difficulty.as_deref());
                    // The whole entry rides into the ETA closure: a STARTED entry ships its
                    // ETA in `timestamp`, so the remaining time has to be re-derived against
                    // whichever snapshot is current, not frozen at render time.
                    let eta_entry = entry;
                    view! {
                        <div style="display: flex; align-items: center; gap: 8px; padding: 2px 0;">
                            <span style={format!("flex-shrink: 0; width: 10px; height: 10px; border-radius: 2px; background: {color}; border: 1px solid rgba(255,255,255,0.18);")} />
                            <span
                                title=title
                                style="flex: 1; min-width: 0; font-family: var(--font-mono); font-size: 0.8rem; color: #d8d5cb; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;"
                            >
                                {territory}
                            </span>
                            <span style="flex-shrink: 0; font-family: var(--font-mono); font-size: 0.76rem; color: #6f748f; font-variant-numeric: tabular-nums;">
                                {move || {
                                    let feed = feed_timestamp.get();
                                    format_eta(eta_entry.eta_secs(feed), feed, tick.get())
                                }}
                            </span>
                        </div>
                    }
                })
                .collect_view()}
        </div>
    }
}

/// Drag the panel wider or narrower from its right edge.
///
/// The mirror image of the sidebar's own handle, which drags from its left edge: the same
/// pointer capture, the same clamped width, persisted the same way.
#[component]
fn WarPanelResizeHandle() -> impl IntoView {
    let WarPanelOpen(panel_open) = expect_context();
    let WarPanelWidth(panel_width) = expect_context();
    let IsMobile(is_mobile) = expect_context();

    let drag_start_x = Rc::new(std::cell::Cell::new(0.0f64));
    let drag_start_width = Rc::new(std::cell::Cell::new(0.0f64));
    let dragging: RwSignal<bool> = RwSignal::new(false);
    let active_pointer_id = Rc::new(std::cell::Cell::new(None::<i32>));

    let drag_start_x_down = drag_start_x.clone();
    let drag_start_width_down = drag_start_width.clone();
    let active_pointer_id_down = active_pointer_id.clone();
    let drag_start_x_move = drag_start_x.clone();
    let drag_start_width_move = drag_start_width.clone();
    let active_pointer_id_move = active_pointer_id.clone();
    let active_pointer_id_end = active_pointer_id.clone();

    let end_drag: Rc<dyn Fn(web_sys::PointerEvent)> = Rc::new(move |e: web_sys::PointerEvent| {
        if active_pointer_id_end.get() != Some(e.pointer_id()) {
            return;
        }
        dragging.set(false);
        active_pointer_id_end.set(None);
        if let Some(target) = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
        {
            target.release_pointer_capture(e.pointer_id()).ok();
        }
    });

    let end_drag_up = end_drag.clone();
    let end_drag_cancel = end_drag.clone();

    view! {
        <div
            class="war-panel-resize-handle"
            class:war-panel-resize-active=move || dragging.get()
            style:display=move || {
                if !is_mobile.get() && panel_open.get() { "block" } else { "none" }
            }
            on:pointerdown=move |e: web_sys::PointerEvent| {
                if !e.is_primary() || e.button() != 0 || is_mobile.get_untracked()
                    || !panel_open.get_untracked()
                {
                    return;
                }
                e.prevent_default();
                dragging.set(true);
                active_pointer_id_down.set(Some(e.pointer_id()));
                drag_start_x_down.set(e.client_x() as f64);
                drag_start_width_down.set(panel_width.get_untracked());
                if let Some(target) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                    target.set_pointer_capture(e.pointer_id()).ok();
                }
            }
            on:pointermove=move |e: web_sys::PointerEvent| {
                if !dragging.get_untracked() || active_pointer_id_move.get() != Some(e.pointer_id()) {
                    return;
                }
                e.prevent_default();
                // Growing to the right, so the delta is the reverse of the sidebar's.
                let next_width = clamp_war_panel_width(
                    drag_start_width_move.get() + (e.client_x() as f64 - drag_start_x_move.get()),
                );
                panel_width.set(next_width);
            }
            on:pointerup=move |e: web_sys::PointerEvent| {
                end_drag_up(e);
            }
            on:pointercancel=move |e: web_sys::PointerEvent| {
                end_drag_cancel(e);
            }
        />
    }
}

/// Collapse control on the panel's right edge, styled after the sidebar's own toggle.
#[component]
fn WarPanelToggle(panel_open: RwSignal<bool>) -> impl IntoView {
    view! {
        <button
            class="war-panel-toggle"
            title=move || if panel_open.get() { "Hide war queue" } else { "Show war queue" }
            style="position: absolute; top: 0; right: -44px; z-index: 11; width: 32px; height: 32px; background: var(--color-deep); border: 1px solid var(--color-border-subtle); border-radius: 6px; cursor: pointer; display: flex; align-items: center; justify-content: center; transition: border-color 0.15s, background 0.15s, color 0.15s; color: var(--color-text-dim); font-family: var(--font-mono); font-size: 1.1rem; line-height: 1;"
            on:click=move |_| panel_open.update(|open| *open = !*open)
            on:mouseenter=move |e| {
                if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                    el.style().set_property("border-color", "rgba(245,197,66,0.4)").ok();
                    el.style().set_property("color", "var(--color-gold)").ok();
                    el.style().set_property("background", "var(--color-surface)").ok();
                }
            }
            on:mouseleave=move |e| {
                if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                    el.style().set_property("border-color", "var(--color-border-subtle)").ok();
                    el.style().set_property("color", "var(--color-text-dim)").ok();
                    el.style().set_property("background", "var(--color-deep)").ok();
                }
            }
        >
            {move || if panel_open.get() { "\u{00AB}" } else { "\u{00BB}" }}
        </button>
    }
}

/// Square colour for a Wynncraft difficulty tier, light green through dark red.
///
/// Shared with [`crate::war_stats`], whose cards dot the same tiers the same way. Note this
/// is deliberately *not* [`TreasuryLevel::color_rgb`]: that is the Minecraft treasury palette,
/// where High is green and Very High is cyan, which would read as backwards here.
pub(crate) fn difficulty_color(raw: Option<&str>) -> &'static str {
    match raw.and_then(TreasuryLevel::from_api_tier) {
        Some(TreasuryLevel::VeryLow) => "#8ce99a",
        Some(TreasuryLevel::Low) => "#50c878",
        Some(TreasuryLevel::Medium) => "#f5c542",
        Some(TreasuryLevel::High) => "#ff4545",
        Some(TreasuryLevel::VeryHigh) => "#8b1a1a",
        None => "#6f748f",
    }
}

/// Row tooltip: the full territory name, plus the difficulty its square stands for.
///
/// Long names are ellipsized in the row, so the untruncated one has to live here anyway; the
/// tier is what makes the colour decodable without a legend. An unclassified territory gets
/// the bare name, matching the neutral square [`difficulty_color`] gives it.
fn row_title(territory: &str, difficulty: Option<&str>) -> String {
    match difficulty.and_then(TreasuryLevel::from_api_tier) {
        Some(level) => format!("{territory} \u{2014} {}", level.label()),
        None => territory.to_string(),
    }
}

/// Buckets the feed's queue entries into the three stages, oldest first within each.
///
/// `timestamp` means two different things - see [`WarQueueEntry::timestamp`] - and oldest
/// first is the useful order under both. For `QUEUED` and `ENTERED` it is the instant the
/// entry reached that stage, so the section lists territories in the order they will start;
/// for `STARTED` it is the expected win instant, so that section leads with the war expected
/// to finish soonest. Entries the backend sent no stamp for have no place in that order, so
/// they trail their section. Entries whose status the backend has since renamed are dropped
/// rather than guessed at.
fn grouped_queue(queues: &[WarQueueEntry]) -> Vec<(QueueStatus, Vec<WarQueueEntry>)> {
    SECTIONS
        .iter()
        .filter_map(|status| {
            let mut entries: Vec<WarQueueEntry> = queues
                .iter()
                .filter(|entry| QueueStatus::from_api_status(&entry.status) == Some(*status))
                .cloned()
                .collect();
            if entries.is_empty() {
                return None;
            }
            // Unstamped entries sink to the bottom of their section: there is nothing to
            // order them by, and floating them above a `STARTED` section would displace the
            // war expected to be won soonest from the top.
            entries.sort_by_key(|entry| (entry.timestamp.is_none(), entry.timestamp));
            Some((*status, entries))
        })
        .collect()
}

/// The ETA column: time remaining until the war is expected to be won, and an em-dash for the
/// rows that have none.
///
/// Every stage ticks: a `STARTED` row counts down to its war being won, a `QUEUED` or
/// `ENTERED` row to its war starting - see [`WarQueueEntry::eta_secs`]. The em-dash is for
/// rows the backend sent no usable stamp for. The ETA is seconds remaining as of
/// `feed_timestamp`, so the elapsed time since that snapshot is subtracted to keep the column
/// moving between polls.
pub(crate) fn format_eta(eta: Option<i64>, feed_timestamp: i64, now: i64) -> String {
    let Some(eta) = eta.filter(|seconds| *seconds >= 0) else {
        return "\u{2014}".to_string();
    };
    // The clock is the browser's, so it can sit either side of the feed's timestamps; both
    // a skewed-behind clock and an overrun ETA are floored rather than rendered negative.
    let elapsed = (now - feed_timestamp).max(0);
    let seconds = (eta - elapsed).max(0);

    let minutes = seconds / 60;
    let secs = seconds % 60;
    if minutes >= 60 {
        format!("{}:{:02}:{secs:02}", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}:{secs:02}")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn entry(territory: &str, status: &str, timestamp: i64) -> WarQueueEntry {
        unstamped_entry(territory, status, Some(timestamp))
    }

    /// The same, for the entries the backend sends without a stamp.
    fn unstamped_entry(territory: &str, status: &str, timestamp: Option<i64>) -> WarQueueEntry {
        WarQueueEntry {
            territory: territory.to_string(),
            difficulty: Some("VERY_LOW".to_string()),
            status: status.to_string(),
            timestamp,
            eta: None,
        }
    }

    #[test]
    fn grouping_orders_sections_started_entered_then_queued() {
        let grouped = grouped_queue(&[
            entry("C", "QUEUED", 3),
            entry("A", "STARTED", 1),
            entry("B", "ENTERED", 2),
        ]);

        let statuses: Vec<QueueStatus> = grouped.iter().map(|(status, _)| *status).collect();
        assert_eq!(
            statuses,
            vec![
                QueueStatus::Started,
                QueueStatus::Entered,
                QueueStatus::Queued
            ]
        );
    }

    #[test]
    fn grouping_sorts_each_section_oldest_first() {
        let grouped = grouped_queue(&[
            entry("newer", "QUEUED", 200),
            entry("older", "QUEUED", 100),
            entry("newest", "QUEUED", 300),
        ]);

        let (_, entries) = &grouped[0];
        let names: Vec<&str> = entries.iter().map(|e| e.territory.as_str()).collect();
        assert_eq!(names, vec!["older", "newer", "newest"]);
    }

    #[test]
    fn grouping_omits_empty_sections_and_unknown_statuses() {
        let grouped = grouped_queue(&[entry("A", "QUEUED", 1), entry("B", "LEFT", 2)]);

        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].0, QueueStatus::Queued);
        assert_eq!(grouped[0].1.len(), 1);
        assert_eq!(grouped[0].1[0].territory, "A");
    }

    #[test]
    fn eta_formats_minutes_and_hours() {
        assert_eq!(format_eta(Some(0), 0, 0), "0:00");
        assert_eq!(format_eta(Some(9), 0, 0), "0:09");
        assert_eq!(format_eta(Some(125), 0, 0), "2:05");
        assert_eq!(format_eta(Some(3_725), 0, 0), "1:02:05");
    }

    #[test]
    fn eta_is_a_placeholder_when_absent_or_negative() {
        assert_eq!(format_eta(None, 0, 0), "\u{2014}");
        assert_eq!(format_eta(Some(-5), 0, 0), "\u{2014}");
    }

    #[test]
    fn eta_counts_down_from_the_feed_snapshot() {
        // 3:00 left as of the snapshot, seen 55 seconds later.
        assert_eq!(format_eta(Some(180), 1_000, 1_055), "2:05");
        assert_eq!(format_eta(Some(180), 1_000, 1_000), "3:00");
    }

    #[test]
    fn eta_floors_at_zero_once_it_runs_out() {
        // A war can overrun its predicted win time; the column holds at zero rather than
        // counting up past it.
        assert_eq!(format_eta(Some(30), 1_000, 1_100), "0:00");
    }

    #[test]
    fn eta_ignores_a_clock_sitting_behind_the_feed() {
        // The clock is the browser's, so it can sit behind the feed's timestamps; that must
        // not inflate the remaining time.
        assert_eq!(format_eta(Some(180), 1_060, 1_000), "3:00");
    }

    #[test]
    fn an_unstamped_entry_sorts_after_the_stamped_ones_in_its_section() {
        // Nothing orders an entry the backend sent no stamp for, and floating it to the top
        // of STARTED would displace the war expected to be won soonest.
        let grouped = grouped_queue(&[
            unstamped_entry("Mangled Lake", "STARTED", None),
            entry("Entrance to Olux", "STARTED", 3),
            entry("Overtaken Outpost", "STARTED", 1),
        ]);
        let (status, entries) = &grouped[0];
        assert_eq!(*status, QueueStatus::Started);
        let order: Vec<&str> = entries
            .iter()
            .map(|entry| entry.territory.as_str())
            .collect();
        assert_eq!(
            order,
            ["Overtaken Outpost", "Entrance to Olux", "Mangled Lake"]
        );
    }

    #[test]
    fn every_difficulty_tier_has_its_own_square_colour() {
        let colors = [
            difficulty_color(Some("VERY_LOW")),
            difficulty_color(Some("LOW")),
            difficulty_color(Some("MEDIUM")),
            difficulty_color(Some("HIGH")),
            difficulty_color(Some("VERY_HIGH")),
        ];
        assert_eq!(
            colors,
            ["#8ce99a", "#50c878", "#f5c542", "#ff4545", "#8b1a1a"]
        );
        // Light green through dark red, so no two tiers can be confused for each other.
        let unique: HashSet<&str> = colors.into_iter().collect();
        assert_eq!(unique.len(), colors.len());
    }

    #[test]
    fn difficulty_colour_tolerates_the_tier_labels_spelling() {
        // `from_api_tier` normalizes case and separators; the palette inherits that.
        assert_eq!(difficulty_color(Some("very high")), "#8b1a1a");
        assert_eq!(difficulty_color(Some("Very-Low")), "#8ce99a");
        assert_eq!(difficulty_color(Some(" medium ")), "#f5c542");
    }

    #[test]
    fn the_square_tracks_difficulty_rather_than_queue_stage() {
        // The regression this whole change is about. Two territories at the same difficulty
        // must read the same however far apart their stages are, and two at the same stage
        // must read differently when their difficulties differ - the reverse of the old
        // stage-coloured square.
        let mut low_queued = entry("Mangled Lake", "QUEUED", 1);
        low_queued.difficulty = Some("VERY_LOW".to_string());
        let mut low_started = entry("Overtaken Outpost", "STARTED", 2);
        low_started.difficulty = Some("VERY_LOW".to_string());
        let mut high_started = entry("Entrance to Olux", "STARTED", 3);
        high_started.difficulty = Some("VERY_HIGH".to_string());

        assert_eq!(
            difficulty_color(low_queued.difficulty.as_deref()),
            difficulty_color(low_started.difficulty.as_deref())
        );
        assert_ne!(
            difficulty_color(high_started.difficulty.as_deref()),
            difficulty_color(low_started.difficulty.as_deref())
        );
    }

    #[test]
    fn an_unknown_difficulty_falls_back_to_neutral() {
        // A tier the backend renames must not silently read as "very low".
        assert_eq!(difficulty_color(Some("EXTREME")), "#6f748f");
        assert_eq!(difficulty_color(Some("")), "#6f748f");
        // And so must a territory the backend never classified at all.
        assert_eq!(difficulty_color(None), "#6f748f");
        assert_eq!(row_title("Mangled Lake", None), "Mangled Lake");
    }

    #[test]
    fn the_row_tooltip_names_the_difficulty_behind_the_square() {
        assert_eq!(
            row_title("Entrance to Olux", Some("VERY_HIGH")),
            "Entrance to Olux \u{2014} Very High"
        );
        // Nothing to explain when the tier did not parse, so just the name.
        assert_eq!(row_title("Mangled Lake", Some("EXTREME")), "Mangled Lake");
    }
}
