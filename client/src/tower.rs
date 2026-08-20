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
    let dps = Memo::new(move |_| {
        tower::calc_dps(
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
        <div style="padding: 10px 0 4px;">
            <div style="font-family: var(--font-display); font-size: 0.85rem; text-transform: uppercase; letter-spacing: 0.12em; color: #5f5d65; margin-bottom: 10px;">
                <span style="color: var(--color-gold); margin-right: 5px; font-size: 0.7rem;">{"\u{25C6}"}</span>"Tower Calculator"
            </div>

            // Stat rows
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

            // HQ toggle + max preset + connections
            <div style="display: flex; gap: 6px; margin-top: 8px; margin-bottom: 8px;">
                <button
                    style=move || format!(
                        "flex: 1; padding: 4px 0; border-radius: 4px; border: 1px solid {}; background: {}; color: {}; font-family: var(--font-display); font-size: 0.56rem; cursor: pointer; text-transform: uppercase; letter-spacing: 0.1em; transition: border-color 0.15s, color 0.15s, background 0.15s;",
                        if is_hq.get() { "rgba(245,197,66,0.35)" } else { "var(--color-border-subtle)" },
                        if is_hq.get() { "rgba(245,197,66,0.08)" } else { "var(--color-surface)" },
                        if is_hq.get() { "var(--color-gold)" } else { "#9f9a95" },
                    )
                    on:click=move |_| is_hq.update(|v| *v = !*v)
                    on:mouseenter=move |e| {
                        if is_hq.get_untracked() {
                            return;
                        }
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("border-color", "rgba(245,197,66,0.35)").ok();
                            el.style().set_property("color", "var(--color-gold)").ok();
                            el.style().set_property("background", "rgba(245,197,66,0.08)").ok();
                        }
                    }
                    on:mouseleave=move |e| {
                        if is_hq.get_untracked() {
                            return;
                        }
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("border-color", "var(--color-border-subtle)").ok();
                            el.style().set_property("color", "#9f9a95").ok();
                            el.style().set_property("background", "var(--color-surface)").ok();
                        }
                    }
                >"HQ"</button>
                <button
                    style=move || format!(
                        "flex: 1; padding: 4px 0; border-radius: 4px; border: 1px solid {}; background: {}; color: {}; font-family: var(--font-display); font-size: 0.56rem; cursor: pointer; text-transform: uppercase; letter-spacing: 0.1em; transition: border-color 0.15s, color 0.15s, background 0.15s;",
                        if is_max_preset_active.get() { "rgba(245,197,66,0.35)" } else { "var(--color-border-subtle)" },
                        if is_max_preset_active.get() { "rgba(245,197,66,0.08)" } else { "var(--color-surface)" },
                        if is_max_preset_active.get() { "var(--color-gold)" } else { "#9f9a95" },
                    )
                    on:click=on_max_toggle
                    on:mouseenter=move |e| {
                        if is_max_preset_active.get_untracked() {
                            return;
                        }
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("border-color", "rgba(245,197,66,0.35)").ok();
                            el.style().set_property("color", "var(--color-gold)").ok();
                            el.style().set_property("background", "rgba(245,197,66,0.08)").ok();
                        }
                    }
                    on:mouseleave=move |e| {
                        if is_max_preset_active.get_untracked() {
                            return;
                        }
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("border-color", "var(--color-border-subtle)").ok();
                            el.style().set_property("color", "#9f9a95").ok();
                            el.style().set_property("background", "var(--color-surface)").ok();
                        }
                    }
                >"11x4"</button>
                <div style="flex: 1; display: flex; align-items: center; gap: 4px;">
                    <span style="font-family: var(--font-body); font-size: 0.62rem; color: #5f5d65; white-space: nowrap;">"Conn"</span>
                    <CounterInput
                        value=connections
                        max=20
                        synced_value=externals
                        synced_max=50
                    />
                </div>
                <div style="flex: 1; display: flex; align-items: center; gap: 4px;">
                    <span style="font-family: var(--font-body); font-size: 0.62rem; color: #5f5d65; white-space: nowrap;">"Ext"</span>
                    <CounterInput value=externals max=50 />
                </div>
            </div>

            <div style="display: flex; gap: 6px; margin-top: -2px; margin-bottom: 8px;">
                <button
                    title="Reset Conn/Ext to guild-owned values"
                    style="flex: 1; padding: 4px 0; border-radius: 4px; border: 1px solid var(--color-border-subtle); background: var(--color-surface); color: #9f9a95; font-family: var(--font-display); font-size: 0.56rem; cursor: pointer; text-transform: uppercase; letter-spacing: 0.1em; transition: border-color 0.15s, color 0.15s, background 0.15s;"
                    on:click=on_reset_owned
                    on:mouseenter=|e| {
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("border-color", "rgba(245,197,66,0.35)").ok();
                            el.style().set_property("color", "var(--color-gold)").ok();
                            el.style().set_property("background", "rgba(245,197,66,0.08)").ok();
                        }
                    }
                    on:mouseleave=|e| {
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("border-color", "var(--color-border-subtle)").ok();
                            el.style().set_property("color", "#9f9a95").ok();
                            el.style().set_property("background", "var(--color-surface)").ok();
                        }
                    }
                >"Reset"</button>
                <button
                    title="Set Ext to all territories reachable in 3 hops (ignores ownership)"
                    style="flex: 1; padding: 4px 0; border-radius: 4px; border: 1px solid var(--color-border-subtle); background: var(--color-surface); color: #9f9a95; font-family: var(--font-display); font-size: 0.56rem; cursor: pointer; text-transform: uppercase; letter-spacing: 0.1em; transition: border-color 0.15s, color 0.15s, background 0.15s;"
                    on:click=on_apply_all_externals
                    on:mouseenter=|e| {
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("border-color", "rgba(80,200,220,0.45)").ok();
                            el.style().set_property("color", "#50c8dc").ok();
                            el.style().set_property("background", "rgba(80,200,220,0.08)").ok();
                        }
                    }
                    on:mouseleave=|e| {
                        if let Some(el) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
                            el.style().set_property("border-color", "var(--color-border-subtle)").ok();
                            el.style().set_property("color", "#9f9a95").ok();
                            el.style().set_property("background", "var(--color-surface)").ok();
                        }
                    }
                >"All Ext"</button>
            </div>

            // Computed results
            <div class="divider-gold" style="margin: 8px 0;" />
            <div style="display: flex; flex-direction: column; gap: 6px;">
                <div style="display: flex; justify-content: space-between; align-items: center;">
                    <span style="font-family: var(--font-display); font-size: 0.6rem; color: #9f9a95; text-transform: uppercase; letter-spacing: 0.1em;">"Avg DPS"</span>
                    <span style="font-family: var(--font-mono); font-size: 0.82rem; color: var(--color-gold); font-weight: 700;">
                        {move || tower::format_stat(dps.get())}
                    </span>
                </div>
                <div style="display: flex; justify-content: space-between; align-items: center;">
                    <span style="font-family: var(--font-display); font-size: 0.6rem; color: #9f9a95; text-transform: uppercase; letter-spacing: 0.1em;">"EHP"</span>
                    <span style="font-family: var(--font-mono); font-size: 0.82rem; color: var(--color-emerald); font-weight: 700;">
                        {move || tower::format_stat(ehp.get())}
                    </span>
                </div>
                <div style="display: flex; justify-content: space-between; align-items: center;">
                    <span style="font-family: var(--font-display); font-size: 0.6rem; color: #9f9a95; text-transform: uppercase; letter-spacing: 0.1em;">"Rating"</span>
                    <span style=move || {
                        let rating = defense_rating.get();
                        let (rr, rg, rb) = rating.color_rgb();
                        format!(
                            "font-family: var(--font-mono); font-size: 0.78rem; font-weight: 700; color: {};",
                            rgba_css(rr, rg, rb, 1.0)
                        )
                    }>
                        {move || format!("{} ({})", defense_rating.get().label(), defense_index.get())}
                    </span>
                </div>
            </div>
        </div>
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
        <div
            style="display: flex; align-items: center; gap: 4px; margin-bottom: 3px;"
            on:wheel=on_wheel
        >
            <span style="font-family: var(--font-display); font-size: 0.58rem; color: #5f5d65; width: 48px; text-transform: uppercase; letter-spacing: 0.06em; flex-shrink: 0;">{label}</span>
            <button
                style="width: 20px; height: 20px; border-radius: 3px; border: 1px solid var(--color-border-subtle); background: var(--color-surface); color: #9f9a95; cursor: pointer; display: flex; align-items: center; justify-content: center; font-size: 0.7rem; font-family: var(--font-mono); padding: 0; flex-shrink: 0; transition: border-color 0.15s;"
                on:click=on_dec
            >"-"</button>
            <input
                type="text"
                inputmode="numeric"
                style="width: 20px; height: 20px; border-radius: 3px; border: 1px solid var(--color-border-subtle); background: var(--color-surface); color: var(--color-text-primary); font-family: var(--font-mono); font-size: 0.72rem; text-align: center; padding: 0; flex-shrink: 0; outline: none; -moz-appearance: textfield; appearance: textfield;"
                prop:value=move || level.get().to_string()
                on:input=on_input
                on:focus=on_focus
            />
            <button
                style="width: 20px; height: 20px; border-radius: 3px; border: 1px solid var(--color-border-subtle); background: var(--color-surface); color: #9f9a95; cursor: pointer; display: flex; align-items: center; justify-content: center; font-size: 0.7rem; font-family: var(--font-mono); padding: 0; flex-shrink: 0; transition: border-color 0.15s;"
                on:click=on_inc
            >"+"</button>
            <span style="font-family: var(--font-mono); font-size: 0.65rem; color: #9f9a95; margin-left: auto; flex-shrink: 0;">
                {move || detail.get()}
            </span>
        </div>
    }
}

