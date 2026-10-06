use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::Deserialize;
use sqlx::{Postgres, QueryBuilder};
use tracing::{info, warn};

use crate::config::{ATHENA_REFRESH_SECS, ATHENA_TERRITORY_URL};
use crate::state::AppState;

/// One entry of Athena's territory list, keyed by territory name at the top
/// level. Unclaimed territories carry `"guild": null`.
#[derive(Deserialize)]
struct AthenaTerritory {
    #[serde(default)]
    guild: Option<AthenaGuild>,
}

#[derive(Deserialize)]
struct AthenaGuild {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    color: Option<String>,
}

pub async fn run(state: AppState) {
    restore_cached_guild_colors_if_empty(&state, "startup").await;
    let mut interval = tokio::time::interval(Duration::from_secs(ATHENA_REFRESH_SECS));

    loop {
        interval.tick().await;

        match fetch_guild_colors(&state.http_client).await {
            Ok(colors) => {
                if colors.is_empty() {
                    warn!(
                        "received empty guild color payload from Athena; keeping last known color cache"
                    );
                    restore_cached_guild_colors_if_empty(&state, "athena_empty_payload").await;
                    continue;
                }

                let loaded_count = colors.len();
                if let Some(pool) = state.db.as_ref()
                    && let Err(e) = persist_guild_colors(pool, &colors).await
                {
                    warn!("failed to persist guild colors cache: {e}");
                }
                let total_count = {
                    let mut current = state.guild_colors.write().await;
                    merge_guild_color_cache(&mut current, colors);
                    current.len()
                };
                state.guild_colors_dirty.store(true, Ordering::Release);
                info!(
                    loaded_count,
                    total_count, "loaded guild colors from Athena and merged into cache"
                );
            }
            Err(e) => {
                warn!("failed to fetch guild colors from Athena: {e}");
                restore_cached_guild_colors_if_empty(&state, "athena_fetch_failure").await;
            }
        }
    }
}

async fn fetch_guild_colors(
    client: &reqwest::Client,
) -> Result<HashMap<String, (u8, u8, u8)>, Box<dyn std::error::Error + Send + Sync>> {
    let resp = client.get(ATHENA_TERRITORY_URL).send().await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    if !status.is_success() {
        return Err(format!(
            "upstream status {status}; body preview: {}",
            body_preview(&bytes)
        )
        .into());
    }

    parse_athena_guild_colors_payload(bytes.as_ref()).map_err(|e| {
        // A schema change upstream surfaces only as this one line, so carry
        // enough of the body to tell what Athena actually sent.
        format!(
            "failed to decode Athena payload: {e}; body preview: {}",
            body_preview(&bytes)
        )
        .into()
    })
}

fn body_preview(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(200)
        .collect::<String>()
}

fn parse_athena_guild_colors_payload(
    bytes: &[u8],
) -> Result<HashMap<String, (u8, u8, u8)>, serde_json::Error> {
    let territories: HashMap<String, AthenaTerritory> = serde_json::from_slice(bytes)?;
    let mut colors = HashMap::new();
    for entry in territories.values() {
        let Some(guild) = entry.guild.as_ref() else {
            continue;
        };
        let Some(guild_name) = guild
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let Some(guild_color_hex) = guild.color.as_deref() else {
            continue;
        };
        if let Some(rgb) = parse_hex_color(guild_color_hex) {
            colors.entry(guild_name.to_string()).or_insert(rgb);
        }
    }
    Ok(colors)
}

fn parse_hex_color(hex: &str) -> Option<(u8, u8, u8)> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((r, g, b))
}

