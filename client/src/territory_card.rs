//! Pieces the territory hover card and the sidebar detail share, so the glance and the full
//! view say the same things in the same words: the war line with who queued the territory
//! and when, the queue's details, and one resource table.

use leptos::prelude::*;
use sequoia_shared::Resources;
use sequoia_shared::war_board::{
    Ending, FrontStage, QUEUE_ENTRY_TO_START_SECS, QueuedBy, WarFront, format_clock,
};

use crate::territory::ClientTerritoryMap;
use crate::war_room::{WarRoom, clock_text, ending, stage_slug};

/// A Unix time as the viewer's local wall-clock time, `14:02`.
pub(crate) fn local_time(at: i64) -> String {
    local(at).map_or_else(String::new, |time| time.format("%H:%M").to_string())
}

/// The same with seconds, for times a few seconds apart.
pub(crate) fn local_time_seconds(at: i64) -> String {
    local(at).map_or_else(String::new, |time| time.format("%H:%M:%S").to_string())
}

fn local(at: i64) -> Option<chrono::DateTime<chrono::Local>> {
    chrono::DateTime::from_timestamp(at, 0).map(|time| time.with_timezone(&chrono::Local))
}

/// How long ago, coarsely: `just now`, `6m ago`, `2h ago`.
pub(crate) fn ago(seconds: i64) -> String {
    match seconds.max(0) {
        0..60 => "just now".to_string(),
        seconds @ 60..3600 => format!("{}m ago", seconds / 60),
        seconds => format!("{}h ago", seconds / 3600),
    }
}

/// When a queue was made and by whom, as words; `None` when the backend said neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueueOrigin {
    /// Local time and how long ago.
    pub when: Option<(String, String)>,
    /// The queue's author, or `None` while the backend does not know it.
    pub queuer: Option<String>,
}

impl QueueOrigin {
    pub fn new(queued: &QueuedBy, now: i64, clock: impl Fn(i64) -> String) -> Option<Self> {
        if queued.at.is_none() && queued.queuer.is_none() {
            return None;
        }
        Some(Self {
            when: queued.at.map(|at| (clock(at), ago(now - at))),
            queuer: queued.queuer.clone(),
        })
    }
}

/// The territory's war, as the hover card and the sidebar both show it: stage and clock,
/// who queued it and when, and how a war that just left the board ended. Nothing when the
/// territory has no war and no recent outcome, or the viewer cannot see the war feed.
#[component]
pub(crate) fn TerritoryWar(
    #[prop(into)] territory: Signal<String>,
    /// The sidebar adds the queue's details behind a disclosure, open while this is true. It
    /// lives with the caller, which may rebuild this view while the disclosure stays open.
    #[prop(optional)]
    details_open: Option<RwSignal<bool>>,
) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let front = Memo::new(move |_| territory.with(|name| room.front(name)));
    let recent = Memo::new(move |_| {
        territory.with(|name| {
            room.outcomes.with(|list| {
                let outcome = list
                    .iter()
                    .rev()
                    .find(|outcome| &outcome.territory == name)?;
                Some((territories.with(|map| ending(outcome, map)), outcome.at))
            })
        })
    });

    move || match front.get() {
        Some(front) => view! { <FrontFacts front details_open /> }.into_any(),
        None => recent
            .get()
            .map(|(ending, at)| {
                let (slug, text) = match ending {
                    Ending::Captured => ("captured", "Captured"),
                    Ending::Cancelled => ("cancelled", "Queue cancelled"),
                    Ending::Ended => ("ended", "War ended"),
                };
                view! {
                    <div class="tcard-war" data-outcome=slug>
                        <span class="tcard-war-stage">{text}</span>
                        <span class="tcard-war-meta">{move || ago(room.now.get() - at)}</span>
                    </div>
                }
            })
            .into_any(),
    }
}

#[component]
fn FrontFacts(front: WarFront, details_open: Option<RwSignal<bool>>) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let kind = front.stage_kind();
    let clock = {
        let front = front.clone();
        Memo::new(move |_| front.clock(room.now.get()))
    };
    let origin = {
        let queued = front.queued.clone();
        Memo::new(move |_| {
            queued
                .as_ref()
                .and_then(|queued| QueueOrigin::new(queued, room.now.get(), local_time))
        })
    };
    let glyph = if front.is_fighting() {
        "war-glyph war-glyph-swords"
    } else {
        "war-glyph war-glyph-hourglass"
    };

    view! {
        <div class="tcard-war" data-stage=stage_slug(kind)>
            <div class="tcard-war-head">
                <span class=glyph aria-hidden="true" />
                <span class="tcard-war-stage">{kind.label()}</span>
                <span class="tcard-war-clock" role="timer" aria-label=move || clock_text(clock.get()).1>
                    {move || clock_text(clock.get()).0}
                </span>
            </div>
            {move || origin.get().map(|origin| view! { <QueueOriginLine origin /> })}
            {details_open.map(|open| view! { <QueueDetails front=front.clone() open /> })}
        </div>
    }
}

