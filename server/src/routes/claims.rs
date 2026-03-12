use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering as AtomicOrdering;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bytes::Bytes;

use super::http_util::{if_none_match_matches, json_bytes_response, not_modified_response};
use chrono::Utc;
use sequoia_shared::{
    CLAIM_DOCUMENT_VERSION_V1, ClaimDocumentV1, ClaimValidationError, ClaimsTerritoryGeometry,
    validate_claim_document,
};
use serde::{Deserialize, Serialize};

use crate::state::{
    AppState, CachedGuildCatalog, CachedGuildCatalogEntry, StoredClaimLayout,
    build_guild_color_lookup, lookup_guild_color,
};

const GUILD_CATALOG_TTL_SECS: i64 = 3600;
const DEFAULT_GUILD_CATALOG_LIMIT: usize = 24;
const MAX_GUILD_CATALOG_LIMIT: usize = 100;
const CLAIMS_GEOMETRY_ETAG_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const CLAIMS_GEOMETRY_ETAG_PRIME: u64 = 0x100000001b3;

#[derive(Debug, Deserialize)]
pub struct GuildCatalogQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct GuildCatalogEntry {
    pub uuid: String,
    pub name: String,
    pub prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<(u8, u8, u8)>,
}

#[derive(Debug, Serialize)]
pub struct GuildCatalogResponse {
    pub guilds: Vec<GuildCatalogEntry>,
    pub cached_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateClaimRequest {
    #[serde(default)]
    pub title: Option<String>,
    pub document: ClaimDocumentV1,
}

#[derive(Debug, Serialize)]
pub struct CreateClaimResponse {
    pub id: String,
    pub created_at: String,
    pub url: String,
}

pub async fn get_guild_catalog(
    State(state): State<AppState>,
    Query(query): Query<GuildCatalogQuery>,
) -> Result<Json<GuildCatalogResponse>, StatusCode> {
    let catalog = load_guild_catalog(&state).await?;
    let colors = state.guild_colors.read().await.clone();
    let normalized_colors = build_guild_color_lookup(&colors);
    let needle = query.q.trim().to_ascii_lowercase();
    let limit = query
        .limit
        .unwrap_or(DEFAULT_GUILD_CATALOG_LIMIT)
        .clamp(1, MAX_GUILD_CATALOG_LIMIT);

    let mut entries: Vec<GuildCatalogEntry> = catalog
        .entries
        .iter()
        .filter(|entry| {
            if needle.is_empty() {
                return true;
            }
            entry.name.to_ascii_lowercase().contains(&needle)
                || entry.prefix.to_ascii_lowercase().contains(&needle)
        })
        .map(|entry| GuildCatalogEntry {
            uuid: entry.uuid.clone(),
            name: entry.name.clone(),
            prefix: entry.prefix.clone(),
            color: lookup_guild_color(&colors, &normalized_colors, &entry.name)
                .or_else(|| Some(sequoia_shared::guild_color(&entry.name))),
        })
        .collect();

    entries.sort_by(|a, b| compare_catalog_entries(a, b, &needle));
    entries.truncate(limit);

    Ok(Json(GuildCatalogResponse {
        guilds: entries,
        cached_at: catalog.fetched_at.to_rfc3339(),
    }))
}

pub async fn get_claims_bootstrap_geometry(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let body = {
        let snapshot = state.live_snapshot.read().await;
        serialize_claims_bootstrap_geometry(&snapshot.territories)
    }
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let etag = claims_geometry_etag(body.as_ref());
    if if_none_match_matches(&headers, &etag) {
        return Ok(not_modified_response(
            "public, max-age=86400",
            Some(etag.as_str()),
        ));
    }

    Ok(json_bytes_response(
        body,
        "public, max-age=86400",
        Some(etag.as_str()),
    ))
}

pub async fn create_claim_layout(
    State(state): State<AppState>,
    Json(mut payload): Json<CreateClaimRequest>,
) -> Result<(StatusCode, Json<CreateClaimResponse>), StatusCode> {
    let Some(pool) = state.db.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };

    let territory_names: Vec<String> = {
        let live_snapshot = state.live_snapshot.read().await;
        live_snapshot.territories.keys().cloned().collect()
    };

    let title = payload
        .title
        .take()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    payload.document.title = title.clone();

    validate_claim_document(
        &payload.document,
        territory_names.iter().map(String::as_str),
    )
    .map_err(validation_status)?;

