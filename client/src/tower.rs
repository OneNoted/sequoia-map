use std::collections::HashMap;

use leptos::prelude::*;
use wasm_bindgen::JsCast;

use sequoia_shared::tower::{
    self, ATTACK_RATES, AURA_LABELS, DAMAGES, DEFENSES, HEALTHS, VOLLEY_LABELS,
};

use crate::app::Selected;
use crate::colors::rgba_css;
use crate::territory::ClientTerritoryMap;

/// Persistent tower calculator state, provided via context so stats survive territory switches.
#[derive(Clone, Copy)]
pub struct TowerState {
    pub damage_lvl: RwSignal<u32>,
    pub attack_lvl: RwSignal<u32>,
    pub health_lvl: RwSignal<u32>,
    pub defense_lvl: RwSignal<u32>,
    pub aura_lvl: RwSignal<u32>,
    pub volley_lvl: RwSignal<u32>,
    pub is_hq: RwSignal<bool>,
    pub connections: RwSignal<u32>,
    pub externals: RwSignal<u32>,
    max_preset_snapshot: RwSignal<Option<TowerLevelSnapshot>>,
}

impl TowerState {
    pub fn new() -> Self {
        Self {
            damage_lvl: RwSignal::new(0),
            attack_lvl: RwSignal::new(0),
            health_lvl: RwSignal::new(0),
            defense_lvl: RwSignal::new(0),
            aura_lvl: RwSignal::new(0),
            volley_lvl: RwSignal::new(0),
            is_hq: RwSignal::new(false),
            connections: RwSignal::new(0),
            externals: RwSignal::new(0),
            max_preset_snapshot: RwSignal::new(None),
        }
    }
}

#[derive(Clone, Copy)]
struct TowerLevelSnapshot {
    damage_lvl: u32,
    attack_lvl: u32,
    health_lvl: u32,
    defense_lvl: u32,
    aura_lvl: u32,
    volley_lvl: u32,
    is_hq: bool,
}

