//! Links into the map that name a territory, `/?territory=<name>`, as Discord territory
//! alerts carry them. The map opens on that territory as it is now; the parameter is taken
//! off the address once read, so it is a way in rather than a view the address keeps.

use wasm_bindgen::JsValue;

const TERRITORY_PARAM: &str = "territory";

/// A query string (`location.search`, with or without its `?`) split into the territory it
/// names and the query without any `territory` parameter, other parameters kept as written.
/// `None` when no `territory` parameter is present. The name is decoded as the browser's
/// `URLSearchParams` decodes it; a repeated parameter names its first value.
pub(crate) fn take_territory(search: &str) -> Option<(String, String)> {
    let mut name = None;
    let mut found = false;
    let mut kept = Vec::new();
    for pair in search.strip_prefix('?').unwrap_or(search).split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if decode(key) == TERRITORY_PARAM {
            found = true;
            name.get_or_insert_with(|| decode(value));
        } else {
            kept.push(pair);
        }
    }
    if !found {
        return None;
    }
    let rest = if kept.is_empty() {
        String::new()
    } else {
        format!("?{}", kept.join("&"))
    };
    Some((name.unwrap_or_default(), rest))
}

/// One name or value of a query as `URLSearchParams` reads it: `+` is a space, `%XX` is that
/// byte, any other `%` is itself, and bytes that are not UTF-8 become replacement characters.
fn decode(component: &str) -> String {
    let hex = |byte: Option<&u8>| byte.and_then(|&byte| char::from(byte).to_digit(16));
    let bytes = component.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        decoded.push(match byte {
            b'+' => b' ',
            b'%' => match (hex(bytes.get(at + 1)), hex(bytes.get(at + 2))) {
                (Some(high), Some(low)) => {
                    at += 2;
                    (high * 16 + low) as u8
                }
                _ => b'%',
            },
            byte => byte,
        });
        at += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Takes a territory link off the page's address: the same path, the other parameters and
/// the hash stay, and so does the history entry with its router state, so it neither reloads
/// nor adds a step to Back. The territory named, unless the link named none.
pub(crate) fn take_entry_territory() -> Option<String> {
    let window = web_sys::window()?;
    let location = window.location();
    let search = location.search().ok()?;
    let (name, rest) = take_territory(&search)?;
    let pathname = location.pathname().ok()?;
    let hash = location.hash().unwrap_or_default();
    if let Ok(history) = window.history() {
        let state = history.state().unwrap_or(JsValue::NULL);
        let cleaned = format!("{pathname}{rest}{hash}");
        if let Err(error) = history.replace_state_with_url(&state, "", Some(&cleaned)) {
            web_sys::console::warn_1(&error);
        }
    }
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::take_territory;

    fn taken(search: &str) -> Option<(String, String)> {
        take_territory(search)
    }

    #[test]
    fn decodes_names_as_url_search_params_does() {
        assert_eq!(
            taken("?territory=Troms%20Lake"),
            Some(("Troms Lake".into(), String::new()))
        );
        assert_eq!(
            taken("?territory=Ranol%27s%20Farm"),
            Some(("Ranol's Farm".into(), String::new()))
        );
        assert_eq!(
            taken("?territory=Cathedral+Harbour"),
            Some(("Cathedral Harbour".into(), String::new()))
        );
        assert_eq!(
            taken("?territory=Raiders%27%20Stronghold&x=%26"),
            Some(("Raiders' Stronghold".into(), "?x=%26".into()))
        );
    }

    #[test]
    fn keeps_other_parameters_as_written_and_in_order() {
        assert_eq!(
            taken("?utm=a%20b&territory=Detlas&x=1+2&flag"),
            Some(("Detlas".into(), "?utm=a%20b&x=1+2&flag".into()))
        );
    }

    #[test]
    fn a_query_without_the_parameter_is_left_alone() {
        assert_eq!(taken(""), None);
        assert_eq!(taken("?"), None);
        assert_eq!(taken("?territoryx=Detlas&mode=1"), None);
    }

    #[test]
    fn an_empty_or_repeated_parameter_is_removed() {
        assert_eq!(
            taken("?territory=&a=1"),
            Some((String::new(), "?a=1".into()))
        );
        assert_eq!(taken("?territory"), Some((String::new(), String::new())));
        assert_eq!(
            taken("?territory=Detlas&territory=Troms%20Lake"),
            Some(("Detlas".into(), String::new()))
        );
    }

    #[test]
    fn decodes_bytes_and_stray_percents_as_url_search_params_does() {
        assert_eq!(
            taken("?territory=Caf%C3%A9%20%E2%9C%93"),
            Some(("Café ✓".into(), String::new()))
        );
        assert_eq!(
            taken("?territory=100%25%2B%+1%4"),
            Some(("100%+% 1%4".into(), String::new()))
        );
        assert_eq!(
            taken("?territory=%FF%C3"),
            Some(("\u{FFFD}\u{FFFD}".into(), String::new()))
        );
    }

    #[test]
    fn an_encoded_parameter_name_still_counts() {
        assert_eq!(
            taken("?%74erritory=Detlas"),
            Some(("Detlas".into(), String::new()))
        );
    }
}