    let document_json =
        serde_json::to_value(&payload.document).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let id = next_claim_id(&state);
    let created_at = Utc::now();

    sqlx::query(
        "INSERT INTO claim_layouts (id, created_at, title, document_version, document) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&id)
    .bind(created_at)
    .bind(title.clone())
    .bind(i32::from(CLAIM_DOCUMENT_VERSION_V1))
    .bind(document_json)
    .execute(pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((
        StatusCode::CREATED,
        Json(CreateClaimResponse {
            id: id.clone(),
            created_at: created_at.to_rfc3339(),
            url: format!("/claims/s/{id}"),
        }),
    ))
}

pub async fn get_claim_layout(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<StoredClaimLayout>, StatusCode> {
    let Some(pool) = state.db.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };

    let row: Option<(chrono::DateTime<Utc>, Option<String>, serde_json::Value)> =
        sqlx::query_as("SELECT created_at, title, document FROM claim_layouts WHERE id = $1")
            .bind(&id)
            .fetch_optional(pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let Some((created_at, title, document_json)) = row else {
        return Err(StatusCode::NOT_FOUND);
    };

    let document: ClaimDocumentV1 =
        serde_json::from_value(document_json).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(StoredClaimLayout {
        id,
        created_at,
        title,
        document,
    }))
}

async fn load_guild_catalog(state: &AppState) -> Result<CachedGuildCatalog, StatusCode> {
    {
        let cache = state.guild_catalog_cache.read().await;
        if let Some(cached) = cache.as_ref() {
            let age = Utc::now()
                .signed_duration_since(cached.fetched_at)
                .num_seconds();
            if age < GUILD_CATALOG_TTL_SECS {
                return Ok(cached.clone());
            }
        }
    }

    let response = state
        .http_client
        .get(state.guild_catalog_url.as_str())
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    if !response.status().is_success() {
        return Err(StatusCode::BAD_GATEWAY);
    }

    let json: serde_json::Value = response.json().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
    let mut entries = parse_catalog_entries(json);
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    let cached = CachedGuildCatalog {
        entries,
        fetched_at: Utc::now(),
    };
    let mut cache = state.guild_catalog_cache.write().await;
    *cache = Some(cached.clone());
    Ok(cached)
}

fn parse_catalog_entries(value: serde_json::Value) -> Vec<CachedGuildCatalogEntry> {
    let mut entries = Vec::new();
    let Some(object) = value.as_object() else {
        return entries;
    };

    for (name, value) in object {
        let Some(entry) = value.as_object() else {
            continue;
        };
        let Some(uuid) = entry.get("uuid").and_then(|value| value.as_str()) else {
            continue;
        };
        let Some(prefix) = entry.get("prefix").and_then(|value| value.as_str()) else {
            continue;
        };
        entries.push(CachedGuildCatalogEntry {
            uuid: uuid.to_string(),
            name: name.to_string(),
            prefix: prefix.to_string(),
        });
    }

    entries
}

fn compare_catalog_entries(a: &GuildCatalogEntry, b: &GuildCatalogEntry, needle: &str) -> Ordering {
    let a_name = a.name.to_ascii_lowercase();
    let b_name = b.name.to_ascii_lowercase();
    let a_prefix = a.prefix.to_ascii_lowercase();
    let b_prefix = b.prefix.to_ascii_lowercase();

    match_rank(&a_prefix, &a_name, needle)
        .cmp(&match_rank(&b_prefix, &b_name, needle))
        .then_with(|| a_name.cmp(&b_name))
        .then_with(|| a.prefix.cmp(&b.prefix))
}

fn match_rank(prefix: &str, name: &str, needle: &str) -> u8 {
    if needle.is_empty() {
        return 0;
    }
    if prefix == needle {
        return 0;
    }
    if name == needle {
        return 1;
    }
    if prefix.starts_with(needle) {
        return 2;
    }
    if name.starts_with(needle) {
        return 3;
    }
    if prefix.contains(needle) {
        return 4;
    }
    if name.contains(needle) {
        return 5;
    }
    6
}

fn next_claim_id(state: &AppState) -> String {
    let suffix = state.next_claim_id.fetch_add(1, AtomicOrdering::Relaxed);
    format!(
        "clm{:x}{suffix:08x}",
        Utc::now().timestamp_millis().unsigned_abs()
    )
}

fn validation_status(error: ClaimValidationError) -> StatusCode {
    match error {
        ClaimValidationError::UnsupportedVersion(_)
        | ClaimValidationError::UnknownTerritory(_)
        | ClaimValidationError::DuplicateMacroId(_)
        | ClaimValidationError::EmptyMacroName(_)
        | ClaimValidationError::DocumentTooLarge(_) => StatusCode::BAD_REQUEST,
    }
}

fn claims_geometry_etag(body: &[u8]) -> String {
    let hash = body
        .iter()
        .fold(CLAIMS_GEOMETRY_ETAG_OFFSET_BASIS, |acc, byte| {
            (acc ^ u64::from(*byte)).wrapping_mul(CLAIMS_GEOMETRY_ETAG_PRIME)
        });
    format!("\"claims-geometry-{hash:016x}\"")
}

#[derive(Debug, Serialize)]
struct StableClaimsBootstrapGeometry {
    territories: BTreeMap<String, ClaimsTerritoryGeometry>,
}

fn serialize_claims_bootstrap_geometry(
    territories: &sequoia_shared::TerritoryMap,
) -> Result<Bytes, serde_json::Error> {
    let geometry = StableClaimsBootstrapGeometry {
        territories: territories
            .iter()
            .map(|(name, territory)| {
                (
                    name.clone(),
                    ClaimsTerritoryGeometry {
                        location: territory.location.clone(),
                        resources: territory.resources.clone(),
                        connections: territory.connections.clone(),
                    },
                )
            })
            .collect(),
    };
    serde_json::to_vec(&geometry).map(Bytes::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sequoia_shared::{GuildRef, Region, Resources, Territory, TerritoryMap};

    #[test]
    fn parse_catalog_entries_reads_name_keyed_payload() {
        let value = serde_json::json!({
            "Alpha Guild": { "uuid": "uuid-a", "prefix": "ALP" },
            "Beta Guild": { "uuid": "uuid-b", "prefix": "BET" }
        });

        let entries = parse_catalog_entries(value);
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .any(|entry| entry.name == "Alpha Guild" && entry.prefix == "ALP")
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.name == "Beta Guild" && entry.uuid == "uuid-b")
        );
    }

    #[test]
    fn compare_catalog_entries_prioritizes_exact_prefix_then_name() {
        let alpha = GuildCatalogEntry {
            uuid: "1".to_string(),
            name: "Alpha Guild".to_string(),
            prefix: "ALP".to_string(),
            color: None,
        };
        let beta = GuildCatalogEntry {
            uuid: "2".to_string(),
            name: "Alpine Beta".to_string(),
            prefix: "BET".to_string(),
            color: None,
        };

        assert_eq!(
            compare_catalog_entries(&alpha, &beta, "alp"),
            Ordering::Less
        );
    }

    #[test]
    fn claims_geometry_etag_changes_with_payload() {
        let first = claims_geometry_etag(br#"{"territories":{"A":{}}}"#);
        let second = claims_geometry_etag(br#"{"territories":{"B":{}}}"#);
        assert_ne!(first, second);
    }

    #[test]
    fn claims_geometry_etag_is_stable_for_known_payload() {
        assert_eq!(
            claims_geometry_etag(br#"{"territories":{"A":{}}}"#),
            "\"claims-geometry-ebe620cff86e5348\""
        );
    }

    #[test]
    fn bootstrap_geometry_serialization_is_stable_across_hash_map_order() {
        let mut first = TerritoryMap::new();
        first.insert("Bravo".to_string(), test_territory([2, 2], [4, 4]));
        first.insert("Alpha".to_string(), test_territory([0, 0], [1, 1]));

        let mut second = TerritoryMap::new();
        second.insert("Alpha".to_string(), test_territory([0, 0], [1, 1]));
        second.insert("Bravo".to_string(), test_territory([2, 2], [4, 4]));

        let first = serialize_claims_bootstrap_geometry(&first).expect("serialize first map");
        let second = serialize_claims_bootstrap_geometry(&second).expect("serialize second map");

        assert_eq!(first, second);
    }

    fn test_territory(start: [i32; 2], end: [i32; 2]) -> Territory {
        Territory {
            guild: GuildRef {
                uuid: "guild".to_string(),
                name: "Guild".to_string(),
                prefix: "GLD".to_string(),
                color: None,
            },
            acquired: Utc::now(),
            location: Region { start, end },
            resources: Resources::default(),
            connections: vec!["Neighbor".to_string()],
            runtime: None,
        }
    }
}
