//! Recent activity: Sequoia's captures and losses over the last day, and older pages back
//! through the week the backend keeps, opened on purpose from the war panel or a territory's
//! sidebar. Nothing loads until it is opened, and nothing is collected in the browser: every
//! page comes from the backend's journal.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use leptos::prelude::*;
use leptos::task::spawn_local;
use sequoia_shared::tower::format_stat;
use sequoia_shared::war_activity::{ActivityEntry, ActivityOutcome, StartSource, WarActivityPage};
use sequoia_shared::war_board::format_clock;

use super::{MapFocus, WarRoom, difficulty_color, difficulty_label};
use crate::app::ActivityIncludeUnconfirmed;
use crate::format::{ago, local_time};
use crate::ui::Disclosure;

const ENDPOINT: &str = "/api/war-activity";
/// The first page covers the last day; Load older reaches back through the backend's window.
const RECENT_SECS: i64 = 24 * 60 * 60;
const PAGE_SIZE: u32 = 25;
/// How often an open list checks for newer entries.
const REFRESH_MS: u64 = 60_000;
const REFRESH_SECS: i64 = (REFRESH_MS / 1000) as i64;

/// One request for a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityRequest {
    pub since: Option<i64>,
    pub before: Option<String>,
    pub territory: Option<String>,
    pub everything: bool,
}

impl ActivityRequest {
    pub fn url(&self) -> String {
        let mut url = format!("{ENDPOINT}?limit={PAGE_SIZE}");
        if let Some(since) = self.since {
            url.push_str(&format!("&since={since}"));
        }
        if let Some(before) = &self.before {
            url.push_str(&format!("&before={}", crate::encode_uri_component(before)));
        }
        if let Some(territory) = &self.territory {
            url.push_str(&format!(
                "&territory={}",
                crate::encode_uri_component(territory)
            ));
        }
        if self.everything {
            url.push_str("&scope=all");
        }
        url
    }
}

/// Where the list stands: what it shows, and what Load older would ask for next.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct ActivityList {
    pub entries: Vec<ActivityEntry>,
    /// The next older page within the current range, from the backend.
    next_before: Option<String>,
    /// Whether the week beyond the first day has been opened up yet.
    widened: bool,
    /// The day boundary the first page stopped at.
    first_since: Option<i64>,
    /// Loaded as the whole week from the start, as a territory's list is.
    whole_week: bool,
    pub ended: bool,
}

impl ActivityList {
    /// After the first page: the last day, or for `whole_week`, the whole week at once.
    pub fn first(page: WarActivityPage, since: i64, whole_week: bool) -> Self {
        let ended = whole_week && page.next_before.is_none();
        Self {
            entries: page.entries,
            next_before: page.next_before,
            widened: whole_week,
            first_since: Some(since),
            whole_week,
            ended,
        }
    }

    /// The request Load older makes: more of the day while the day has more, then the rest
    /// of the week from where the list stops.
    pub fn older(&self, base: &ActivityRequest) -> Option<ActivityRequest> {
        if self.ended {
            return None;
        }
        if let Some(before) = &self.next_before {
            let since = if self.widened { None } else { self.first_since };
            return Some(ActivityRequest {
                since,
                before: Some(before.clone()),
                ..base.clone()
            });
        }
        if self.widened {
            return None;
        }
        // The day is exhausted: carry on from its last entry, or from the day's edge.
        let before = self
            .entries
            .last()
            .map(|entry| entry.cursor.clone())
            .or_else(|| {
                self.first_since
                    .map(|since| format!("{}.{}", since * 1_000_000, i64::MAX))
            });
        Some(ActivityRequest {
            since: None,
            before,
            ..base.clone()
        })
    }

    /// Appends an older page. Entries already shown are skipped, so a page boundary that
    /// moved under new entries never shows one twice.
    pub fn extend(&mut self, page: WarActivityPage, widened: bool) {
        let known: std::collections::HashSet<i64> =
            self.entries.iter().map(|entry| entry.id).collect();
        self.entries.extend(
            page.entries
                .into_iter()
                .filter(|entry| !known.contains(&entry.id)),
        );
        self.widened |= widened;
        self.next_before = page.next_before;
        self.ended = self.widened && self.next_before.is_none();
    }