async fn restore_cached_guild_colors_if_empty(state: &AppState, reason: &str) {
    {
        let current = state.guild_colors.read().await;
        if !current.is_empty() {
            return;
        }
    }

    let Some(pool) = state.db.as_ref() else {
        return;
    };

    match load_cached_guild_colors(pool).await {
        Ok(colors) if colors.is_empty() => {
            warn!("guild color cache is empty; no fallback colors available ({reason})");
        }
        Ok(colors) => {
            let count = colors.len();
            let mut current = state.guild_colors.write().await;
            if current.is_empty() {
                *current = colors;
                state.guild_colors_dirty.store(true, Ordering::Release);
                info!("restored guild colors for {count} guilds from persisted cache ({reason})");
            }
        }
        Err(e) => {
            warn!("failed to load persisted guild color cache ({reason}): {e}");
        }
    }
}

async fn load_cached_guild_colors(
    pool: &sqlx::PgPool,
) -> Result<HashMap<String, (u8, u8, u8)>, String> {
    let rows: Vec<(String, i16, i16, i16)> =
        sqlx::query_as("SELECT guild_name, color_r, color_g, color_b FROM guild_color_cache")
            .fetch_all(pool)
            .await
            .map_err(|e| format!("query guild_color_cache: {e}"))?;

    Ok(rows_to_guild_colors(rows))
}

fn rows_to_guild_colors(rows: Vec<(String, i16, i16, i16)>) -> HashMap<String, (u8, u8, u8)> {
    let mut colors = HashMap::new();
    for (guild_name, color_r, color_g, color_b) in rows {
        let Some(rgb) = parse_rgb_triplet(color_r, color_g, color_b) else {
            continue;
        };
        colors.insert(guild_name, rgb);
    }
    colors
}

fn parse_rgb_triplet(color_r: i16, color_g: i16, color_b: i16) -> Option<(u8, u8, u8)> {
    let color_r = u8::try_from(color_r).ok()?;
    let color_g = u8::try_from(color_g).ok()?;
    let color_b = u8::try_from(color_b).ok()?;
    Some((color_r, color_g, color_b))
}

fn merge_guild_color_cache(
    current: &mut HashMap<String, (u8, u8, u8)>,
    incoming: HashMap<String, (u8, u8, u8)>,
) {
    for (guild_name, color) in incoming {
        current.insert(guild_name, color);
    }
}