/// Tower calculator component for the sidebar detail panel.
#[component]
pub fn TowerCalculator() -> impl IntoView {
    let Selected(selected) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();

    let TowerState {
        damage_lvl,
        attack_lvl,
        health_lvl,
        defense_lvl,
        aura_lvl,
        volley_lvl,
        is_hq,
        connections,
        externals,
        max_preset_snapshot,
    } = expect_context();

    // Computed results
    let dps_range = Memo::new(move |_| {
        tower::calc_dps_range(
            damage_lvl.get() as usize,
            attack_lvl.get() as usize,
            is_hq.get(),
            connections.get(),
            externals.get(),
        )
    });

    let ehp = Memo::new(move |_| {
        tower::calc_ehp(
            health_lvl.get() as usize,
            defense_lvl.get() as usize,
            is_hq.get(),
            connections.get(),
            externals.get(),
        )
    });

    let defense_index = Memo::new(move |_| {
        tower::calc_defense_index(
            damage_lvl.get() as usize,
            attack_lvl.get() as usize,
            health_lvl.get() as usize,
            defense_lvl.get() as usize,
            aura_lvl.get() as usize,
            volley_lvl.get() as usize,
            is_hq.get(),
            connections.get(),
            externals.get(),
        )
    });

    let defense_rating = Memo::new(move |_| tower::DefenseRating::from_index(defense_index.get()));

    let damage_range = Memo::new(move |_| {
        let lvl = damage_lvl.get() as usize;
        let d = &DAMAGES[lvl.min(11)];
        let mult = tower::calc_stat(1.0, is_hq.get(), connections.get(), externals.get());
        (d.start * mult, d.end * mult)
    });

    let attack_speed = Memo::new(move |_| ATTACK_RATES[attack_lvl.get() as usize]);

    let health_val = Memo::new(move |_| {
        let base = HEALTHS[health_lvl.get().min(11) as usize];
        tower::calc_stat(base, is_hq.get(), connections.get(), externals.get())
    });

    let defense_pct = Memo::new(move |_| DEFENSES[defense_lvl.get().min(11) as usize]);

    let is_max_preset_active = Memo::new(move |_| max_preset_snapshot.get().is_some());
    let on_max_toggle = move |_| {
        if let Some(snapshot) = max_preset_snapshot.get_untracked() {
            damage_lvl.set(snapshot.damage_lvl);
            attack_lvl.set(snapshot.attack_lvl);
            health_lvl.set(snapshot.health_lvl);
            defense_lvl.set(snapshot.defense_lvl);
            aura_lvl.set(snapshot.aura_lvl);
            volley_lvl.set(snapshot.volley_lvl);
            is_hq.set(snapshot.is_hq);
            max_preset_snapshot.set(None);
        } else {
            max_preset_snapshot.set(Some(TowerLevelSnapshot {
                damage_lvl: damage_lvl.get_untracked(),
                attack_lvl: attack_lvl.get_untracked(),
                health_lvl: health_lvl.get_untracked(),
                defense_lvl: defense_lvl.get_untracked(),
                aura_lvl: aura_lvl.get_untracked(),
                volley_lvl: volley_lvl.get_untracked(),
                is_hq: is_hq.get_untracked(),
            }));
            damage_lvl.set(11);
            attack_lvl.set(11);
            health_lvl.set(11);
            defense_lvl.set(11);
        }
    };

    let owned_counts = Memo::new(move |_| {
        let name = selected.get()?;
        let map = territories.get();
        let ct = map.get(&name)?;
        let guild_uuid = ct.territory.guild.uuid.as_str();
        let connections_slice = ct.territory.connections.as_slice();

        let (guild_conn, _total_conn, ext) =
            tower::count_guild_connections(&name, connections_slice, guild_uuid, |n| {
                let ct2 = map.get(n)?;
                Some((
                    ct2.territory.guild.uuid.as_str(),
                    ct2.territory.connections.as_slice(),
                ))
            });
        Some((guild_conn, ext))
    });

    let owned_counts_for_apply = owned_counts;
    let on_reset_owned = move |_| {
        max_preset_snapshot.set(None);
        damage_lvl.set(0);
        attack_lvl.set(0);
        health_lvl.set(0);
        defense_lvl.set(0);
        aura_lvl.set(0);
        volley_lvl.set(0);
        is_hq.set(false);
        if let Some((guild_conn, ext)) = owned_counts_for_apply.get_untracked() {
            connections.set(guild_conn);
            externals.set(ext);
        }
    };

    let selected_for_all_ext = selected;
    let territories_for_all_ext = territories;
    let on_apply_all_externals = move |_| {
        let Some(name) = selected_for_all_ext.get_untracked() else {
            return;
        };
        let map = territories_for_all_ext.get_untracked();
        if !map.contains_key(&name) {
            return;
        }
        let connections_map: HashMap<String, Vec<String>> = map
            .iter()
            .map(|(territory, ct)| (territory.clone(), ct.territory.connections.clone()))
            .collect();
        let all_ext = tower::find_externals(&name, &connections_map, 3).len() as u32;
        externals.set(all_ext);
    };

    view! {
        <section class="tcard-section tower" aria-labelledby="tower-heading">
            <h3 class="tcard-section-title" id="tower-heading">"Tower calculator"</h3>

            <div class="tower-stats">
                <StatRow label="Damage" level=damage_lvl max=11 detail=Memo::new(move |_| {
                    let (lo, hi) = damage_range.get();
                    format!("{}-{}", tower::format_stat(lo), tower::format_stat(hi))
                }) />
                <StatRow label="Attack" level=attack_lvl max=11 detail=Memo::new(move |_| {
                    format!("{:.2}x", attack_speed.get())
                }) />
                <StatRow label="Health" level=health_lvl max=11 detail=Memo::new(move |_| {
                    tower::format_stat(health_val.get())
                }) />
                <StatRow label="Defense" level=defense_lvl max=11 detail=Memo::new(move |_| {
                    format!("{:.0}%", defense_pct.get())
                }) />
                <StatRow label="Aura" level=aura_lvl max=3 detail=Memo::new(move |_| {
                    AURA_LABELS[aura_lvl.get().min(3) as usize].to_string()
                }) />
                <StatRow label="Volley" level=volley_lvl max=3 detail=Memo::new(move |_| {
                    VOLLEY_LABELS[volley_lvl.get().min(3) as usize].to_string()
                }) />
            </div>

            <div class="tower-links">
                // Each caption names its own input; wrapping the steppers in a label would hand a
                // click on the caption to the first stepper button.
                <div class="tower-link">
                    <label for="tower-connections">"Connections"</label>
                    <CounterInput id="tower-connections" label="Connections" value=connections max=20 synced_value=externals synced_max=50 />
                </div>
                <div class="tower-link">
                    <label for="tower-externals">"Externals"</label>
                    <CounterInput id="tower-externals" label="Externals" value=externals max=50 />
                </div>
            </div>

            <div class="tower-actions">
                <button
                    type="button"
                    class="tower-action"
                    aria-pressed=move || is_hq.get().to_string()
                    on:click=move |_| is_hq.update(|v| *v = !*v)
                >"HQ"</button>
                <button
                    type="button"
                    class="tower-action"
                    title="Damage, attack, health and defense at 11"
                    aria-pressed=move || is_max_preset_active.get().to_string()
                    on:click=on_max_toggle
                >"11x4"</button>
                <button
                    type="button"
                    class="tower-action"
                    title="Reset connections and externals to the guild's own"
                    on:click=on_reset_owned
                >"Reset"</button>
                <button
                    type="button"
                    class="tower-action"
                    title="Externals: every territory within 3 hops, whoever owns it"
                    on:click=on_apply_all_externals
                >"All Ext"</button>
            </div>

            <dl class="tower-results">
                <div>
                    <dt>"DPS"</dt>
                    <dd class="tower-dps">
                        {move || {
                            let (lo, hi) = dps_range.get();
                            format!("{}-{}", tower::format_stat(lo), tower::format_stat(hi))
                        }}
                    </dd>
                </div>
                <div>
                    <dt>"EHP"</dt>
                    <dd class="tower-ehp">{move || tower::format_stat(ehp.get())}</dd>
                </div>
                <div>
                    <dt>"Rating"</dt>
                    <dd style:color=move || {
                        let (rr, rg, rb) = defense_rating.get().color_rgb();
                        rgba_css(rr, rg, rb, 1.0)
                    }>
                        {move || format!("{} ({})", defense_rating.get().label(), defense_index.get())}
                    </dd>
                </div>
            </dl>
        </section>
    }
}