    /// Merges a fresh first page in at the top without disturbing what is loaded below. When
    /// more arrived than one page holds, the fresh page does not reach what is loaded, and
    /// merging would leave a hole Load older could never fill: the list starts over from the
    /// fresh page instead, and says so. So does an empty list that the fresh page fills, which
    /// takes the fresh page's continuation rather than the empty one's end.
    pub fn refresh(&mut self, page: WarActivityPage, since: i64) -> bool {
        let known: std::collections::HashSet<i64> =
            self.entries.iter().map(|entry| entry.id).collect();
        let reaches = page.next_before.is_none()
            || page.entries.iter().any(|entry| known.contains(&entry.id));
        let fills = self.entries.is_empty() && !page.entries.is_empty();
        if !reaches || fills {
            *self = Self::first(page, since, self.whole_week);
            return true;
        }
        let mut fresh: Vec<ActivityEntry> = page
            .entries
            .iter()
            .filter(|entry| !known.contains(&entry.id))
            .cloned()
            .collect();
        // An outcome confirmed since it was loaded replaces the old row in place.
        for entry in &page.entries {
            if let Some(held) = self.entries.iter_mut().find(|held| held.id == entry.id) {
                *held = entry.clone();
            }
        }
        fresh.extend(std::mem::take(&mut self.entries));
        self.entries = fresh;
        false
    }
}

async fn fetch(request: &ActivityRequest) -> Result<WarActivityPage, String> {
    let response = gloo_net::http::Request::get(&request.url())
        .send()
        .await
        .map_err(|error| format!("request failed: {error}"))?;
    if !response.ok() {
        return Err(format!("status {}", response.status()));
    }
    response
        .json::<WarActivityPage>()
        .await
        .map_err(|error| format!("decode failed: {error}"))
}

#[derive(Clone, Copy, PartialEq)]
enum Load {
    Idle,
    Loading,
    Failed,
}

/// What the feed shows in place of its rows, or that it shows rows.
#[derive(Clone, Copy, PartialEq)]
enum Shown {
    Loading,
    Unavailable,
    Nothing,
    Rows,
}

/// Whether the feed is still mounted, and which selection its requests answer. Plain atomics
/// rather than reactive values, so a response arriving after the feed is gone can ask without
/// touching anything the feed owned.
#[derive(Clone)]
struct Requests {
    alive: Arc<AtomicBool>,
    epoch: Arc<AtomicU64>,
}

impl Requests {
    fn new() -> Self {
        Self {
            alive: Arc::new(AtomicBool::new(true)),
            epoch: Arc::new(AtomicU64::new(0)),
        }
    }

    fn asked(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }

    /// Answers to requests made before this are dropped.
    fn supersede(&self) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
    }

    fn close(&self) {
        self.alive.store(false, Ordering::Relaxed);
    }

    /// Whether an answer to a request made at `asked` still belongs to the feed.
    fn current(&self, asked: u64) -> bool {
        self.alive.load(Ordering::Relaxed) && self.epoch.load(Ordering::Relaxed) == asked
    }
}

