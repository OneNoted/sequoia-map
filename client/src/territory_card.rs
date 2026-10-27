//! Pieces the territory hover card and the sidebar detail share, so the glance and the full
//! view say the same things in the same words: the war line with who queued the territory
//! and when, the queue's details, and one resource table.

use leptos::prelude::*;
use sequoia_shared::war_board::{
    Ending, FrontStage, QUEUE_ENTRY_TO_START_SECS, QueuedBy, WarFront, format_clock,
};
use sequoia_shared::{Resources, TerritoryRuntimeData, TreasuryLevel};

use crate::colors::rgba_css;
use crate::defense::DefenseTier;
use crate::format::{ago, compact, local_time, local_time_seconds};
use crate::territory::ClientTerritoryMap;
use crate::ui::Disclosure;
use crate::war_room::{WarRoom, clock_text, ending, stage_slug};

/// How long a new capture stays protected from another queue.
const COOLDOWN_SECS: i64 = 600;

/// The treasury tier a reporter saw, or failing that the one the time held implies.
pub(crate) fn treasury(runtime: Option<&TerritoryRuntimeData>, held_secs: i64) -> TreasuryLevel {
    runtime
        .and_then(|runtime| runtime.treasury.as_deref())
        .and_then(TreasuryLevel::from_api_tier)
        .unwrap_or_else(|| TreasuryLevel::from_held_seconds(held_secs.max(0)))
}

/// The cooldown still left on a territory held for `held_secs`, as `m:ss` and the share left.
pub(crate) fn cooldown(held_secs: i64) -> Option<(String, f64)> {
    let remaining = COOLDOWN_SECS - held_secs.max(0);
    (remaining > 0).then(|| {
        (
            format!("{}:{:02}", remaining / 60, remaining % 60),
            remaining as f64 / COOLDOWN_SECS as f64,
        )
    })
}

/// The name line every card opens with: the guild's swatch, the name, and HQ when it is one.
/// The sidebar's is the panel's heading; the glances' are plain text.
#[component]
pub(crate) fn TerritoryHead(
    name: String,
    hq: bool,
    #[prop(optional)] heading: bool,
) -> impl IntoView {
    let name = if heading {
        view! { <h2 class="tcard-name">{name}</h2> }.into_any()
    } else {
        view! { <span class="tcard-name">{name}</span> }.into_any()
    };
    view! {
        <header class="tcard-head">
            <span class="tcard-swatch" aria-hidden="true" />
            {name}
            {hq.then(|| view! { <span class="tcard-hq" title="Headquarters">"HQ"</span> })}
        </header>
    }
}

/// A treasury tier in its colour, with the production buff it gives when asked for.
#[component]
pub(crate) fn TreasuryValue(level: TreasuryLevel, #[prop(optional)] buff: bool) -> impl IntoView {
    let (r, g, b) = level.color_rgb();
    let percent = level.buff_percent();
    view! {
        <dd style:color=rgba_css(r, g, b, 1.0)>
            {level.label()}
            {(buff && percent > 0).then(|| view! { <span class="tcard-buff">{format!("+{percent}%")}</span> })}
        </dd>
    }
}