async fn persist_guild_colors(
    pool: &sqlx::PgPool,
    colors: &HashMap<String, (u8, u8, u8)>,
) -> Result<(), String> {
    if colors.is_empty() {
        return Ok(());
    }

    let mut query_builder = QueryBuilder::<Postgres>::new(
        "INSERT INTO guild_color_cache (guild_name, color_r, color_g, color_b) ",
    );
    query_builder.push_values(colors.iter(), |mut builder, (guild_name, color)| {
        builder
            .push_bind(guild_name)
            .push_bind(i16::from(color.0))
            .push_bind(i16::from(color.1))
            .push_bind(i16::from(color.2));
    });
    query_builder.push(
        " ON CONFLICT (guild_name) DO UPDATE \
         SET color_r = EXCLUDED.color_r, \
             color_g = EXCLUDED.color_g, \
             color_b = EXCLUDED.color_b, \
             updated_at = now()",
    );

    query_builder
        .build()
        .execute(pool)
        .await
        .map_err(|e| format!("upsert guild color cache rows: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        merge_guild_color_cache, parse_athena_guild_colors_payload, parse_hex_color,
        parse_rgb_triplet, rows_to_guild_colors,
    };
    use std::collections::HashMap;

    #[test]
    fn parse_hex_color_accepts_valid_hex_triplets() {
        assert_eq!(parse_hex_color("#ffd700"), Some((255, 215, 0)));
        assert_eq!(parse_hex_color("50c878"), Some((80, 200, 120)));
    }

    /// Shaped after the live `cache/get/territoryList` body: a flat map keyed by
    /// territory, whose `guild` is an object carrying the color.
    #[test]
    fn parse_athena_payload_reads_nested_guild_colors() {
        let payload = r##"{
            "Apprentice Huts": {
                "guild": {
                    "uuid": "0dd24dcc-370c-4e27-a1b4-2dfa92e76667",
                    "name": "Aequitas",
                    "prefix": "Aeq",
                    "hq": "Nivla Woods Exit",
                    "color": "#ffd700"
                },
                "acquired": "2026-08-20T09:49:31.591000Z",
                "location": {"start": [-600, -610], "end": [-670, -780]},
                "hq": false
            },
            "Jofash Tunnel": {
                "guild": {
                    "uuid": "cef53bae-dc42-46aa-8ccf-1b10d282b420",
                    "name": "Titans Valor",
                    "prefix": "ANO",
                    "hq": "Nodguj Nation",
                    "color": "#ffffff"
                },
                "acquired": "2026-08-18T01:20:00.714000Z",
                "location": {"start": [-140, -3560], "end": [-260, -3700]},
                "hq": false
            }
        }"##;

        let colors = parse_athena_guild_colors_payload(payload.as_bytes())
            .expect("the live Athena schema should decode");
        assert_eq!(colors.len(), 2);
        assert_eq!(colors.get("Aequitas"), Some(&(255, 215, 0)));
        assert_eq!(colors.get("Titans Valor"), Some(&(255, 255, 255)));
    }

    #[test]
    fn parse_athena_payload_skips_rows_without_a_named_guild() {
        let payload = r##"{
            "Lion Lair": {
                "guild": null,
                "acquired": "2026-02-26T22:13:13.493000Z",
                "location": {"start": [890, -2140], "end": [790, -2320]}
            },
            "Nameless Quarry": {
                "guild": {"uuid": "0dd2", "name": "   ", "color": "#123456"}
            },
            "Colorless Bluff": {
                "guild": {"uuid": "0dd2", "name": "Nerfuria"}
            },
            "Ragni": {
                "guild": {"uuid": "0dd2", "name": "Aequitas", "color": "#ffd700"}
            }
        }"##;

        let colors = parse_athena_guild_colors_payload(payload.as_bytes())
            .expect("payload should decode despite null and partial guild rows");
        assert_eq!(colors.len(), 1);
        assert_eq!(colors.get("Aequitas"), Some(&(255, 215, 0)));
    }

    #[test]
    fn parse_rgb_triplet_accepts_valid_ranges() {
        assert_eq!(parse_rgb_triplet(0, 127, 255), Some((0, 127, 255)));
    }

    #[test]
    fn parse_rgb_triplet_rejects_invalid_ranges() {
        assert_eq!(parse_rgb_triplet(-1, 0, 0), None);
        assert_eq!(parse_rgb_triplet(0, 256, 0), None);
    }

    #[test]
    fn rows_to_guild_colors_skips_invalid_rows() {
        let rows = vec![
            ("Aequitas".to_string(), 255, 215, 0),
            ("Broken".to_string(), -1, 10, 20),
            ("Paladins United".to_string(), 199, 179, 240),
        ];
        let colors = rows_to_guild_colors(rows);
        assert_eq!(colors.len(), 2);
        assert_eq!(colors.get("Aequitas"), Some(&(255, 215, 0)));
        assert_eq!(colors.get("Paladins United"), Some(&(199, 179, 240)));
        assert!(!colors.contains_key("Broken"));
    }

    #[test]
    fn merge_guild_color_cache_preserves_existing_entries() {
        let mut current = HashMap::new();
        current.insert("Avicia".to_string(), (16, 16, 254));
        current.insert("Aequitas".to_string(), (255, 215, 0));

        let mut incoming = HashMap::new();
        incoming.insert("Avicia".to_string(), (17, 17, 255));
        incoming.insert("Nerfuria".to_string(), (200, 80, 80));

        merge_guild_color_cache(&mut current, incoming);

        assert_eq!(current.get("Avicia"), Some(&(17, 17, 255)));
        assert_eq!(current.get("Aequitas"), Some(&(255, 215, 0)));
        assert_eq!(current.get("Nerfuria"), Some(&(200, 80, 80)));
    }
}