/// The activity list, loading once it is shown. `territory` narrows it to one territory. It is
/// kept while its disclosure closes, so reopening shows what it held at once; it only checks
/// for newer entries while `visible`, and catches up as soon as it is shown again.
#[component]
pub(crate) fn ActivityFeed(
    territory: Option<Signal<String>>,
    #[prop(into)] visible: Signal<bool>,
) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let ActivityIncludeUnconfirmed(everything) = expect_context();
    let list = RwSignal::new(None::<ActivityList>);
    let load = RwSignal::new(Load::Idle);
    // The rows on show belong to a selection that has since changed; they stay, marked as
    // pending, until the new selection's first page replaces them.
    let stale = RwSignal::new(false);
    let checked_at = RwSignal::new(0_i64);
    let root = NodeRef::<leptos::html::Div>::new();
    // Superseded whenever the selection changes: an answer to an older selection is dropped,
    // and so is any answer once the feed is gone.
    let requests = Requests::new();
    on_cleanup({
        let requests = requests.clone();
        move || requests.close()
    });

    let base = Memo::new(move |_| ActivityRequest {
        since: None,
        before: None,
        territory: territory.map(|territory| territory.get()),
        everything: everything.get(),
    });
    // A territory's list is short and wants its whole week at once; the war panel's starts
    // with the last day.
    let whole_week = territory.is_some();
    let first_request = move || {
        let since = room.now.get_untracked() - RECENT_SECS;
        (
            ActivityRequest {
                since: (!whole_week).then_some(since),
                ..base.get_untracked()
            },
            since,
        )
    };
    let load_first = {
        let requests = requests.clone();
        move || {
            let (request, since) = first_request();
            let requests = requests.clone();
            let asked = requests.asked();
            load.set(Load::Loading);
            spawn_local(async move {
                let result = fetch(&request).await;
                if !requests.current(asked) {
                    return;
                }
                checked_at.set(room.now.get_untracked());
                stale.set(false);
                match result {
                    Ok(page) => {
                        let first = list.with_untracked(Option::is_none);
                        list.set(Some(ActivityList::first(page, since, whole_week)));
                        load.set(Load::Idle);
                        // What the first answer revealed comes into view, as opening did.
                        if first && let Some(root) = root.get_untracked() {
                            crate::ui::reveal_soon(root.into());
                        }
                    }
                    Err(_) => {
                        list.set(None);
                        load.set(Load::Failed);
                    }
                }
            });
        }
    };
    Effect::new({
        let requests = requests.clone();
        let load_first = load_first.clone();
        move || {
            base.track();
            requests.supersede();
            stale.set(list.with_untracked(Option::is_some));
            load_first();
        }
    });
    // Newer entries are picked up now and then rather than streamed, and only while shown.
    let refresh = {
        let requests = requests.clone();
        move || {
            if load.get_untracked() == Load::Loading {
                return;
            }
            // Without a first page there is nothing to refresh. One that failed is asked for
            // again, so a passing outage does not leave a kept feed unavailable.
            if list.with_untracked(Option::is_none) {
                if load.get_untracked() == Load::Failed {
                    load_first();
                }
                return;
            }
            checked_at.set(room.now.get_untracked());
            let (request, since) = first_request();
            let requests = requests.clone();
            let asked = requests.asked();
            spawn_local(async move {
                if let Ok(page) = fetch(&request).await
                    && requests.current(asked)
                {
                    let started_over = list
                        .try_update(|list| {
                            list.as_mut().is_some_and(|list| list.refresh(page, since))
                        })
                        .unwrap_or(false);
                    // An older page still on its way belongs to the list that was replaced.
                    if started_over {
                        requests.supersede();
                        load.set(Load::Idle);
                    }
                }
            });
        }
    };
    Effect::new({
        let refresh = refresh.clone();
        move || {
            let refresh = refresh.clone();
            let handle = set_interval_with_handle(
                move || {
                    if visible.get_untracked() {
                        refresh();
                    }
                },
                std::time::Duration::from_millis(REFRESH_MS),
            );
            on_cleanup(move || {
                if let Ok(handle) = handle {
                    handle.clear();
                }
            });
        }
    });
    Effect::new(move |was_visible: Option<bool>| {
        let shown = visible.get();
        let due = room.now.get_untracked() - checked_at.get_untracked() >= REFRESH_SECS;
        let unavailable =
            load.get_untracked() == Load::Failed && list.with_untracked(Option::is_none);
        if shown && was_visible == Some(false) && (due || unavailable) {
            refresh();
        }
        shown
    });
    let load_older = move |_| {
        if load.get_untracked() == Load::Loading {
            return;
        }
        let Some(request) = list.with_untracked(|list| {
            list.as_ref()
                .and_then(|list| list.older(&base.get_untracked()))
        }) else {
            return;
        };
        let widened = request.since.is_none();
        let requests = requests.clone();
        let asked = requests.asked();
        load.set(Load::Loading);
        spawn_local(async move {
            let result = fetch(&request).await;
            if !requests.current(asked) {
                return;
            }
            match result {
                Ok(page) => {
                    list.update(|list| {
                        if let Some(list) = list.as_mut() {
                            list.extend(page, widened);
                        }
                    });
                    load.set(Load::Idle);
                }
                Err(_) => load.set(Load::Failed),
            }
        });
    };
    // Only a change between these rebuilds the list; rows themselves are keyed by entry, so
    // pages and refreshes keep a retained row's state, its focus and its open details.
    let shown = Memo::new(move |_| {
        list.with(|list| match list {
            None if load.get() == Load::Failed => Shown::Unavailable,
            None => Shown::Loading,
            Some(list) if list.entries.is_empty() && list.ended => Shown::Nothing,
            Some(_) => Shown::Rows,
        })
    });
    let has_entries = Memo::new(move |_| {
        list.with(|list| list.as_ref().is_some_and(|list| !list.entries.is_empty()))
    });
    let can_load_older = Memo::new(move |_| {
        !stale.get()
            && list.with(|list| {
                list.as_ref()
                    .is_some_and(|list| list.older(&base.get()).is_some())
            })
    });
    let ended = Memo::new(move |_| list.with(|list| list.as_ref().is_some_and(|list| list.ended)));
    let busy = move || (load.get() == Load::Loading).to_string();

    view! {
        <div class="activity" node_ref=root aria-busy=busy class:activity-stale=move || stale.get()>
            {move || match shown.get() {
                Shown::Unavailable => view! { <p class="activity-note" role="status">"Recent activity is unavailable"</p> }.into_any(),
                Shown::Loading => view! { <p class="activity-note activity-pending" role="status">"Loading"</p> }.into_any(),
                Shown::Nothing => view! { <p class="activity-note">"Nothing in the last 7 days"</p> }.into_any(),
                Shown::Rows => view! {
                    <ol class="activity-list">
                        <For
                            each=move || list.with(|list| list.as_ref().map(|list| list.entries.clone()).unwrap_or_default())
                            key=|entry| entry.id
                            children=move |entry| {
                                let id = entry.id;
                                // The row follows its entry's updates without being rebuilt.
                                let entry = Memo::new(move |previous: Option<&ActivityEntry>| {
                                    list.with(|list| {
                                        list.as_ref()
                                            .and_then(|list| list.entries.iter().find(|held| held.id == id))
                                            .cloned()
                                    })
                                    .or_else(|| previous.cloned())
                                    .unwrap_or_else(|| entry.clone())
                                });
                                view! { <ActivityRow entry /> }
                            }
                        />
                    </ol>
                }.into_any(),
            }}
            <Show when=move || can_load_older.get()>
                // Not disabled while loading, which would drop keyboard focus from it.
                <button type="button" class="ui-button activity-older" aria-disabled=busy on:click=load_older.clone()>
                    "Load older"
                </button>
            </Show>
            <Show when=move || ended.get() && has_entries.get()>
                <p class="activity-note">"Activity is kept for 7 days"</p>
            </Show>
            <Show when=move || load.get() == Load::Failed && list.with(Option::is_some)>
                <p class="activity-note" role="status">"Could not load more"</p>
            </Show>
        </div>
    }
}

