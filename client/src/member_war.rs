//! Sequoia members' war standing and records, opened in place from member rows.
//!
//! [`MemberWar`] keeps what has been fetched for the session: each member's standing, each
//! member's record per period, and the roster's war roles. Every answer is stored under the key
//! it was asked for, so a late answer can only ever fill its own slot. Expanded rows and the
//! chosen period live here too, so they survive a panel closing and reopening.
//!
//! Standing (rank, roles) and records (history) are separate requests. Changing the period asks
//! only for a record, so it cannot change what is shown as current.

use std::collections::{HashMap, HashSet};

use leptos::prelude::*;
use sequoia_shared::member_war::{
    MemberRosterRoles, MemberStanding, MemberWarPeriod, PeriodKind, PointsStatus, RankStanding,
    WarRole, WarRoleKind, WarTotals,
};
use sequoia_shared::war_board::RosterMember;

use crate::auth::nmsr_face;
use crate::format::since;
use crate::navbar::CurrentViewer;
use crate::war_room::{ClassIcon, MapFocus, WarRoom};

/// The window a record is asked for. Custom days are `YYYY-MM-DD`, both inclusive.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Period {
    Week,
    Month,
    Season,
    Custom { from: String, to: String },
}

impl Period {
    fn query(&self) -> String {
        match self {
            Self::Week => "period=7d".into(),
            Self::Month => "period=30d".into(),
            Self::Season => "period=season".into(),
            Self::Custom { from, to } => format!("period=custom&from={from}&to={to}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Load<T> {
    Pending,
    Ready(T),
    Failed(Failure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The server or an upstream could not answer; worth retrying.
    Unavailable,
    /// No published season window is running.
    NoSeason,
    /// Not on Sequoia's current roster.
    NotOnRoster,
    /// The viewer is not a member.
    Denied,
}

impl Failure {
    fn of(status: u16) -> Self {
        match status {
            403 => Self::Denied,
            404 => Self::NotOnRoster,
            409 => Self::NoSeason,
            _ => Self::Unavailable,
        }
    }
}

impl<T> Load<T> {
    fn project<U: Clone>(&self, pick: impl Fn(&T) -> Option<&U>) -> Option<Load<U>> {
        Some(match self {
            Self::Pending => Load::Pending,
            Self::Ready(value) => Load::Ready(pick(value)?.clone()),
            Self::Failed(failure) => Load::Failed(*failure),
        })
    }
}

/// How long current standing (ranks, roles) is shown before the next look asks again.
const CURRENT_REFRESH_MS: f64 = 60_000.0;
/// The same for a record over a period.
const RECORD_REFRESH_MS: f64 = 5.0 * 60_000.0;

/// One thing the store asks the server for. Usernames are lowercased.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Ask {
    Roster,
    Standing(String),
    Record(String, Period),
}

#[derive(Clone, Debug, PartialEq)]
enum Answer {
    Roster(MemberRosterRoles),
    Standing(MemberStanding),
    Record(MemberWarPeriod),
}

impl Ask {
    /// The member this is about, for answers that concern one member only.
    fn member(&self) -> Option<&str> {
        match self {
            Self::Roster => None,
            Self::Standing(user) | Self::Record(user, _) => Some(user),
        }
    }

    fn refresh_after_ms(&self) -> f64 {
        match self {
            Self::Roster | Self::Standing(_) => CURRENT_REFRESH_MS,
            Self::Record(..) => RECORD_REFRESH_MS,
        }
    }

    async fn fetch(&self) -> Load<Answer> {
        let name = |name: &str| crate::encode_uri_component(name);
        match self {
            Self::Roster => answer(get_json("/api/members").await, Answer::Roster),
            Self::Standing(user) => answer(
                get_json(&format!("/api/members/{}", name(user))).await,
                Answer::Standing,
            ),
            Self::Record(user, period) => answer(
                get_json(&format!(
                    "/api/members/{}/war?{}",
                    name(user),
                    period.query()
                ))
                .await,
                Answer::Record,
            ),
        }
    }
}

fn answer<T>(load: Load<T>, wrap: fn(T) -> Answer) -> Load<Answer> {
    match load {
        Load::Pending => Load::Pending,
        Load::Ready(value) => Load::Ready(wrap(value)),
        Load::Failed(failure) => Load::Failed(failure),
    }
}

#[derive(Clone)]
struct Fetched {
    load: Load<Answer>,
    at: f64,
    /// A refresh of this answer failed transiently; it is still shown, marked as not current.
    outdated: bool,
}

/// What to do about an entry when it is looked at.
fn is_due(entry: Option<&Fetched>, refresh_after_ms: f64, now: f64) -> bool {
    match entry {
        None => true,
        Some(Fetched {
            load: Load::Pending,
            ..
        }) => false,
        // A refusal or outage is asked again on the next look; a firm answer ages like data.
        Some(Fetched {
            load: Load::Failed(Failure::Unavailable | Failure::Denied),
            ..
        }) => true,
        Some(Fetched { outdated: true, .. }) => true,
        Some(Fetched { at, .. }) => now - at > refresh_after_ms,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct MemberWar {
    expanded: RwSignal<HashSet<String>>,
    pub period: RwSignal<Period>,
    /// The custom days as typed, shared by every open row so they always show the same window.
    custom_from: RwSignal<String>,
    custom_to: RwSignal<String>,
    entries: RwSignal<HashMap<Ask, Fetched>>,
    /// Requests on their way; at most one per [`Ask`], whoever asks.
    in_flight: StoredValue<HashSet<Ask>>,
    /// Bumped when the viewer changes or is refused, so answers asked before are dropped.
    generation: StoredValue<u64>,
    /// Bumped per member when they leave the roster, so their late answers are dropped.
    member_epochs: StoredValue<HashMap<String, u64>>,
}

fn key(username: &str) -> String {
    username.to_ascii_lowercase()
}

fn now_ms() -> f64 {
    js_sys::Date::now()
}

async fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> Load<T> {
    match gloo_net::http::Request::get(url).send().await {
        Ok(response) if response.ok() => match response.json::<T>().await {
            Ok(value) => Load::Ready(value),
            Err(_) => Load::Failed(Failure::Unavailable),
        },
        Ok(response) => Load::Failed(Failure::of(response.status())),
        Err(_) => Load::Failed(Failure::Unavailable),
    }
}

impl MemberWar {
    /// Creates the store. Any change to the viewer (another account, a selected Minecraft
    /// account, gaining or losing membership) forgets everything fetched and every open row.
    pub fn new() -> Self {
        let this = Self {
            expanded: RwSignal::new(HashSet::new()),
            period: RwSignal::new(Period::Week),
            custom_from: RwSignal::new(
                (chrono::Utc::now().date_naive() - chrono::Duration::days(13)).to_string(),
            ),
            custom_to: RwSignal::new(chrono::Utc::now().date_naive().to_string()),
            entries: RwSignal::new(HashMap::new()),
            in_flight: StoredValue::new(HashSet::new()),
            generation: StoredValue::new(0),
            member_epochs: StoredValue::new(HashMap::new()),
        };
        let CurrentViewer(viewer) = expect_context();
        Effect::new(move |previous: Option<Option<crate::auth::Viewer>>| {
            let current = viewer.get();
            if previous.is_some_and(|previous| previous != current) {
                this.generation.update_value(|generation| *generation += 1);
                this.in_flight.update_value(HashSet::clear);
                this.entries.set(HashMap::new());
                this.expanded.set(HashSet::new());
            }
            current
        });
        this
    }

    pub fn is_expanded(&self, username: &str) -> bool {
        self.expanded.with(|open| open.contains(&key(username)))
    }

    pub fn set_expanded(&self, username: &str, open: bool) {
        self.expanded.update(|expanded| {
            if open {
                expanded.insert(key(username));
            } else {
                expanded.remove(&key(username));
            }
        });
    }

    fn load(&self, ask: &Ask) -> Option<Load<Answer>> {
        self.entries
            .with(|entries| entries.get(ask).map(|fetched| fetched.load.clone()))
    }

    /// Asks unless the answer is fresh or already on its way. An answer being refreshed stays
    /// shown until its replacement arrives, and a failed refresh keeps it.
    fn ensure(&self, ask: Ask) {
        if self.in_flight.with_value(|set| set.contains(&ask)) {
            return;
        }
        let (due, shown) = self.entries.with_untracked(|entries| {
            let entry = entries.get(&ask);
            (
                is_due(entry, ask.refresh_after_ms(), now_ms()),
                entry.is_some_and(|fetched| matches!(fetched.load, Load::Ready(_))),
            )
        });
        if !due {
            return;
        }
        self.in_flight.update_value(|set| {
            set.insert(ask.clone());
        });
        if !shown {
            self.entries.update(|entries| {
                entries.insert(
                    ask.clone(),
                    Fetched {
                        load: Load::Pending,
                        at: now_ms(),
                        outdated: false,
                    },
                );
            });
        }
        let generation = self.generation.get_value();
        let epoch = self.member_epoch(ask.member());
        let this = *self;
        wasm_bindgen_futures::spawn_local(async move {
            let load = ask.fetch().await;
            if this.generation.try_get_value() != Some(generation) {
                return;
            }
            this.in_flight.update_value(|set| {
                set.remove(&ask);
            });
            if this.member_epoch(ask.member()) != epoch {
                return;
            }
            match load {
                Load::Failed(failure @ Failure::Denied) => return this.answer_everything(failure),
                Load::Failed(failure @ Failure::NotOnRoster) => {
                    if let Some(member) = ask.member() {
                        return this.answer_member(member, failure);
                    }
                }
                _ => {}
            }
            this.entries.update(|entries| {
                // An outage keeps what was shown, marked as not current. A refusal, a name gone
                // from the roster or a season that ended is an answer and replaces it.
                if shown
                    && matches!(load, Load::Failed(Failure::Unavailable))
                    && let Some(previous) = entries.get_mut(&ask)
                {
                    previous.outdated = true;
                    return;
                }
                entries.insert(
                    ask,
                    Fetched {
                        load,
                        at: now_ms(),
                        outdated: false,
                    },
                );
            });
        });
    }

    fn outdated(&self, ask: &Ask) -> bool {
        self.entries
            .with(|entries| entries.get(ask).is_some_and(|fetched| fetched.outdated))
    }

    fn member_epoch(&self, member: Option<&str>) -> u64 {
        member.map_or(0, |member| {
            self.member_epochs
                .with_value(|epochs| epochs.get(member).copied().unwrap_or(0))
        })
    }

    /// A refusal is about the viewer, not one request: every answer held for them, any member,
    /// any period, becomes that refusal, and answers still on their way are dropped.
    fn answer_everything(&self, failure: Failure) {
        self.generation.update_value(|generation| *generation += 1);
        self.in_flight.update_value(HashSet::clear);
        self.entries.update(|entries| {
            for fetched in entries.values_mut() {
                *fetched = Fetched {
                    load: Load::Failed(failure),
                    at: now_ms(),
                    outdated: false,
                };
            }
        });
    }

    /// A member gone from the roster: every answer about them, any period, becomes that, and
    /// their answers still on their way are dropped.
    fn answer_member(&self, member: &str, failure: Failure) {
        self.member_epochs.update_value(|epochs| {
            *epochs.entry(member.to_string()).or_default() += 1;
        });
        self.in_flight
            .update_value(|set| set.retain(|ask| ask.member() != Some(member)));
        self.entries.update(|entries| {
            for (ask, fetched) in entries.iter_mut() {
                if ask.member() == Some(member) {
                    *fetched = Fetched {
                        load: Load::Failed(failure),
                        at: now_ms(),
                        outdated: false,
                    };
                }
            }
        });
    }

    fn retry(&self, ask: Ask) {
        self.entries.update(|entries| {
            entries.remove(&ask);
        });
        self.ensure(ask);
    }

    pub fn standing(&self, username: &str) -> Option<Load<MemberStanding>> {
        self.load(&Ask::Standing(key(username)))?
            .project(|answer| match answer {
                Answer::Standing(standing) => Some(standing),
                _ => None,
            })
    }

    pub fn standing_outdated(&self, username: &str) -> bool {
        self.outdated(&Ask::Standing(key(username)))
    }

    pub fn ensure_standing(&self, username: &str) {
        self.ensure(Ask::Standing(key(username)));
    }

    pub fn retry_standing(&self, username: &str) {
        self.retry(Ask::Standing(key(username)));
    }

    pub fn record(&self, username: &str, period: &Period) -> Option<Load<MemberWarPeriod>> {
        self.load(&Ask::Record(key(username), period.clone()))?
            .project(|answer| match answer {
                Answer::Record(record) => Some(record),
                _ => None,
            })
    }

    pub fn record_outdated(&self, username: &str, period: &Period) -> bool {
        self.outdated(&Ask::Record(key(username), period.clone()))
    }

    pub fn ensure_record(&self, username: &str, period: &Period) {
        self.ensure(Ask::Record(key(username), period.clone()));
    }

    pub fn retry_record(&self, username: &str, period: &Period) {
        self.retry(Ask::Record(key(username), period.clone()));
    }

    pub fn ensure_roster(&self) {
        self.ensure(Ask::Roster);
    }

    /// The highest insignia on the roster feed, for a compact row.
    pub fn roster_insignia(&self, username: &str) -> Option<WarRole> {
        let Some(Load::Ready(Answer::Roster(roster))) = self.load(&Ask::Roster) else {
            return None;
        };
        roster
            .roles
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(username))?
            .1
            .iter()
            .find(|role| role.kind == WarRoleKind::Insignia)
            .cloned()
    }
}

/// A compact online-member row whose detail opens in place.
#[component]
pub(crate) fn MemberRow(username: String, rank_label: String, server: String) -> impl IntoView {
    let war = expect_context::<MemberWar>();
    let room = expect_context::<WarRoom>();
    let focus_map = MapFocus::from_context();
    let row_ref = NodeRef::<leptos::html::Li>::new();
    let toggle_ref = NodeRef::<leptos::html::Button>::new();
    let open = {
        let username = username.clone();
        Memo::new(move |_| war.is_expanded(&username))
    };
    let reporting = {
        let username = username.clone();
        Memo::new(move |_| room.member(&username))
    };
    let insignia = {
        let username = username.clone();
        Memo::new(move |_| war.roster_insignia(&username))
    };
    let minute = minute_clock();
    Effect::new(move || {
        let _minute = minute.get();
        war.ensure_roster();
    });
    let detail_id = format!("member-war-{}", key(&username));
    let toggle = {
        let username = username.clone();
        move |_| {
            let opening = !war.is_expanded(&username);
            war.set_expanded(&username, opening);
            if opening {
                // The whole row, so the member's own line stays in view above a long record.
                request_animation_frame(move || {
                    if let Some(row) = row_ref.get_untracked() {
                        crate::ui::reveal(&row);
                    }
                });
            }
        }
    };
    let collapse_on_escape = {
        let username = username.clone();
        move |event: web_sys::KeyboardEvent| {
            if event.key() == "Escape" {
                event.prevent_default();
                event.stop_propagation();
                war.set_expanded(&username, false);
                if let Some(toggle) = toggle_ref.get_untracked() {
                    let _ = toggle.focus();
                }
            }
        }
    };
    let face = nmsr_face(&username, 32);
    let name = username.clone();
    let accessible = format!("{username}, {rank_label}");

    view! {
        <li class="mrow" class:mrow-open=move || open.get() node_ref=row_ref on:keydown=collapse_on_escape>
            <div class="mrow-line">
                <button
                    type="button"
                    class="mrow-toggle"
                    aria-expanded=move || open.get().to_string()
                    aria-controls=detail_id.clone()
                    aria-label=accessible
                    node_ref=toggle_ref
                    on:click=toggle
                >
                    <img class="mrow-face" src=face alt="" width="16" height="16" loading="lazy" />
                    <span class="mrow-name">{name}</span>
                    {move || insignia.get().map(|role| view! { <RoleArt role /> })}
                    <span class="mrow-rank">{rank_label}</span>
                    {move || reporting.get().map(|member| view! { <ClassIcon class=member.class /> })}
                    <span class="mrow-chevron" aria-hidden="true" />
                </button>
                <MemberPlace member=reporting server focus_map />
            </div>
            <Show when=move || open.get()>
                <div class="mrow-detail" id=detail_id.clone() >
                    <MemberDetail username=username.clone() />
                </div>
            </Show>
        </li>
    }
}

/// Where a member is: a territory or position to look at when the war feed places them, their
/// world server otherwise.
#[component]
fn MemberPlace(
    member: Memo<Option<RosterMember>>,
    server: String,
    focus_map: MapFocus,
) -> impl IntoView {
    move || match member.get() {
        Some(member) => {
            let warring = member.is_warring();
            let place = crate::war_room::whereabouts(&member);
            let target = member
                .territory()
                .map(|territory| LookAt::Territory(territory.to_string()))
                .or_else(|| member.position().map(LookAt::Position));
            match target {
                Some(target) => {
                    let title = format!("Show {place} on the map");
                    view! {
                        <button
                            type="button"
                            class="mrow-place mrow-place-go"
                            class:mrow-warring=warring
                            title=title.clone()
                            aria-label=title
                            on:click=move |_| match &target {
                                LookAt::Territory(name) => focus_map.look_at_territory(name),
                                LookAt::Position(position) => focus_map.position(*position),
                            }
                        >
                            {place}
                        </button>
                    }
                    .into_any()
                }
                None => view! { <span class="mrow-place">{place}</span> }.into_any(),
            }
        }
        None => view! { <span class="mrow-place">{server.clone()}</span> }.into_any(),
    }
}

enum LookAt {
    Territory(String),
    Position((f64, f64)),
}

#[component]
fn RoleArt(role: WarRole) -> impl IntoView {
    role.art_url.map(|url| {
        view! {
            <img class="mrow-art" src=url alt=role.label.clone() title=role.label width="14" height="14" />
        }
    })
}

/// A member's war roles. Absent roles and a missing profile show nothing; only a sync that is
/// behind or a feed that could not be asked is said out loud.
#[component]
fn WarRoles(username: String) -> impl IntoView {
    let war = expect_context::<MemberWar>();
    let standing = {
        let username = username.clone();
        Memo::new(move |_| war.standing(&username))
    };
    let outdated = {
        let username = username.clone();
        Memo::new(move |_| war.standing_outdated(&username))
    };
    let refresh = {
        let username = username.clone();
        move |_| war.ensure_standing(&username)
    };
    let unavailable = move |username: String| {
        view! {
            <p class="mwar-roles mwar-quiet">
                "Ranks unavailable "
                <button type="button" class="ui-text-action" on:click=move |_| war.retry_standing(&username)>"Retry"</button>
            </p>
        }
        .into_any()
    };
    move || {
        let note = outdated.get().then(|| {
            view! {
                <p class="mwar-quiet mwar-outdated">
                    "Ranks could not refresh "
                    <button type="button" class="ui-text-action" on:click=refresh.clone()>"Retry"</button>
                </p>
            }
        });
        let roles = match standing.get() {
        Some(Load::Failed(Failure::Unavailable))
        | Some(Load::Ready(MemberStanding {
            ranks: RankStanding::Unavailable,
            ..
        })) => Some(unavailable(username.clone())),
        Some(Load::Ready(MemberStanding {
            ranks: RankStanding::Synced { stale, roles },
            ..
        })) if stale || !roles.is_empty() => Some(
            view! {
                <ul class="mwar-roles" aria-label="War ranks">
                    {roles.into_iter().map(|role| view! { <RoleChip role /> }).collect_view()}
                    {stale.then(|| view! { <li class="mwar-quiet">"Discord rank sync is behind"</li> })}
                </ul>
            }
            .into_any(),
        ),
        _ => None,
    };
        (roles, note)
    }
}

#[component]
fn RoleChip(role: WarRole) -> impl IntoView {
    let color = role.color.clone();
    let kind = match role.kind {
        WarRoleKind::Insignia => "insignia",
        WarRoleKind::WarHonour => "honour",
        WarRoleKind::Military => "military",
        WarRoleKind::MilitarySpecialty => "specialty",
    };
    view! {
        <li class="mwar-role" data-kind=kind style:--role-color=color>
            {role.art_url.clone().map(|url| view! { <img class="mrow-art" src=url alt="" width="14" height="14" /> })}
            <span>{role.label}</span>
        </li>
    }
}

/// The map's clock in whole minutes: mounted views look again each minute, and the store's
/// freshness rules decide whether that look costs a request. Read it with `get`: tracking a
/// memo without evaluating it never subscribes the memo to the clock.
fn minute_clock() -> Memo<i64> {
    let tick: RwSignal<i64> = expect_context();
    Memo::new(move |_| tick.get() / 60)
}

/// The expanded row: roles, the period, the record and the external profile.
#[component]
fn MemberDetail(username: String) -> impl IntoView {
    let war = expect_context::<MemberWar>();
    {
        let username = username.clone();
        let minute = minute_clock();
        Effect::new(move || {
            let _minute = minute.get();
            let period = war.period.get();
            war.ensure_standing(&username);
            war.ensure_roster();
            war.ensure_record(&username, &period);
        });
    }
    let card = crate::player_card_url(&username);
    view! {
        <WarRoles username=username.clone() />
        <PeriodPicker />
        <RecordView username=username.clone() />
        <a class="ui-text-action mwar-profile" href=card target="_blank" rel="noopener noreferrer">
            "Seqwawa profile"
            <span class="ui-external" aria-hidden="true" />
        </a>
    }
}

/// 7 days, 30 days, the season, or chosen days. One choice for every open row.
#[component]
fn PeriodPicker() -> impl IntoView {
    let war = expect_context::<MemberWar>();
    let name = format!("mwar-period-{}", next_picker_id());
    let today = chrono::Utc::now().date_naive();
    let (custom_from, custom_to) = (war.custom_from, war.custom_to);
    let apply_custom = move || {
        let (from, to) = (custom_from.get_untracked(), custom_to.get_untracked());
        let valid = matches!(
            (
                chrono::NaiveDate::parse_from_str(&from, "%Y-%m-%d"),
                chrono::NaiveDate::parse_from_str(&to, "%Y-%m-%d"),
            ),
            (Ok(first), Ok(last)) if first <= last && last <= today
        );
        if valid {
            war.period.set(Period::Custom { from, to });
        }
    };
    let choices = [
        (Period::Week, "7 days"),
        (Period::Month, "30 days"),
        (Period::Season, "Season"),
    ];
    let custom_selected = Memo::new(move |_| matches!(war.period.get(), Period::Custom { .. }));
    view! {
        <div class="mwar-periods" role="radiogroup" aria-label="Period">
            {choices.into_iter().map(|(period, label)| {
                let checked = {
                    let period = period.clone();
                    Memo::new(move |_| war.period.get() == period)
                };
                view! {
                    <label class="mwar-period">
                        <input type="radio" name=name.clone() prop:checked=move || checked.get() on:change=move |_| war.period.set(period.clone()) />
                        <span>{label}</span>
                    </label>
                }
            }).collect_view()}
            <label class="mwar-period">
                <input type="radio" name=name.clone() prop:checked=move || custom_selected.get() on:change=move |_| apply_custom() />
                <span>"Custom"</span>
            </label>
        </div>
        <Show when=move || custom_selected.get()>
            <div class="mwar-custom">
                <label>
                    <span class="sr-only">"From"</span>
                    <input type="date" max=today.to_string() prop:value=move || custom_from.get() on:change=move |event| { custom_from.set(event_target_value(&event)); apply_custom(); } />
                </label>
                <span aria-hidden="true">"to"</span>
                <label>
                    <span class="sr-only">"To"</span>
                    <input type="date" max=today.to_string() prop:value=move || custom_to.get() on:change=move |event| { custom_to.set(event_target_value(&event)); apply_custom(); } />
                </label>
            </div>
        </Show>
    }
}

fn next_picker_id() -> u32 {
    thread_local! {
        static NEXT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    NEXT.with(|next| {
        let id = next.get();
        next.set(id.wrapping_add(1));
        id
    })
}

/// The record for the chosen period. While a new period loads, the last record stays in place,
/// marked busy, so the row does not collapse and jump.
#[component]
fn RecordView(username: String) -> impl IntoView {
    let war = expect_context::<MemberWar>();
    let tick: RwSignal<i64> = expect_context();
    let current = {
        let username = username.clone();
        Memo::new(move |_| war.record(&username, &war.period.get()))
    };
    let shown = Memo::new(
        move |previous: Option<&Option<MemberWarPeriod>>| match current.get() {
            Some(Load::Ready(record)) => Some(record),
            Some(Load::Pending) | None => previous.cloned().flatten(),
            Some(Load::Failed(_)) => None,
        },
    );
    let busy = Memo::new(move |_| matches!(current.get(), Some(Load::Pending) | None));
    let failure = Memo::new(move |_| match current.get() {
        Some(Load::Failed(failure)) => Some(failure),
        _ => None,
    });
    let outdated = {
        let username = username.clone();
        Memo::new(move |_| war.record_outdated(&username, &war.period.get()))
    };
    let refresh = {
        let username = username.clone();
        move |_| war.ensure_record(&username, &war.period.get_untracked())
    };
    let retry = move |_| war.retry_record(&username, &war.period.get_untracked());
    view! {
        <div class="mwar-record" aria-busy=move || busy.get().to_string() aria-live="polite">
            {move || outdated.get().then(|| view! {
                <p class="mwar-quiet mwar-outdated">
                    "Could not refresh, showing the last record "
                    <button type="button" class="ui-text-action" on:click=refresh.clone()>"Retry"</button>
                </p>
            })}
            {move || match failure.get() {
                Some(Failure::NoSeason) => Some(view! { <p class="mwar-quiet">"No published season is running"</p> }.into_any()),
                Some(Failure::NotOnRoster) => Some(view! { <p class="mwar-quiet">"No longer on the Sequoia roster"</p> }.into_any()),
                Some(Failure::Denied) => Some(view! { <p class="mwar-quiet">"Sign in as a Sequoia member to see war records"</p> }.into_any()),
                Some(Failure::Unavailable) => Some(view! {
                    <p class="mwar-quiet">
                        "War record unavailable "
                        <button type="button" class="ui-text-action" on:click=retry.clone()>"Retry"</button>
                    </p>
                }.into_any()),
                None => None,
            }}
            {move || (shown.with(Option::is_none) && busy.get()).then(|| view! { <p class="mwar-quiet">"Loading war record"</p> })}
            {move || shown.get().map(|period| view! { <RecordBody period now=tick.get_untracked() /> })}
        </div>
    }
}

#[component]
fn RecordBody(period: MemberWarPeriod, now: i64) -> impl IntoView {
    let record = period.record;
    let totals = record.totals;
    let window = window_label(&period.kind, period.season_id, &period.from, &period.to);
    if totals.recorded() == 0 {
        return view! {
            <p class="mwar-window">{window}</p>
            <p class="mwar-empty">"No recorded wars"</p>
            <Previous kind=period.kind totals=record.previous.totals />
        }
        .into_any();
    }
    let attribution = attribution(&record.by_guild);
    view! {
        <p class="mwar-window">{window}</p>
        <dl class="mwar-stats">
            <div>
                <dt>"Points"</dt>
                <dd class="mwar-points">{points_text(&totals)}</dd>
            </div>
            <div>
                <dt>"Scored wars"</dt>
                <dd>{totals.war_count}</dd>
            </div>
            {(totals.unavailable_war_count > 0).then(|| view! {
                <div>
                    <dt>"Without points"</dt>
                    <dd>{totals.unavailable_war_count}</dd>
                </div>
            })}
        </dl>
        <Previous kind=period.kind totals=record.previous.totals />
        {attribution.map(|line| view! { <p class="mwar-attribution">{line}</p> })}
        {(!record.recent.is_empty()).then(|| view! {
            <h4 class="mwar-heading">"Recent wars"</h4>
            <ol class="mwar-recent">
                {record.recent.into_iter().map(|war| {
                    let (value, state) = match (war.points_status, war.war_points) {
                        (PointsStatus::Unavailable, _) | (_, None) => ("Unknown".to_string(), "unavailable"),
                        (PointsStatus::OffSeason, _) => ("Off-season".to_string(), "off-season"),
                        (PointsStatus::Scored, Some(points)) => (format!("{points:.1}"), "scored"),
                    };
                    let guild = war.guild_prefix.clone().filter(|prefix| !prefix.eq_ignore_ascii_case(crate::SEQUOIA_GUILD_PREFIX));
                    view! {
                        <li>
                            <span class="mwar-when">{since(&war.completed_at, now)}</span>
                            <span class="mwar-where">
                                {war.territory}
                                {guild.map(|prefix| view! { <span class="mwar-guild">{format!(" [{prefix}]")}</span> })}
                            </span>
                            <span class="mwar-value" data-state=state>{value}</span>
                        </li>
                    }
                }).collect_view()}
            </ol>
        })}
    }
    .into_any()
}

/// The previous equal window's observed totals, beside the current ones; never a growth rate.
#[component]
fn Previous(kind: PeriodKind, totals: WarTotals) -> impl IntoView {
    let span = match kind {
        PeriodKind::Week => "Previous 7 days",
        PeriodKind::Month => "Previous 30 days",
        PeriodKind::Season | PeriodKind::Custom => "Previous equal span",
    };
    let text = summary(&totals);
    view! { <p class="mwar-previous">{format!("{span}: {text}")}</p> }
}

/// A window's totals in one line: no wars, wars that could not be scored, or points and wars.
fn summary(totals: &WarTotals) -> String {
    let plural = |count: u64, one: &str, many: &str| {
        format!("{count} {}", if count == 1 { one } else { many })
    };
    match (totals.war_count, totals.unavailable_war_count) {
        (0, 0) => "no recorded wars".to_string(),
        (0, unscored) => plural(unscored, "war without points", "wars without points"),
        (scored, _) => format!(
            "{} points, {}",
            points_text(totals),
            plural(scored, "scored war", "scored wars")
        ),
    }
}

/// Points, unless none of the wars could be scored: then they are unknown, not zero.
fn points_text(totals: &WarTotals) -> String {
    if totals.war_count == 0 {
        "unknown".to_string()
    } else {
        format!("{:.1}", totals.war_points)
    }
}

fn window_label(kind: &PeriodKind, season: Option<i32>, from: &str, to: &str) -> String {
    let parse = |rfc: &str| chrono::DateTime::parse_from_rfc3339(rfc).ok();
    let day = |time: Option<chrono::DateTime<chrono::FixedOffset>>| {
        time.map(|time| time.format("%b %-d").to_string())
            .unwrap_or_default()
    };
    // The end is exclusive: a window ending at midnight ends on the day before.
    let last = parse(to).map(|end| end - chrono::Duration::seconds(1));
    match (kind, season) {
        (PeriodKind::Season, Some(season)) => {
            format!("Season {season}, since {}", day(parse(from)))
        }
        _ => format!("{} to {}", day(parse(from)), day(last)),
    }
}

/// Which guild the wars counted for, when they were not all Sequoia's.
fn attribution(by_guild: &[sequoia_shared::member_war::GuildWarTotals]) -> Option<String> {
    let all_sequoia = by_guild.iter().all(|guild| {
        guild
            .guild_prefix
            .as_deref()
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(crate::SEQUOIA_GUILD_PREFIX))
    });
    if all_sequoia {
        return None;
    }
    let parts: Vec<String> = by_guild
        .iter()
        .map(|guild| {
            let wars = guild.totals.recorded();
            match &guild.guild_prefix {
                Some(prefix) => format!("[{prefix}] {wars}"),
                None => format!("unattributed {wars}"),
            }
        })
        .collect();
    Some(format!("Wars by guild at the time: {}", parts.join(", ")))
}

/// The war popover's compact line: roles and the last 7 days.
#[component]
pub(crate) fn MemberGlance(username: String) -> impl IntoView {
    let war = expect_context::<MemberWar>();
    {
        let username = username.clone();
        let minute = minute_clock();
        Effect::new(move || {
            let _minute = minute.get();
            war.ensure_standing(&username);
            war.ensure_record(&username, &Period::Week);
        });
    }
    let outdated = {
        let username = username.clone();
        Memo::new(move |_| war.record_outdated(&username, &Period::Week))
    };
    let week = {
        let username = username.clone();
        Memo::new(move |_| war.record(&username, &Period::Week))
    };
    view! {
        <WarRoles username=username.clone() />
        <p class="mwar-glance">
            {move || match week.get() {
                Some(Load::Ready(period)) => {
                    let totals = period.record.totals;
                    let note = if outdated.get() { ", could not refresh" } else { "" };
                    format!("7 days: {}{note}", summary(&totals))
                }
                Some(Load::Failed(Failure::NotOnRoster)) => "Not on the current Sequoia roster".to_string(),
                Some(Load::Failed(_)) => "War record unavailable".to_string(),
                Some(Load::Pending) | None => "7 days: loading".to_string(),
            }}
        </p>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn totals(war_points: f64, war_count: u64, unavailable_war_count: u64) -> WarTotals {
        WarTotals {
            war_points,
            war_count,
            unavailable_war_count,
        }
    }

    #[test]
    fn points_are_unknown_when_no_war_could_be_scored_and_zero_only_when_scored() {
        assert_eq!(points_text(&totals(0.0, 0, 3)), "unknown");
        assert_eq!(points_text(&totals(0.0, 2, 0)), "0.0");
        assert_eq!(points_text(&totals(412.46, 9, 1)), "412.5");
    }

    #[test]
    fn a_summary_never_turns_unscored_wars_into_zero_points() {
        assert_eq!(summary(&totals(0.0, 0, 0)), "no recorded wars");
        assert_eq!(summary(&totals(0.0, 0, 2)), "2 wars without points");
        assert_eq!(summary(&totals(0.0, 1, 0)), "0.0 points, 1 scored war");
    }

    #[test]
    fn a_custom_period_asks_for_its_own_days() {
        let period = Period::Custom {
            from: "2026-10-01".into(),
            to: "2026-10-14".into(),
        };
        assert_eq!(
            period.query(),
            "period=custom&from=2026-10-01&to=2026-10-14"
        );
        assert_eq!(Period::Season.query(), "period=season");
    }
}