#[component]
fn QueueOriginLine(origin: QueueOrigin) -> impl IntoView {
    let queuer = match origin.queuer {
        Some(name) => view! { " by " <span class="tcard-war-queuer">{name}</span> }.into_any(),
        None => view! { ", queuer unknown" }.into_any(),
    };
    view! {
        <p class="tcard-war-origin">
            "Queued"
            {origin.when.map(|(time, ago)| view! {
                " " <time class="tcard-war-time">{time}</time>
                <span class="tcard-war-meta">" (" {ago} ")"</span>
            })}
            {queuer}
        </p>
    }
}

/// The queue's secondary timing, behind a disclosure: how long it was set for, when it runs
/// out and when the war is expected to start. Nothing once the war is being fought.
#[component]
pub(crate) fn QueueDetails(front: WarFront, open: RwSignal<bool>) -> impl IntoView {
    let (queued, starts_at) = match front.stage {
        FrontStage::Queued { starts_at, .. } => (true, starts_at),
        FrontStage::Entered { starts_at } => (false, starts_at),
        FrontStage::Fighting { .. } => return ().into_any(),
    };
    let ends_at = starts_at.map(|at| at - QUEUE_ENTRY_TO_START_SECS);
    let time = |at: Option<i64>| at.map_or_else(|| "Unknown".to_string(), local_time_seconds);
    view! {
        <Disclosure open summary="Queue details">
            <dl class="tcard-facts">
                {queued.then(|| view! {
                    <div><dt>"Queue timer"</dt><dd>{original_timer(&front)}</dd></div>
                    <div><dt>"Queue ends"</dt><dd>{time(ends_at)}</dd></div>
                })}
                <div><dt>"War starts"</dt><dd>{time(starts_at)}</dd></div>
            </dl>
        </Disclosure>
    }
    .into_any()
}

/// A `<details>` whose open state is held by `open`, so it survives its parent being rebuilt.
#[component]
pub(crate) fn Disclosure(
    open: RwSignal<bool>,
    summary: &'static str,
    #[prop(optional)] class: &'static str,
    children: Children,
) -> impl IntoView {
    view! {
        <details
            class=format!("tcard-disclosure {class}")
            prop:open=move || open.get()
            on:toggle=move |event| {
                let element: web_sys::HtmlDetailsElement = event_target(&event);
                open.set(element.open());
            }
        >
            <summary>{summary}</summary>
            {children()}
        </details>
    }
}

/// The queue's original length, or `Unknown` when the backend's timer is a guess.
pub(crate) fn original_timer(front: &WarFront) -> String {
    front.timer().map_or_else(
        || "Unknown".to_string(),
        |timer| format_clock(timer.duration()),
    )
}

/// One row of [`ResourceTable`]: a resource and whatever is known of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResourceRow {
    pub icon: &'static str,
    pub label: &'static str,
    pub double: bool,
    pub held: Option<i32>,
    pub per_hour: i32,
    pub cap: Option<i32>,
}

impl ResourceRow {
    /// Storage at least nine tenths full, worth noticing before it caps.
    pub fn nearly_full(&self) -> bool {
        match (self.held, self.cap) {
            (Some(held), Some(cap)) if cap > 0 => i64::from(held) * 10 >= i64::from(cap) * 9,
            _ => false,
        }
    }
}

/// Rows for every resource the territory makes, holds or stores, from its base generation and
/// whatever live figures exist. Live production replaces base generation when present.
pub(crate) fn resource_rows(
    base: &Resources,
    held: Option<&Resources>,
    production: Option<&Resources>,
    capacity: Option<&Resources>,
) -> Vec<ResourceRow> {
    let per_hour = production.unwrap_or(base);
    let row = |icon, label, double, pick: fn(&Resources) -> i32| ResourceRow {
        icon,
        label,
        double,
        held: held.map(pick),
        per_hour: pick(per_hour),
        cap: capacity.map(pick),
    };
    [
        row("emerald", "Emeralds", base.has_double_emeralds(), |r| {
            r.emeralds
        }),
        row("ore", "Ore", base.has_double_ore(), |r| r.ore),
        row("crops", "Crops", base.has_double_crops(), |r| r.crops),
        row("fish", "Fish", base.has_double_fish(), |r| r.fish),
        row("wood", "Wood", base.has_double_wood(), |r| r.wood),
    ]
    .into_iter()
    .filter(|row| {
        row.per_hour > 0 || row.held.is_some_and(|v| v > 0) || row.cap.is_some_and(|v| v > 0)
    })
    .collect()
}

/// `9.0k`, `300`.
pub(crate) fn compact_amount(value: i32) -> String {
    if value >= 1000 {
        format!("{:.1}k", f64::from(value) / 1000.0)
    } else {
        value.to_string()
    }
}