#[component]
fn ActivityRow(entry: Memo<ActivityEntry>) -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let focus_map = MapFocus::from_context();
    let open = RwSignal::new(false);
    // An entry keeps its territory and time; its outcome and snapshot can still be updated.
    let (territory, at) =
        entry.with_untracked(|entry| (entry.territory.clone(), entry.occurred_at));
    let outcome = move || entry.with(|entry| entry.outcome);
    let difficulty = move || entry.with(|entry| entry.difficulty.clone());
    let detail_label = move || {
        entry.with(|entry| {
            format!(
                "Details for {} {}",
                entry.territory,
                entry.outcome.label().to_lowercase()
            )
        })
    };
    let has_detail = Memo::new(move |_| {
        entry.with(|entry| {
            entry.queue.is_some() || entry.war.is_some() || entry.owner_change.is_some()
        })
    });
    view! {
        <li class="activity-row" data-outcome=move || outcome_slug(outcome())>
            <div class="activity-head">
                <span
                    class="war-difficulty"
                    style:background=move || difficulty_color(difficulty().as_deref())
                    title=move || difficulty_label(difficulty().as_deref()).unwrap_or("Difficulty unknown")
                />
                <button type="button" class="war-link activity-territory" title=format!("Show {territory} on the map") on:click={
                    let territory = territory.clone();
                    move |_| focus_map.territory(&territory)
                }>
                    {territory.clone()}
                </button>
                <span class="activity-outcome">{move || outcome().label()}</span>
                <time class="activity-when" title=local_time(at)>{move || ago(room.now.get() - at)}</time>
                <Show when=move || has_detail.get()>
                    <button
                        type="button"
                        class="war-front-expand"
                        aria-label=detail_label
                        aria-expanded=move || open.get().to_string()
                        on:click=move |event| {
                            open.update(|open| *open = !*open);
                            if open.get_untracked()
                                && let Some(row) = event_target::<web_sys::Element>(&event).closest("li").ok().flatten()
                            {
                                crate::ui::reveal_soon(row);
                            }
                        }
                    />
                </Show>
            </div>
            <Show when=move || open.get() && has_detail.get()>
                {move || view! { <ActivityDetail entry=entry.get() /> }}
            </Show>
        </li>
    }
}

