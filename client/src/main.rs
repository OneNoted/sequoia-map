mod app;
mod assets;
mod auth;
mod canvas;
#[cfg(target_arch = "wasm32")]
mod gpu;
mod heat;
mod history;
mod icons;
mod map_intel;
mod navbar;
mod playback;
mod players;
mod render_loop;
mod renderer;
mod season_scalar;
mod sidebar;
mod site_nav;
mod sse;
mod tiles;
mod timeline;
mod tower;
mod ui_icons;
mod war_stats;
mod warcontroller;

// Shared map math and state helpers, also used by the other browser client.
#[cfg(target_arch = "wasm32")]
pub(crate) use sequoia_map_engine::{claim_labels, label_layout, overlay_sizing};
pub(crate) use sequoia_map_engine::{colors, defense, spatial, territory, time_format, viewport};

#[cfg(not(target_arch = "wasm32"))]
#[path = "gpu/native.rs"]
mod gpu;

use leptos::mount::mount_to;
use leptos::prelude::*;
use leptos_router::components::Router;
use std::any::Any;
use std::cell::RefCell;
use wasm_bindgen::JsCast;

thread_local! {
    static APP_MOUNT_HANDLE: RefCell<Option<Box<dyn Any>>> = RefCell::new(None);
}

pub(crate) const SEQUOIA_WEBSITE_URL: &str = "https://seqwawa.com";

/// Our guild's Wynncraft tag. Members of this guild get seqwawa playercards
/// instead of Wynncraft stats pages, since playercards only cover our roster.
pub(crate) const SEQUOIA_GUILD_PREFIX: &str = "SEQ";

fn encode_uri_component_fallback(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut encoded = String::with_capacity(input.len());
    for byte in input.bytes() {
        let is_safe = matches!(
            byte,
            b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b'-'
                | b'_'
                | b'.'
                | b'!'
                | b'~'
                | b'*'
                | b'\''
                | b'('
                | b')'
        );
        if is_safe {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0F) as usize] as char);
        }
    }
    encoded
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn encode_uri_component(value: &str) -> String {
    js_sys::encode_uri_component(value)
        .as_string()
        .unwrap_or_else(|| encode_uri_component_fallback(value))
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn encode_uri_component(value: &str) -> String {
    encode_uri_component_fallback(value)
}

pub(crate) fn guild_stats_url(guild_name: &str) -> String {
    let encoded = encode_uri_component(guild_name);
    format!("https://wynncraft.com/stats/guild/{encoded}")
}

/// A player's card on the Sequoia website. Only our own members have one, so
/// gate callers on [`SEQUOIA_GUILD_PREFIX`] before linking here.
pub(crate) fn player_card_url(username: &str) -> String {
    let encoded = encode_uri_component(username);
    format!("{SEQUOIA_WEBSITE_URL}/statistics/player/playercard?player={encoded}")
}

fn main() {
    console_error_panic_hook::set_once();
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let mount_target = document
        .get_element_by_id("app")
        .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        .or_else(|| document.body());
    let Some(target) = mount_target else {
        return;
    };

    APP_MOUNT_HANDLE.with(move |slot| {
        // If main() is re-entered (e.g. dev/hot-reload runtime quirks), drop the old mount
        // so stale effects/signals can't keep mutating app state.
        let _old = slot.borrow_mut().take();
        let handle = mount_to(target, || {
            view! {
                <Router>
                    <app::App />
                </Router>
            }
        });
        *slot.borrow_mut() = Some(Box::new(handle));
    });
}

#[cfg(test)]
mod tests {
    use super::{encode_uri_component_fallback, guild_stats_url, player_card_url};

    #[test]
    fn fallback_uri_encoder_escapes_reserved_characters() {
        assert_eq!(encode_uri_component_fallback("A/B? C"), "A%2FB%3F%20C");
    }

    #[test]
    fn guild_stats_url_uses_encoded_path_segment() {
        assert_eq!(
            guild_stats_url("Sequoia/Map? Guild"),
            "https://wynncraft.com/stats/guild/Sequoia%2FMap%3F%20Guild"
        );
    }

    #[test]
    fn player_card_url_encodes_the_player_query_parameter() {
        assert_eq!(
            player_card_url("theoplegends"),
            "https://seqwawa.com/statistics/player/playercard?player=theoplegends"
        );
        assert_eq!(
            player_card_url("Odd Name&x"),
            "https://seqwawa.com/statistics/player/playercard?player=Odd%20Name%26x"
        );
    }
}