/// A reported defense tier in the rating palette the calculator uses too.
#[component]
pub(crate) fn DefenseValue(tier: String) -> impl IntoView {
    let tier = DefenseTier::parse(&tier);
    view! { <dd class="tcard-defense" style:color=tier.color()>{tier.label().to_string()}</dd> }
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

/// A territory's resource rows from its base generation and its live runtime figures. Reported
/// production is used even when every figure is zero: that is an observed zero, not an absent
/// report. Held and capacity figures that are all zero count as not reported.
pub(crate) fn territory_resource_rows(
    base: &Resources,
    runtime: Option<&TerritoryRuntimeData>,
) -> Vec<ResourceRow> {
    resource_rows(
        base,
        runtime
            .and_then(|runtime| runtime.held_resources.as_ref())
            .filter(|value| !value.is_empty()),
        runtime.and_then(|runtime| runtime.production_rates.as_ref()),
        runtime
            .and_then(|runtime| runtime.storage_capacity.as_ref())
            .filter(|value| !value.is_empty()),
    )
}

/// Rows for every resource the territory makes, holds or stores, from its base generation and
/// whatever live figures exist. Live production replaces base generation when present; a
/// resource the territory generates keeps its row under a reported zero, so the zero shows.
pub(crate) fn resource_rows(
    base: &Resources,
    held: Option<&Resources>,
    production: Option<&Resources>,
    capacity: Option<&Resources>,
) -> Vec<ResourceRow> {
    let per_hour = production.unwrap_or(base);
    let row = |icon, label, double, pick: fn(&Resources) -> i32| {
        let row = ResourceRow {
            icon,
            label,
            double,
            held: held.map(pick),
            per_hour: pick(per_hour),
            cap: capacity.map(pick),
        };
        let generated = pick(base) > 0;
        (row.per_hour > 0
            || generated
            || row.held.is_some_and(|v| v > 0)
            || row.cap.is_some_and(|v| v > 0))
        .then_some(row)
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
    .flatten()
    .collect()
}

/// An amount as the resource table writes it, `9.0k`, or a dash while unreported.
fn amount(value: Option<i32>) -> String {
    value.map_or_else(|| "\u{2013}".to_string(), |value| compact(i64::from(value)))
}

/// Every resource as one row: what is held against its storage cap, and production per hour.
/// The held and cap column only appears when either figure is known.
#[component]
pub(crate) fn ResourceTable(
    rows: Vec<ResourceRow>,
    #[prop(optional)] icon_px: u32,
) -> impl IntoView {
    if rows.is_empty() {
        return ().into_any();
    }
    let icon_px = if icon_px == 0 { 12 } else { icon_px };
    let show_storage = rows
        .iter()
        .any(|row| row.held.is_some() || row.cap.is_some());
    view! {
        <table class="tcard-resources">
            <thead>
                <tr>
                    <th scope="col"><span class="sr-only">"Resource"</span></th>
                    {show_storage.then(|| view! { <th scope="col">"Held / Cap"</th> })}
                    <th scope="col">"Per hour"</th>
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
                            {show_storage.then(|| view! {
                                <td class="tcard-storage" class:tcard-full=full title=full.then_some("Storage nearly full")>
                                    {amount(row.held)}
                                    <span class="tcard-cap">" / " {amount(row.cap)}</span>
                                </td>
                            })}
                            <td>{amount(Some(row.per_hour))}</td>
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
    fn the_cooldown_runs_ten_minutes_from_the_capture() {
        assert_eq!(cooldown(0), Some(("10:00".to_string(), 1.0)));
        assert_eq!(
            cooldown(545).map(|(left, _)| left),
            Some("0:55".to_string())
        );
        assert_eq!(cooldown(600), None);
        assert_eq!(
            cooldown(-30).map(|(left, _)| left),
            Some("10:00".to_string())
        );
    }

    #[test]
    fn a_reported_treasury_wins_over_the_time_held() {
        let reported = TerritoryRuntimeData {
            treasury: Some("HIGH".to_string()),
            ..Default::default()
        };
        assert_eq!(treasury(Some(&reported), 0), TreasuryLevel::High);
        assert_eq!(treasury(None, 0), TreasuryLevel::from_held_seconds(0));
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
    fn reported_zero_production_stays_zero_rather_than_falling_back_to_base() {
        let base = resources(9_000, 3_600, 0);
        let runtime = TerritoryRuntimeData {
            held_resources: Some(resources(140, 60, 0)),
            production_rates: Some(Resources::default()),
            ..Default::default()
        };
        let rows = territory_resource_rows(&base, Some(&runtime));
        assert_eq!(
            rows.iter()
                .map(|row| (row.label, row.per_hour))
                .collect::<Vec<_>>(),
            [("Emeralds", 0), ("Ore", 0)]
        );
        // Without a report, base generation stands in.
        let absent = territory_resource_rows(&base, None);
        assert_eq!(absent[0].per_hour, 9_000);
    }

    #[test]
    fn reported_zero_production_keeps_its_rows_without_holdings() {
        let base = resources(9_000, 3_600, 0);
        let zero = Some(Resources::default());
        let shown = |runtime: &TerritoryRuntimeData| {
            territory_resource_rows(&base, Some(runtime))
                .iter()
                .map(|row| (row.label, row.held, row.per_hour, row.cap))
                .collect::<Vec<_>>()
        };
        let expected = [("Emeralds", None, 0, None), ("Ore", None, 0, None)];
        // Held and capacity absent.
        let absent = TerritoryRuntimeData {
            production_rates: zero.clone(),
            ..Default::default()
        };
        assert_eq!(shown(&absent), expected);
        // Held and capacity reported, but all zero.
        let empty = TerritoryRuntimeData {
            held_resources: zero.clone(),
            production_rates: zero.clone(),
            storage_capacity: zero,
            ..Default::default()
        };
        assert_eq!(shown(&empty), expected);
        // A resource the territory neither generates, holds nor stores still has no row.
        assert!(shown(&empty).iter().all(|(label, ..)| *label != "Crops"));
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
}