/// Small counter input with +/- buttons, editable value, and scroll support.
#[component]
fn CounterInput(
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
        <div
            style="display: flex; align-items: center; gap: 2px;"
            on:wheel=on_wheel
        >
            <button
                style="width: 16px; height: 16px; border-radius: 2px; border: 1px solid var(--color-border-subtle); background: var(--color-surface); color: #9f9a95; cursor: pointer; display: flex; align-items: center; justify-content: center; font-size: 0.6rem; font-family: var(--font-mono); padding: 0;"
                on:click=on_dec
            >"-"</button>
            <input
                type="text"
                inputmode="numeric"
                style="width: 16px; height: 16px; border-radius: 2px; border: 1px solid var(--color-border-subtle); background: var(--color-surface); color: var(--color-text-primary); font-family: var(--font-mono); font-size: 0.65rem; text-align: center; padding: 0; outline: none; -moz-appearance: textfield; appearance: textfield;"
                prop:value=move || value.get().to_string()
                on:input=on_input
                on:focus=on_focus
            />
            <button
                style="width: 16px; height: 16px; border-radius: 2px; border: 1px solid var(--color-border-subtle); background: var(--color-surface); color: #9f9a95; cursor: pointer; display: flex; align-items: center; justify-content: center; font-size: 0.6rem; font-family: var(--font-mono); padding: 0;"
                on:click=on_inc
            >"+"</button>
        </div>
    }
}