/// A single stat row with label, +/- buttons, editable level input, scroll support, and detail text.
#[component]
fn StatRow(
    label: &'static str,
    level: RwSignal<u32>,
    max: u32,
    detail: Memo<String>,
) -> impl IntoView {
    let on_dec = move |_| level.update(|v| *v = v.saturating_sub(1));
    let on_inc = move |_| level.update(|v| *v = (*v + 1).min(max));
    let on_wheel = move |ev: web_sys::WheelEvent| {
        ev.prevent_default();
        if ev.delta_y() < 0.0 {
            level.update(|v| *v = (*v + 1).min(max));
        } else if ev.delta_y() > 0.0 {
            level.update(|v| *v = v.saturating_sub(1));
        }
    };
    let on_input = move |ev: leptos::ev::Event| {
        let val = event_target_value(&ev);
        if let Ok(n) = val.parse::<u32>() {
            level.set(n.min(max));
        }
    };
    let on_focus = move |ev: web_sys::FocusEvent| {
        if let Some(target) = ev.target()
            && let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>()
        {
            input.select();
        }
    };

    view! {
        <div class="tower-stat" on:wheel=on_wheel>
            <span class="tower-stat-label">{label}</span>
            <div class="tower-stepper">
                <button type="button" aria-label=format!("Lower {label}") on:click=on_dec>"\u{2212}"</button>
                <input
                    type="text"
                    inputmode="numeric"
                    aria-label=format!("{label} level")
                    prop:value=move || level.get().to_string()
                    on:input=on_input
                    on:focus=on_focus
                />
                <button type="button" aria-label=format!("Raise {label}") on:click=on_inc>"+"</button>
            </div>
            <span class="tower-stat-detail">{move || detail.get()}</span>
        </div>
    }
}

/// Small counter input with +/- buttons, editable value, and scroll support.
#[component]
fn CounterInput(
    id: &'static str,
    label: &'static str,
    value: RwSignal<u32>,
    max: u32,
    #[prop(optional)] synced_value: Option<RwSignal<u32>>,
    #[prop(optional)] synced_max: Option<u32>,
) -> impl IntoView {
    let apply_value = move |next: u32| {
        let prev = value.get_untracked();
        let next = next.min(max);
        if prev == next {
            return;
        }
        value.set(next);

        let Some(synced_value) = synced_value else {
            return;
        };
        let synced_cap = synced_max.unwrap_or(u32::MAX);

        if next > prev {
            let delta = next - prev;
            synced_value.update(|v| *v = v.saturating_add(delta).min(synced_cap));
        } else {
            let delta = prev - next;
            synced_value.update(|v| *v = v.saturating_sub(delta));
        }
    };

    let on_dec = move |_| apply_value(value.get_untracked().saturating_sub(1));
    let on_inc = move |_| apply_value(value.get_untracked() + 1);
    let on_wheel = move |ev: web_sys::WheelEvent| {
        ev.prevent_default();
        if ev.delta_y() < 0.0 {
            apply_value(value.get_untracked() + 1);
        } else if ev.delta_y() > 0.0 {
            apply_value(value.get_untracked().saturating_sub(1));
        }
    };
    let on_input = move |ev: leptos::ev::Event| {
        let val = event_target_value(&ev);
        if let Ok(n) = val.parse::<u32>() {
            apply_value(n);
        }
    };
    let on_focus = move |ev: web_sys::FocusEvent| {
        if let Some(target) = ev.target()
            && let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>()
        {
            input.select();
        }
    };

    view! {
        <div class="tower-stepper" on:wheel=on_wheel>
            <button type="button" aria-label=format!("Fewer {label}") on:click=on_dec>"\u{2212}"</button>
            <input
                type="text"
                inputmode="numeric"
                id=id
                aria-label=label
                prop:value=move || value.get().to_string()
                on:input=on_input
                on:focus=on_focus
            />
            <button type="button" aria-label=format!("More {label}") on:click=on_inc>"+"</button>
        </div>
    }
}