/// What was recorded about the attempt, as it was then. Absent facts are left out.
#[component]
fn ActivityDetail(entry: ActivityEntry) -> impl IntoView {
    let mut facts: Vec<(&'static str, String)> = Vec::new();
    if let Some(change) = &entry.owner_change {
        let guild = |guild: &sequoia_shared::war_activity::ActivityGuild| match (
            &guild.prefix,
            &guild.name,
        ) {
            (Some(prefix), Some(name)) => format!("[{prefix}] {name}"),
            (None, Some(name)) => name.clone(),
            (Some(prefix), None) => format!("[{prefix}]"),
            (None, None) => "Unknown guild".to_string(),
        };
        match entry.outcome {
            ActivityOutcome::Lost => facts.push(("Taken by", guild(&change.to))),
            _ => facts.push(("Taken from", guild(&change.from))),
        }
        facts.push(("Changed hands", stamp(change.changed_at, change.exact)));
    }
    if let Some(queue) = &entry.queue {
        let when = queue.queued_at.map(local_time);
        let by = queue
            .queuer
            .clone()
            .unwrap_or_else(|| "queuer unknown".to_string());
        facts.push((
            "Queued",
            match when {
                Some(when) => format!("{when}, {by}"),
                None => by,
            },
        ));
        if let Some(seconds) = queue.seconds {
            facts.push(("Queue timer", format_clock(seconds)));
        }
    }
    if entry.attempt_ambiguous {
        facts.push(("Attack", "Several fit; none attached".to_string()));
    }
    if let Some(war) = &entry.war {
        // Each time is only what it says: a start only when a queue scheduled it, and an end
        // only when a member reported one.
        match (war.starts_at, war.start_source) {
            (Some(at), Some(StartSource::Queue)) => facts.push(("War start", local_time(at))),
            (Some(at), Some(StartSource::Estimate)) => {
                facts.push(("War start", format!("about {}", local_time(at))))
            }
            _ => {}
        }
        if let Some(at) = war.first_reported_at {
            facts.push(("First report", local_time(at)));
        }
        match (war.ended_at, war.last_reported_at) {
            (Some(at), _) => facts.push(("Reported end", local_time(at))),
            (None, Some(at)) => facts.push(("Last report", local_time(at))),
            (None, None) => {}
        }
        if let Some(fought) = war.fought_secs() {
            facts.push(("Fought", format_clock(fought)));
        }
        if war.interrupted {
            facts.push(("Reports", "stopped and resumed".to_string()));
        }
        if war.after_gap {
            facts.push(("Attack", "May continue the one before".to_string()));
        }
        if let Some(tower) = &war.tower {
            let mut line = Vec::new();
            if let Some(ehp) = tower.ehp {
                line.push(format!("{} EHP", format_stat(ehp as f64)));
            }
            if let Some(dps) = tower.dps {
                line.push(format!("{} DPS", format_stat(dps as f64)));
            }
            if !tower.from_start
                && let Some(health) = tower.first_health
            {
                line.push(format!("first seen at {:.0}%", health * 100.0));
            }
            if !line.is_empty() {
                facts.push(("Tower", line.join(", ")));
            }
        }
        if !war.participants.is_empty() {
            facts.push(("Reported by", war.participants.join(", ")));
        }
    }
    view! {
        <dl class="war-front-detail activity-detail">
            {facts.into_iter().map(|(label, value)| view! { <div><dt>{label}</dt><dd>{value}</dd></div> }).collect_view()}
        </dl>
    }
}

/// A time, marked as approximate when only the moment it was first seen is known.
fn stamp(at: i64, exact: bool) -> String {
    if exact {
        local_time(at)
    } else {
        format!("by {}", local_time(at))
    }
}

fn outcome_slug(outcome: ActivityOutcome) -> &'static str {
    match outcome {
        ActivityOutcome::Captured => "captured",
        ActivityOutcome::Lost => "lost",
        ActivityOutcome::Cancelled => "cancelled",
        ActivityOutcome::Ended => "ended",
    }
}