/// Every resource as one row, with a column per kind of figure known: held, per hour and
/// storage cap. Replaces a chip group per figure, which repeated every resource's name.
#[component]
pub(crate) fn ResourceTable(
    rows: Vec<ResourceRow>,
    #[prop(optional)] icon_px: u32,
) -> impl IntoView {
    if rows.is_empty() {
        return ().into_any();
    }
    let icon_px = if icon_px == 0 { 12 } else { icon_px };
    let show_held = rows.iter().any(|row| row.held.is_some());
    let show_cap = rows.iter().any(|row| row.cap.is_some());
    let cell = |value: Option<i32>| value.map_or_else(|| "\u{2013}".to_string(), compact_amount);
    view! {
        <table class="tcard-resources">
            <thead>
                <tr>
                    <th scope="col"><span class="sr-only">"Resource"</span></th>
                    {show_held.then(|| view! { <th scope="col">"Held"</th> })}
                    <th scope="col">"Per hour"</th>
                    {show_cap.then(|| view! { <th scope="col">"Cap"</th> })}
                </tr>
            </thead>
            <tbody>
                {rows.into_iter().map(|row| {
                    let icon = crate::ui_icons::sprite_style(row.icon, icon_px).unwrap_or_default();
                    let full = row.nearly_full();
                    view! {
                        <tr>
                            <th scope="row">
                                <span class="tcard-resource-icon" style=icon aria-hidden="true" />
                                {row.label}
                                {row.double.then(|| view! {
                                    <span class="tcard-double" title="Double generation">"\u{00D7}2"</span>
                                })}
                            </th>
                            {show_held.then(|| view! {
                                <td class:tcard-full=full title=full.then_some("Storage nearly full")>{cell(row.held)}</td>
                            })}
                            <td>{compact_amount(row.per_hour)}</td>
                            {show_cap.then(|| view! { <td>{cell(row.cap)}</td> })}
                        </tr>
                    }
                }).collect_view()}
            </tbody>
        </table>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resources(emeralds: i32, ore: i32, crops: i32) -> Resources {
        Resources {
            emeralds,
            ore,
            crops,
            fish: 0,
            wood: 0,
        }
    }

    #[test]
    fn a_queue_origin_says_when_and_who_and_keeps_an_unknown_queuer_unknown() {
        let clock = |at: i64| format!("t{at}");
        let known = QueuedBy {
            at: Some(1_000),
            queuer: Some("EpicPuppy613".to_string()),
        };
        assert_eq!(
            QueueOrigin::new(&known, 1_400, clock),
            Some(QueueOrigin {
                when: Some(("t1000".to_string(), "6m ago".to_string())),
                queuer: Some("EpicPuppy613".to_string()),
            })
        );
        let anonymous = QueuedBy {
            at: Some(1_000),
            queuer: None,
        };
        assert_eq!(
            QueueOrigin::new(&anonymous, 1_010, clock).map(|origin| origin.queuer),
            Some(None)
        );
        assert_eq!(QueueOrigin::new(&QueuedBy::default(), 1_000, clock), None);
    }

    #[test]
    fn ago_is_coarse() {
        assert_eq!(ago(-5), "just now");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(61), "1m ago");
        assert_eq!(ago(7_300), "2h ago");
    }

    #[test]
    fn resource_rows_merge_every_figure_and_prefer_live_production() {
        let base = resources(9_000, 3_600, 0);
        let held = resources(140, 60, 0);
        let production = resources(18_000, 3_600, 0);
        let capacity = resources(3_000, 300, 300);
        let rows = resource_rows(&base, Some(&held), Some(&production), Some(&capacity));

        assert_eq!(
            rows.iter().map(|row| row.label).collect::<Vec<_>>(),
            ["Emeralds", "Ore", "Crops"]
        );
        assert_eq!(rows[0].per_hour, 18_000);
        assert_eq!(
            (rows[2].held, rows[2].per_hour, rows[2].cap),
            (Some(0), 0, Some(300))
        );
        assert!(!rows[1].nearly_full());
    }

    #[test]
    fn base_generation_alone_fills_only_the_per_hour_column() {
        let rows = resource_rows(&resources(9_000, 0, 7_200), None, None, None);
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|row| row.held.is_none() && row.cap.is_none())
        );
    }

    #[test]
    fn storage_counts_as_nearly_full_from_nine_tenths() {
        let row = |held, cap| ResourceRow {
            icon: "ore",
            label: "Ore",
            double: false,
            held: Some(held),
            per_hour: 1,
            cap: Some(cap),
        };
        assert!(row(270, 300).nearly_full());
        assert!(!row(269, 300).nearly_full());
        assert!(!row(5, 0).nearly_full());
    }

    #[test]
    fn amounts_are_compact() {
        assert_eq!(compact_amount(300), "300");
        assert_eq!(compact_amount(9_000), "9.0k");
        assert_eq!(compact_amount(15_100), "15.1k");
    }
}