/// The war panel's Recent activity, closed until a member opens it.
#[component]
pub(crate) fn RecentActivity() -> impl IntoView {
    view! {
        <section class="war-section war-section-activity" aria-label="Recent activity">
            <ActivityDisclosure />
        </section>
    }
}

/// Recent activity behind a disclosure. Nothing loads until it is first opened; after that
/// the feed is kept, so closing and reopening shows what it held without a reload.
#[component]
pub(crate) fn ActivityDisclosure(
    #[prop(optional, into)] territory: Option<Signal<String>>,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let opened =
        Memo::new(move |opened: Option<&bool>| opened.copied().unwrap_or(false) || open.get());
    view! {
        <Disclosure open summary="Recent activity" class="activity-disclosure">
            <Show when=move || opened.get()>
                <ActivityFeed territory=territory visible=open />
            </Show>
        </Disclosure>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: i64, at: i64) -> ActivityEntry {
        ActivityEntry {
            id,
            territory: format!("Territory {id}"),
            outcome: ActivityOutcome::Lost,
            occurred_at: at,
            occurred_at_exact: true,
            difficulty: None,
            queue: None,
            war: None,
            owner_change: None,
            attempt_ambiguous: false,
            cursor: format!("{}.{id}", at * 1_000_000),
        }
    }

    fn page(entries: Vec<ActivityEntry>, next: Option<&str>) -> WarActivityPage {
        WarActivityPage {
            server_time: 1_000_000,
            entries,
            next_before: next.map(str::to_string),
            kept_since: 400_000,
        }
    }

    fn base() -> ActivityRequest {
        ActivityRequest {
            since: None,
            before: None,
            territory: None,
            everything: false,
        }
    }

    #[test]
    fn a_request_names_only_what_it_asks_for() {
        assert_eq!(base().url(), "/api/war-activity?limit=25");
        let request = ActivityRequest {
            since: Some(900),
            before: Some("1000.5".to_string()),
            territory: Some("Path to the Grootslangs".to_string()),
            everything: true,
        };
        assert_eq!(
            request.url(),
            "/api/war-activity?limit=25&since=900&before=1000.5&territory=Path%20to%20the%20Grootslangs&scope=all"
        );
    }

    #[test]
    fn load_older_finishes_the_day_then_opens_the_week_then_ends() {
        let since = 913_600;
        let mut list = ActivityList::first(
            page(
                vec![entry(9, 990_000), entry(8, 980_000)],
                Some("980000000000.8"),
            ),
            since,
            false,
        );

        let more_today = list.older(&base()).expect("more of the day");
        assert_eq!(
            (more_today.since, more_today.before.as_deref()),
            (Some(since), Some("980000000000.8"))
        );
        list.extend(page(vec![entry(7, 950_000)], None), false);

        let week = list.older(&base()).expect("the rest of the week");
        assert_eq!(
            (week.since, week.before.as_deref()),
            (None, Some("950000000000.7"))
        );
        list.extend(page(vec![entry(6, 500_000)], None), true);

        assert!(list.ended);
        assert_eq!(list.older(&base()), None);
        assert_eq!(
            list.entries
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            [9, 8, 7, 6]
        );
    }

    #[test]
    fn an_empty_day_continues_from_the_days_edge_in_microseconds() {
        let list = ActivityList::first(page(Vec::new(), None), 913_600, false);
        let week = list.older(&base()).expect("the week");
        assert_eq!(
            week.before.as_deref(),
            Some(format!("913600000000.{}", i64::MAX).as_str())
        );
    }

    #[test]
    fn a_whole_week_list_ends_with_its_only_page() {
        let list = ActivityList::first(page(vec![entry(3, 990_000)], None), 913_600, true);
        assert!(list.ended);
        assert_eq!(list.older(&base()), None);
    }

    #[test]
    fn an_empty_territory_list_takes_the_continuation_of_a_busy_refresh() {
        // The territory had nothing; then 30 entries arrived and the refresh holds the newest 25.
        let mut list = ActivityList::first(page(Vec::new(), None), 813_600, true);
        assert!(list.ended);
        let head: Vec<ActivityEntry> = (6..=30)
            .rev()
            .map(|id| entry(id, 900_000 + id * 100))
            .collect();
        assert!(list.refresh(page(head, Some("900600000000.6")), 913_600));

        assert!(!list.ended);
        let older = list.older(&base()).expect("Load older");
        assert_eq!(
            (older.since, older.before.as_deref()),
            (None, Some("900600000000.6"))
        );
        let rest: Vec<ActivityEntry> = (1..=5)
            .rev()
            .map(|id| entry(id, 900_000 + id * 100))
            .collect();
        list.extend(page(rest, None), true);

        assert!(list.ended);
        assert_eq!(
            list.entries
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            (1..=30).rev().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_refresh_that_is_still_empty_keeps_a_finished_list_finished() {
        let mut list = ActivityList::first(page(Vec::new(), None), 813_600, false);
        list.extend(page(Vec::new(), None), true);
        assert!(list.ended);
        assert!(!list.refresh(page(Vec::new(), None), 913_600));
        assert!(list.ended);
        assert_eq!(list.older(&base()), None);
    }

    #[test]
    fn pages_and_refreshes_never_show_an_entry_twice() {
        let mut list = ActivityList::first(
            page(
                vec![entry(9, 990_000), entry(8, 980_000)],
                Some("980000000000.8"),
            ),
            913_600,
            false,
        );
        list.extend(
            page(vec![entry(8, 980_000), entry(7, 970_000)], None),
            false,
        );
        let mut confirmed = entry(9, 990_000);
        confirmed.outcome = ActivityOutcome::Captured;
        assert!(!list.refresh(
            page(vec![entry(10, 995_000), confirmed], Some("990000000000.9")),
            913_600,
        ));

        assert_eq!(
            list.entries
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            [10, 9, 8, 7]
        );
        assert_eq!(list.entries[1].outcome, ActivityOutcome::Captured);
    }

    #[test]
    fn more_arrivals_than_a_page_start_the_list_over_instead_of_leaving_a_hole() {
        // Held [10, 9]; then 40 down to 11 arrive and the refreshed page holds only 40 to 16.
        let mut list = ActivityList::first(
            page(vec![entry(10, 900_100), entry(9, 900_090)], None),
            813_600,
            false,
        );
        list.extend(page(vec![entry(8, 800_000)], None), true);
        let head: Vec<ActivityEntry> = (16..=40)
            .rev()
            .map(|id| entry(id, 900_000 + id * 100))
            .collect();
        assert!(list.refresh(page(head, Some("901600000000.16")), 913_600));

        assert_eq!(list.entries.len(), 25);
        assert_eq!(list.entries.last().map(|entry| entry.id), Some(16));
        assert!(!list.ended);
        // Load older carries on from 16, so 15 to 11 and then the held rows come back in order.
        let older = list.older(&base()).expect("more");
        assert_eq!(older.before.as_deref(), Some("901600000000.16"));
        assert_eq!(older.since, Some(913_600));
    }
}
